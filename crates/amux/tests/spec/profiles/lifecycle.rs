//! Cloud attachment is optional; deleting a device is final for every caller.

use amux::ProtocolError;
use amux::installation::{Intent, Observed, OperationId, ProfileEvent, ProfileStatus};
use amux::testnet::{TestNet, Via, WatchProbe};

async fn devices() -> TestNet {
    TestNet::builder()
        .cloud()
        .installation("laptop")
        .persistent()
        .profile("personal")
        .cloud_user("alice")
        .profile("work")
        .cloud_user("bob")
        .cloud_only()
        .daemon("phone")
        .cloud_user("alice")
        .cloud_only()
        .daemon("desk")
        .daemon("colleague")
        .cloud_user("bob")
        .cloud_only()
        .paired("phone", "laptop/personal", Via::Cloud)
        .paired("desk", "laptop/personal", Via::Tcp)
        .paired("colleague", "laptop/work", Via::Cloud)
        .start()
        .await
}

#[cfg(unix)]
#[tokio::test]
async fn logout_keeps_local_agents_artifacts_identity_and_trust() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let agent = a.spawn_echo_agent("notes").await;
    let client = a.socket_client().await;
    let artifact = client
        .put_artifact(
            agent.id.into(),
            amux::ArtifactKind::File,
            "notes.txt",
            "text/plain",
            b"retained".to_vec(),
        )
        .await
        .unwrap();
    let mut local = a.attach(&a, "notes").await;
    let mut direct = net.daemon("desk").attach(&a, "notes").await;
    let cloud = net.daemon("phone").attach(&a, "notes").await;
    let identity = a.identity_on_disk();
    let work_links = b.cloud_link_ids().await;
    laptop.logout("personal").await;
    net.cloud_relay_sees_offline(&a).await;
    cloud.expect_disconnect().await;
    assert_eq!(a.status().intent, Intent::LoggedOut);
    assert_eq!(a.identity_on_disk(), identity);
    assert!(!a.paths().credentials_path().unwrap().exists());
    a.trusts(&net.daemon("phone")).await;
    a.trusts(&net.daemon("desk")).await;
    assert_eq!(
        client
            .get_artifact(agent.id.into(), &artifact.id)
            .await
            .unwrap()
            .1,
        b"retained"
    );
    for session in [&mut local, &mut direct] {
        session.send("after-logout").await;
        session.expect_output("after-logout").await;
    }
    net.daemon("colleague").can_call(&b).await;
    assert_eq!(b.cloud_link_ids().await, work_links);
    println!(
        "Logout removes Alice's credentials and cloud presence and closes her cloud session. The existing local and LAN sessions still echo, stored artifacts and host key survive, both peers remain trusted, and Bob's exact cloud link still serves calls."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pause_closes_only_cloud_sessions_and_repeated_resume_keeps_one_link() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    a.spawn_echo_agent("personal").await;
    b.spawn_echo_agent("work").await;
    let cloud = net.daemon("phone").attach(&a, "personal").await;
    let mut direct = net.daemon("desk").attach(&a, "personal").await;
    let mut work = net.daemon("colleague").attach(&b, "work").await;
    let work_links = b.cloud_link_ids().await;
    let credentials = std::fs::read(a.paths().credentials_path().unwrap()).unwrap();
    laptop.pause("personal").await;
    net.cloud_relay_sees_offline(&a).await;
    cloud.expect_disconnect().await;
    assert_eq!(a.status().intent, Intent::Paused);
    assert_eq!(
        std::fs::read(a.paths().credentials_path().unwrap()).unwrap(),
        credentials
    );
    a.socket_client().await.list_agents().await.unwrap();
    a.trusts(&net.daemon("phone")).await;
    for session in [&mut direct, &mut work] {
        session.send("after-pause").await;
        session.expect_output("after-pause").await;
    }
    let admin = laptop.front_door();
    let (first, second) = tokio::join!(
        admin.resume(OperationId::new(), a.id),
        admin.resume(OperationId::new(), a.id)
    );
    first.unwrap();
    second.unwrap();
    a.reaches_status(Observed::Connected).await;
    let links = a.cloud_link_ids().await;
    assert_eq!(links.len(), 1);
    laptop.resume("personal").await;
    assert_eq!(a.cloud_link_ids().await, links);
    assert_eq!(b.cloud_link_ids().await, work_links);
    net.daemon("phone").can_call(&a).await;
    println!(
        "Pause removes Alice's cloud presence and existing cloud session while retaining credentials, trust, local calls and the open LAN and Bob sessions. Concurrent and repeated resume leave exactly one Alice link; Bob's original link is unchanged."
    );
    drop(admin);
    laptop.pause("personal").await;
    laptop.restart().await;
    assert_eq!(a.status().intent, Intent::Paused);
    assert!(a.cloud_link_ids().await.is_empty());
    a.socket_client().await.list_agents().await.unwrap();
    b.reaches_status(Observed::Connected).await;
    laptop.resume("personal").await;
    a.reaches_status(Observed::Connected).await;
    net.daemon("phone").can_call(&a).await;
    println!(
        "Paused intent survives disk reopen: Alice stays local and Bob reconnects. Explicit resume restores Alice with the existing trust."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn authentication_subscription_and_version_failures_do_not_disconnect_another_profile() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    b.spawn_echo_agent("work").await;
    let mut work = net.daemon("colleague").attach(&b, "work").await;
    let links = b.cloud_link_ids().await;
    for (error, expected) in [
        (
            ProtocolError::InvalidCredentials,
            Observed::AuthenticationRequired,
        ),
        (
            ProtocolError::PaymentRequired,
            Observed::SubscriptionRequired,
        ),
        (
            ProtocolError::UpdateRequired {
                minimum_version: "999.0.0".into(),
                client_version: "0.0.0".into(),
            },
            Observed::UpdateRequired {
                minimum_version: Some("999.0.0".into()),
            },
        ),
    ] {
        laptop.pause("personal").await;
        net.reject_cloud_user("alice", Some(error));
        laptop.resume("personal").await;
        a.reaches_status(expected.clone()).await;
        assert!(a.cloud_link_ids().await.is_empty());
        b.reaches_status(Observed::Connected).await;
        assert_eq!(b.cloud_link_ids().await, links);
        work.send("account-failure-isolated").await;
        work.expect_output("account-failure-isolated").await;
        a.socket_client().await.list_agents().await.unwrap();
        println!(
            "Alice reports {expected:?} after a structured relay refusal, with no cloud link. Bob retains the exact same link and open echo session; Alice's local API still works."
        );
        net.reject_cloud_user("alice", None);
        laptop.login("personal", "alice").await.unwrap();
        a.reaches_status(Observed::Connected).await;
    }
}

async fn watch_until(
    watch: &mut WatchProbe,
    wanted: impl Fn(&ProfileStatus) -> bool,
) -> ProfileStatus {
    loop {
        match watch.next().await {
            ProfileEvent::Upserted { profile, .. } if wanted(&profile) => return *profile,
            ProfileEvent::Upserted { .. } => {}
            event => panic!("unexpected watch event: {event:?}"),
        }
    }
}

#[tokio::test]
async fn a_watcher_observes_lifecycle_and_connector_changes_in_order_including_deletion() {
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let mut watch = laptop.watch().await;
    let snapshot = watch.snapshot().await;
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.iter().all(|p| p.observed == Observed::Connected));
    let admin = laptop.front_door();
    let created = admin
        .create(OperationId::new(), Some("spare".into()))
        .await
        .unwrap();
    watch_until(&mut watch, |p| {
        p.record.id == created.record.id && p.available
    })
    .await;
    admin
        .rename(
            OperationId::new(),
            a.id,
            a.status().record.revision,
            Some("home".into()),
        )
        .await
        .unwrap();
    watch_until(&mut watch, |p| {
        p.record.id == a.id && p.record.label.override_name.as_deref() == Some("home")
    })
    .await;
    laptop.pause("personal").await;
    watch_until(&mut watch, |p| {
        p.record.id == a.id && p.intent == Intent::Paused && p.observed == Observed::Local
    })
    .await;
    laptop.resume("personal").await;
    watch_until(&mut watch, |p| {
        p.record.id == a.id && p.observed == Observed::Connecting
    })
    .await;
    watch_until(&mut watch, |p| {
        p.record.id == a.id && p.observed == Observed::Connected
    })
    .await;
    laptop.logout("personal").await;
    watch_until(&mut watch, |p| {
        p.record.id == a.id && p.intent == Intent::LoggedOut && p.observed == Observed::Local
    })
    .await;
    laptop.login("personal", "alice").await.unwrap();
    watch_until(&mut watch, |p| {
        p.record.id == a.id && p.intent == Intent::Bound && p.observed == Observed::Connected
    })
    .await;
    laptop.delete("personal").await;
    watch_until(&mut watch, |p| p.record.id == a.id && !p.available).await;
    loop {
        match watch.next().await {
            ProfileEvent::Removed { id, .. } => {
                assert_eq!(id, a.id);
                break;
            }
            ProfileEvent::Upserted { .. } => {}
            event => panic!("unexpected {event:?}"),
        }
    }
    assert!(admin.profiles().iter().all(|p| p.record.id != a.id));
    println!(
        "From the initial two-profile snapshot, a watcher receives contiguous sequence numbers across creation, rename, pause, Connecting, Connected, logout, login, unavailability and removal. Deletion remains visible even after the profile disappears from the directory."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn delete_closes_every_transport_and_late_service_work_cannot_recreate_the_device() {
    let checkout = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=amux test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(checkout.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(checkout.path().join("note.txt"), "artifact diff\n").unwrap();
    let net = devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let agent = a.spawn_echo_agent_in("personal", checkout.path()).await;
    b.spawn_echo_agent("work").await;
    let local = a.client();
    let unix = a.socket_client().await;
    let mut local_events = local.subscribe_agents().await.unwrap();
    let mut unix_events = unix.subscribe_agents().await.unwrap();
    let direct = net.daemon("desk").attach(&a, "personal").await;
    let cloud = net.daemon("phone").attach(&a, "personal").await;
    let mut work = net.daemon("colleague").attach(&b, "work").await;
    let work_links = b.cloud_link_ids().await;
    let artifact = unix
        .put_artifact(
            agent.id.into(),
            amux::ArtifactKind::File,
            "notes.txt",
            "text/plain",
            b"retained".to_vec(),
        )
        .await
        .unwrap();
    let retained = a.retain_work().await;
    let directory = a
        .paths()
        .config_path
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let socket = a.paths().socket_path;
    let mut hold = laptop.identity().hold_next_userinfo();
    let refresh = {
        let a = a.clone();
        tokio::spawn(async move { a.refresh_credentials().await })
    };
    hold.entered().await;
    // Poll into Git's first await: the request already holds the agent and
    // artifact owner, so stopping sockets alone cannot protect its writes.
    let mut diff = Box::pin(retained.diff(&agent));
    assert!(futures_util::poll!(diff.as_mut()).is_pending());
    let admin = laptop.front_door();
    let mut delete = Box::pin(admin.delete(OperationId::new(), a.id, a.status().record.revision));
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), delete.as_mut())
            .await
            .is_err(),
        "deletion must drain the already accepted artifact computation"
    );
    let (computed, deleted) = tokio::join!(diff, delete);
    computed.unwrap();
    deleted.unwrap();
    assert!(!directory.exists());
    assert!(!socket.exists());
    direct.expect_disconnect().await;
    cloud.expect_disconnect().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while local_events.recv().await.is_ok() {}
        while unix_events.recv().await.is_ok() {}
        assert!(local.list_agents().await.is_err());
        assert!(unix.list_agents().await.is_err());
    })
    .await
    .expect("in-process and Unix clients must close");
    hold.release();
    assert!(matches!(
        refresh.await.unwrap(),
        Err(amux::AuthError::Unauthenticated)
    ));
    retained
        .assert_late_writes_rejected(&agent, &net.daemon("desk"), &artifact)
        .await;
    assert!(
        !directory.exists(),
        "late operations recreated a deleted profile"
    );
    assert!(matches!(admin.resume(OperationId::new(), a.id).await,
        Err(amux::installation::InstallationError::Deleted(id)) if id == a.id));
    assert!(admin.client(a.id).is_err());
    work.send("delete-isolated").await;
    work.expect_output("delete-isolated").await;
    assert_eq!(b.cloud_link_ids().await, work_links);
    b.socket_client().await.list_agents().await.unwrap();
    println!(
        "Delete drains an artifact computation that already resolved its owner, closes accepted Unix and in-process subscriptions plus LAN and cloud sessions, and removes the device directory and socket. A held refresh and retained pairing, artifact put/get/diff, pinned input and replay requests all fail after deletion without recreating files. Bob's original link and open session continue working."
    );
}

#[tokio::test]
async fn blocked_agent_input_leaves_other_input_diff_and_profile_lifecycle_available() {
    use std::time::Duration;

    use tokio::time::timeout;

    let checkout = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=amux test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    std::fs::write(checkout.path().join("note.txt"), "concurrent diff\n").unwrap();
    for action in ["pause", "logout", "delete", "shutdown"] {
        let net = TestNet::builder()
            .cloud()
            .installation("laptop")
            .profile("personal")
            .cloud_user("alice")
            .start()
            .await;
        let laptop = net.installation("laptop");
        let profile = laptop.profile("personal");
        let blocked_agent = profile
            .spawn_echo_agent_in("blocked", checkout.path())
            .await;
        let responsive_agent = profile
            .spawn_echo_agent_in("responsive", checkout.path())
            .await;
        let mut responsive = profile.attach(&profile, "responsive").await;
        let mut blocked_session = profile.attach(&profile, "blocked").await;
        let held_queue = profile.hold_echo_input(&blocked_agent).await;
        let retained = profile.retain_work().await;
        let mut blocked = Box::pin(retained.send_echo_input(&blocked_agent, b"held input"));
        assert!(futures_util::poll!(blocked.as_mut()).is_pending());

        // Keep an accepted Diff pending too: service reads must share the gate.
        let mut diff = Box::pin(retained.diff(&responsive_agent));
        assert!(futures_util::poll!(diff.as_mut()).is_pending());
        timeout(Duration::from_secs(5), async {
            responsive.send("independent input").await;
            responsive.expect_output("independent input").await;
            diff.await.unwrap();
        })
        .await
        .expect("input and Diff must progress past a blocked PTY write");
        assert!(futures_util::poll!(blocked.as_mut()).is_pending());

        timeout(Duration::from_secs(5), async {
            match action {
                "pause" => {
                    laptop.pause("personal").await;
                }
                "logout" => {
                    laptop.logout("personal").await;
                }
                "delete" => laptop.delete("personal").await,
                "shutdown" => laptop.stop().await,
                _ => unreachable!(),
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{action} waited for a blocked PTY write"));
        assert!(futures_util::poll!(blocked.as_mut()).is_pending());
        if matches!(action, "pause" | "logout") {
            drop(held_queue);
            timeout(Duration::from_secs(5), blocked)
                .await
                .unwrap()
                .unwrap();
            blocked_session.expect_output("held input").await;
        } else {
            assert!(matches!(
                retained.send_echo_input(&responsive_agent, b"late").await,
                Err(ProtocolError::FailedPrecondition { .. })
            ));
        }
        println!(
            "With one agent's production PTY input queue blocked, another agent echoes, an accepted Diff completes, and {action} finishes without waiting for that input. Closed profiles reject retained input calls."
        );
    }
}

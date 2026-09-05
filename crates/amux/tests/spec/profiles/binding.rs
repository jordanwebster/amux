//! Login commits an account to a complete device; refusals cannot move it.

use amux::AuthError;
use amux::installation::{
    BindError, BindRequest, BindTarget, Intent, NonPristine, Observed, OperationId,
};
use amux::test_fixtures::Fault;
use amux::testnet::{InstallationHandle, TestNet, Via};

fn request(installation: &InstallationHandle, user: &str, target: BindTarget) -> BindRequest {
    BindRequest {
        target,
        cloud_url: installation.identity().url(),
        staged_refresh_token: installation.identity().refresh_token_for(user),
        adopt_non_pristine: false,
    }
}

async fn bound_devices() -> TestNet {
    TestNet::builder()
        .cloud()
        .installation("laptop")
        .persistent()
        .profile("personal")
        .cloud_user("alice")
        .cloud_only()
        .profile("work")
        .cloud_user("bob")
        .cloud_only()
        .profile("spare")
        .daemon("phone")
        .cloud_user("alice")
        .cloud_only()
        .paired("phone", "laptop/personal", Via::Cloud)
        .start()
        .await
}

#[tokio::test]
async fn refused_rebinding_duplicate_account_and_invalid_login_preserve_the_live_device() {
    let net = bound_devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let phone = net.daemon("phone");
    a.spawn_echo_agent("still-running").await;
    let mut session = phone.attach(&a, "still-running").await;
    let identity = a.identity_on_disk();
    let record = a.status().record;
    let path = a.paths().credentials_path().unwrap();
    let credentials = std::fs::read(&path).unwrap();
    let links = a.cloud_link_ids().await;
    assert_eq!(links.len(), 1);
    assert!(matches!(laptop.login("personal", "bob").await,
        Err(BindError::ProfileBoundToOtherAccount { profile }) if profile == a.id));
    assert!(matches!(laptop.login("spare", "alice").await,
        Err(BindError::AccountAlreadyBound { profile }) if profile == a.id));
    laptop
        .identity()
        .inject(Fault::RejectRefresh("login rejected".into()));
    assert!(matches!(
        laptop.login("personal", "alice").await,
        Err(BindError::TokenRejected(_))
    ));
    laptop.identity().inject(Fault::MissingSubject);
    assert!(matches!(
        laptop.login("personal", "alice").await,
        Err(BindError::MissingSubject)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), credentials);
    assert_eq!(a.status().record, record);
    assert_eq!(a.identity_on_disk(), identity);
    assert_eq!(a.cloud_link_ids().await, links);
    assert!(laptop.profile("spare").status().record.binding.is_none());
    assert_eq!(laptop.front_door().profiles().len(), 3);
    session.send("connection-survived").await;
    session.expect_output("connection-survived").await;
    a.trusts(&phone).await;
    b.reaches_status(Observed::Connected).await;
    // A successful subsequent refresh proves the accepted token is still usable.
    a.refresh_credentials().await.unwrap();
    println!(
        "Wrong account, duplicate account, rejected token and missing subject are refused. Credential bytes, profile record, host key and exact live link stay unchanged; the already-open cloud session still echoes and the accepted credential refreshes."
    );
}

#[tokio::test]
async fn simultaneous_logins_select_one_profile_and_leave_one_cloud_link() {
    let net = TestNet::builder()
        .cloud()
        .installation("laptop")
        .profile("personal")
        .cloud_only()
        .profile("work")
        .cloud_user("bob")
        .cloud_only()
        .installation("phone")
        .profile("personal")
        .cloud_user("alice")
        .cloud_only()
        .start()
        .await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let admin = laptop.front_door();
    let first = request(&laptop, "alice", BindTarget::ByAccount);
    let second = request(&laptop, "alice", BindTarget::ByAccount);
    let mut hold = laptop.identity().hold_next_userinfo();
    let worker = {
        let admin = admin.clone();
        tokio::spawn(async move { admin.bind(OperationId::new(), first).await })
    };
    hold.entered().await;
    let other = {
        let admin = admin.clone();
        tokio::spawn(async move { admin.bind(OperationId::new(), second).await })
    };
    hold.release();
    assert_eq!(worker.await.unwrap().unwrap().record.id, a.id);
    assert_eq!(other.await.unwrap().unwrap().record.id, a.id);
    assert_eq!(admin.profiles().len(), 2);
    assert_eq!(
        admin
            .profiles()
            .iter()
            .filter(|p| p
                .record
                .binding
                .as_ref()
                .is_some_and(|b| b.account.subject == "alice"))
            .count(),
        1
    );
    a.reaches_status(Observed::Connected).await;
    assert_eq!(a.cloud_link_ids().await.len(), 1);
    let phone = net.installation("phone").profile("personal");
    let pin = a.start_pairing().await;
    phone.pair(&a).with_cloud_pin(&pin).await.unwrap();
    phone.can_call(&a).await;
    println!(
        "Overlapping Alice logins both return the same formerly unbound profile. The directory has one Alice device and one Bob device; Alice has exactly one live cloud link and can pair and serve a remote call."
    );
}

#[tokio::test]
async fn a_swapped_subject_on_refresh_cannot_reconnect_a_profile_into_another_account() {
    let net = bound_devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let record = a.status().record;
    let identity = a.identity_on_disk();
    let credentials = std::fs::read(a.paths().credentials_path().unwrap()).unwrap();
    laptop.identity().inject(Fault::SwapSubject {
        from: "alice".into(),
        to: "bob".into(),
    });
    assert!(matches!(
        a.refresh_credentials().await,
        Err(AuthError::AccountMismatch)
    ));
    assert_eq!(
        std::fs::read(a.paths().credentials_path().unwrap()).unwrap(),
        credentials
    );
    laptop.pause("personal").await;
    net.cloud_relay_sees_offline(&a).await;
    laptop.resume("personal").await;
    a.reaches_status(Observed::AuthenticationRequired).await;
    assert!(a.cloud_link_ids().await.is_empty());
    assert_eq!(a.status().record.binding, record.binding);
    assert_eq!(a.identity_on_disk(), identity);
    b.reaches_status(Observed::Connected).await;
    assert_eq!(b.cloud_link_ids().await.len(), 1);
    a.cloud_isolated_from(&b).await;
    laptop.login("personal", "alice").await.unwrap();
    net.daemon("phone").can_call(&a).await;
    println!(
        "A refresh returning Bob's subject fails with AccountMismatch and leaves Alice's credential and identity unchanged. Reconnect reports AuthenticationRequired with no cloud link; Bob remains connected. A valid Alice login restores the paired device."
    );
}

#[tokio::test]
async fn logout_wins_over_a_refresh_whose_rotated_token_is_waiting_for_userinfo() {
    let net = bound_devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let record = a.status().record;
    let mut hold = laptop.identity().hold_next_userinfo();
    let refresh = {
        let a = a.clone();
        tokio::spawn(async move { a.refresh_credentials().await })
    };
    hold.entered().await;
    laptop.logout("personal").await;
    net.cloud_relay_sees_offline(&a).await;
    hold.release();
    assert!(matches!(
        refresh.await.unwrap(),
        Err(AuthError::Unauthenticated)
    ));
    assert!(!a.paths().credentials_path().unwrap().exists());
    assert_eq!(a.status().intent, Intent::LoggedOut);
    assert_eq!(a.status().record.binding, record.binding);
    laptop.resume("personal").await;
    assert!(a.cloud_link_ids().await.is_empty());
    a.socket_client().await.list_agents().await.unwrap();
    laptop.restart().await;
    assert_eq!(a.status().intent, Intent::LoggedOut);
    assert!(a.cloud_link_ids().await.is_empty());
    assert!(!a.paths().credentials_path().unwrap().exists());
    println!(
        "Logout commits while refreshed userinfo is held. The released refresh returns Unauthenticated, cannot recreate credentials, and cannot reconnect on resume or installation restart; local calls remain available."
    );
}

#[tokio::test]
async fn logout_reserves_the_account_after_restart_and_relogin_preserves_host_and_trust() {
    let net = bound_devices().await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let phone = net.daemon("phone");
    let identity = a.identity_on_disk();
    let binding = a.status().record.binding;
    laptop.logout("personal").await;
    laptop.restart().await;
    assert_eq!(a.status().intent, Intent::LoggedOut);
    assert_eq!(a.status().record.binding, binding);
    assert!(matches!(laptop.login("spare", "alice").await,
        Err(BindError::AccountAlreadyBound { profile }) if profile == a.id));
    let result = laptop
        .front_door()
        .bind(
            OperationId::new(),
            request(&laptop, "alice", BindTarget::ByAccount),
        )
        .await
        .unwrap();
    assert_eq!(result.record.id, a.id);
    assert_eq!(result.host_id, identity.0);
    assert_eq!(a.identity_on_disk(), identity);
    assert_eq!(a.status().record.binding, binding);
    a.trusts(&phone).await;
    phone.trusts(&a).await;
    phone.can_call(&a).await;
    assert_eq!(a.cloud_link_ids().await.len(), 1);
    println!(
        "Logout reserves Alice's profile across disk reopen and refuses a second profile. Login by account returns the original UUID, host id and key; the phone calls it using existing mutual trust, without pairing again."
    );
}

#[tokio::test]
async fn pairing_or_agent_creation_during_login_requires_explicit_adoption_confirmation() {
    for pair in [true, false] {
        let net = TestNet::builder()
            .cloud()
            .installation("laptop")
            .profile("personal")
            .profile("work")
            .cloud_user("bob")
            .installation("phone")
            .profile("personal")
            .cloud_user("alice")
            .start()
            .await;
        let laptop = net.installation("laptop");
        let a = laptop.profile("personal");
        let phone = net.installation("phone").profile("personal");
        let admin = laptop.front_door();
        let login = request(&laptop, "alice", BindTarget::Explicit(a.id));
        let mut hold = laptop.identity().hold_next_userinfo();
        let worker = {
            let admin = admin.clone();
            let login = login.clone();
            tokio::spawn(async move { admin.bind(OperationId::new(), login).await })
        };
        hold.entered().await;
        let expected = if pair {
            let pin = a.start_pairing().await;
            phone.pair(&a).with_pin(&pin).await.unwrap();
            a.trusts(&phone).await;
            NonPristine::TrustEntries(1)
        } else {
            a.spawn_echo_agent("created-during-login").await;
            NonPristine::LocalAgents(1)
        };
        hold.release();
        assert!(matches!(worker.await.unwrap(),
            Err(BindError::AdoptionNeedsConfirmation { profile, reason })
                if profile == a.id && reason == expected));
        assert!(a.status().record.binding.is_none());
        assert!(a.cloud_link_ids().await.is_empty());
        assert!(!a.paths().credentials_path().unwrap().exists());
        let result = admin
            .bind(
                OperationId::new(),
                BindRequest {
                    adopt_non_pristine: true,
                    ..login
                },
            )
            .await
            .unwrap();
        assert_eq!(result.record.id, a.id);
        a.reaches_status(Observed::Connected).await;
        if pair {
            a.trusts(&phone).await;
            phone.can_call(&a).await;
        } else {
            let mut session = a.attach(&a, "created-during-login").await;
            session.send("adopted-agent-survives").await;
            session.expect_output("adopted-agent-survives").await;
        }
        println!(
            "Login held at userinfo observes newly committed {expected:?} and asks for adoption confirmation. Retrying the same staged login with confirmation keeps the device and its local state and starts one cloud link."
        );
        assert_eq!(a.cloud_link_ids().await.len(), 1);
    }
}

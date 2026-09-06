//! Account boundaries preserve device identity, trust and independent service.

#[cfg(unix)]
use amux::installation::{InstallationError, Observed, ProfileEvent};
use amux::testnet::{TestNet, Via};

/// Binding two accounts starts two full devices; each device pairs independently.
#[tokio::test]
async fn a_key_pinned_only_by_one_profile_cannot_authenticate_into_another() {
    let net = TestNet::builder()
        .cloud()
        .installation("laptop")
        .profile("personal")
        .cloud_user("alice")
        .profile("work")
        .cloud_user("bob")
        .daemon("phone")
        .cloud_user("alice")
        .paired("phone", "laptop/personal", Via::Cloud)
        .start()
        .await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let phone = net.daemon("phone");
    assert_ne!(a.host_id(), b.host_id());
    assert_ne!(a.identity_on_disk().1, b.identity_on_disk().1);
    a.trusts(&phone).await;
    b.does_not_trust(&phone).await;
    phone.can_call(&a).await;
    phone.cannot_authenticate_to(&b).await;
    b.client().list_agents().await.unwrap();
    println!(
        "Personal profile accepts its paired phone; work rejects the same key at device mTLS and still serves local calls."
    );
}

/// Pairing state belongs to each complete device, including the attempt budget.
#[tokio::test]
async fn concurrent_pairing_windows_share_no_secrets_limits_or_commits() {
    let net = TestNet::builder()
        .cloud()
        .installation("laptop")
        .profile("personal")
        .cloud_user("alice")
        .profile("work")
        .cloud_user("bob")
        .daemon("phone")
        .cloud_user("bob")
        .daemon("visitor")
        .no_cloud()
        .start()
        .await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let phone = net.daemon("phone");
    let (qa, qb) = tokio::join!(a.start_qr_pairing(), b.start_qr_pairing());
    assert_ne!(qa.secret, qb.secret);
    let wrong_qr = amux::testnet::QrPayload {
        host_id: b.host_id(),
        secret: qa.secret.clone(),
    };
    phone
        .pair(&b)
        .with_qr(&wrong_qr)
        .await
        .expect_err("personal QR secret must not pair work");
    a.cancel_pairing().await;
    b.pair_mode_active().await;
    b.cancel_pairing().await;
    let (pa, pb) = tokio::join!(a.start_pairing(), b.start_pairing());
    for _ in 0..5 {
        phone
            .pair(&a)
            .with_pin(&pa.wrong_guess())
            .await
            .expect_err("wrong PIN");
    }
    a.pair_mode_ends().await;
    b.pair_mode_active().await;
    phone.pair(&b).with_pin(&pb).await.unwrap();
    b.trusts(&phone).await;
    a.does_not_trust(&phone).await;
    phone.does_not_trust(&a).await;
    phone.can_call(&b).await;
    let visitor = net.daemon("visitor");
    let (pa, pb) = tokio::join!(a.start_pairing(), b.start_pairing());
    let (personal, work) =
        tokio::join!(phone.pair(&a).with_pin(&pa), visitor.pair(&b).with_pin(&pb));
    personal.unwrap();
    work.unwrap();
    a.trusts(&phone).await;
    b.trusts(&visitor).await;
    a.does_not_trust(&visitor).await;
    visitor.does_not_trust(&a).await;
    println!(
        "Two simultaneous pairing windows have distinct QR secrets; exhausting personal's five attempts leaves work pairable and commits trust only to work."
    );
}

/// A second supervisor cannot acquire a live installation's root.
#[cfg(unix)]
#[tokio::test]
async fn two_supervisors_cannot_share_a_root_or_steal_its_serving_socket() {
    let net = TestNet::builder()
        .installation("laptop")
        .persistent()
        .profile("personal")
        .profile("work")
        .start()
        .await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let client = a.socket_client().await;
    assert!(matches!(
        laptop.try_second_supervisor().await,
        Err(InstallationError::RootBusy(_))
    ));
    client.list_agents().await.unwrap();
    b.socket_client().await.list_agents().await.unwrap();
    assert_eq!(laptop.front_door().profiles().len(), 2);
    println!("Second supervisor receives RootBusy; both original profile sockets remain callable.");
}

/// Startup failures are directory entries, not installation-wide outages.
#[cfg(unix)]
#[tokio::test]
async fn one_profile_failing_to_start_leaves_the_directory_and_other_profiles_serving() {
    let net = TestNet::builder()
        .installation("laptop")
        .persistent()
        .profile("personal")
        .profile("work")
        .start()
        .await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    laptop.stop().await;
    let socket = a.paths().socket_path;
    let occupied = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    laptop.restart().await;
    let mut watch = laptop.watch().await;
    let snapshot = watch.snapshot().await;
    assert_eq!(snapshot.len(), 2);
    assert_eq!(a.status().observed, Observed::StartupFailed);
    assert!(!a.status().available);
    assert!(a.status().startup_error.is_some());
    assert!(b.status().available);
    b.socket_client().await.list_agents().await.unwrap();
    // A connection must still arrive at the listener that occupied the path.
    let _probe = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    occupied.set_nonblocking(true).unwrap();
    occupied
        .accept()
        .expect("startup did not steal the live listener");
    let created = laptop
        .front_door()
        .create(amux::installation::OperationId::new(), Some("extra".into()))
        .await
        .unwrap();
    assert!(created.available);
    assert!(matches!(watch.next().await, ProfileEvent::Upserted { .. }));
    println!(
        "Personal reports StartupFailed without stealing the occupied listener; work serves over its socket and the directory can create another profile."
    );
}

/// Sharing a relay creates no cross-account path; explicit LAN pairing still
/// confers ordinary device authority across accounts.
#[tokio::test]
async fn separate_tenants_exchange_no_presence_claims_routes_frames_or_candidates_but_lan_pairing_grants_authority()
 {
    let net = TestNet::builder()
        .cloud()
        .installation("laptop")
        .profile("personal")
        .cloud_user("alice")
        .profile("work")
        .cloud_user("bob")
        .installation("phone")
        .profile("personal")
        .cloud_user("alice")
        .profile("work")
        .cloud_user("bob")
        .paired("phone/personal", "laptop/personal", Via::Cloud)
        .paired("phone/work", "laptop/work", Via::Cloud)
        .start()
        .await;
    let laptop = net.installation("laptop");
    let phone = net.installation("phone");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let pa = phone.profile("personal");
    let pb = phone.profile("work");
    a.can_call(&pa).await;
    b.can_call(&pb).await;
    for (from, to) in [
        (&a, &b),
        (&a, &pb),
        (&b, &a),
        (&b, &pa),
        (&pa, &b),
        (&pb, &a),
    ] {
        from.cloud_isolated_from(to).await;
        from.does_not_trust(to).await;
    }
    a.cloud_cannot_forward_to(&b, &pa).await;
    b.cloud_cannot_forward_to(&a, &pb).await;
    let pin = b.start_pairing().await;
    a.pair(&b).with_pin(&pin).await.unwrap();
    a.can_call(&b).await;
    b.can_call(&a).await;
    a.connects_to(&b).via_direct().await;
    println!(
        "Alice and Bob each have callable paired devices, with no cross-tenant inventory, candidates, claims or routes. Forced relay frames reach same-tenant controls only. Explicit LAN pairing then permits calls across accounts."
    );
}

/// Fixture lifecycle verbs drive production credentials and reopen the same
/// persistent device without changing the other account's identity or trust.
#[tokio::test]
async fn installation_handles_keep_profile_identity_through_login_pause_and_reopen() {
    let net = TestNet::builder()
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
        .paired("phone", "laptop/personal", Via::Cloud)
        .start()
        .await;
    let laptop = net.installation("laptop");
    let a = laptop.profile("personal");
    let b = laptop.profile("work");
    let phone = net.daemon("phone");
    let identity = a.identity_on_disk();
    let other_identity = b.identity_on_disk();
    let old_client = a.client();
    laptop.logout("personal").await;
    net.cloud_relay_sees_offline(&a).await;
    old_client.list_agents().await.unwrap();
    laptop.login("personal", "alice").await.unwrap();
    phone.can_call(&a).await;
    laptop.pause("personal").await;
    net.cloud_relay_sees_offline(&a).await;
    laptop.resume("personal").await;
    phone.can_call(&a).await;
    laptop.restart().await;
    assert!(old_client.list_agents().await.is_err());
    phone.can_call(&a).await;
    a.trusts(&phone).await;
    assert_eq!(a.identity_on_disk(), identity);
    assert_eq!(b.identity_on_disk(), other_identity);
    laptop.delete("personal").await;
    assert_eq!(laptop.front_door().profiles().len(), 1);
    b.client().list_agents().await.unwrap();
    println!(
        "Production login, logout, pause, resume and disk reopen preserve the paired profile identity; deletion removes only that profile."
    );
}

use futures_util::FutureExt;

use super::*;

#[tokio::test(start_paused = true)]
async fn profile_fixture_waits_for_its_previous_listener_to_release() {
    let fixtures = Arc::new(Mutex::new(FixturePlan::default()));
    let factory = fixture_factory(fixtures.clone(), None);
    let id = ProfileId::new();
    let listener = factory(id).await.listener.unwrap();
    let addr = listener.local_addr().unwrap();

    // Model the old address remaining occupied after shutdown acknowledges.
    // Rebinding must yield so teardown can finish on this same runtime.
    let mut restart = factory(id);
    assert!(futures_util::poll!(&mut restart).is_pending());
    assert!(
        fixtures.try_lock().is_ok(),
        "rebind must not hold the fixture lock"
    );
    drop(listener);

    let restarted = restart.await.listener.unwrap();
    assert_eq!(restarted.local_addr().unwrap(), addr);
    let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let _server = restarted.accept().await.unwrap();
    assert_eq!(
        std::net::TcpListener::bind(addr).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse,
        "a live fixture must still own its address exclusively"
    );
}

#[tokio::test(start_paused = true)]
async fn profile_fixture_refuses_an_address_that_stays_occupied() {
    let factory = fixture_factory(Arc::new(Mutex::new(FixturePlan::default())), None);
    let id = ProfileId::new();
    let _listener = factory(id).await.listener.unwrap();
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        std::panic::AssertUnwindSafe(factory(id)).catch_unwind(),
    )
    .await
    .expect("occupied profile address must exhaust its rebind deadline");
    assert!(
        result.is_err(),
        "restart must not silently choose another port"
    );
    assert!(started.elapsed() >= std::time::Duration::from_secs(5));
}

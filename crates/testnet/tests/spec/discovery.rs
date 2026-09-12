use node::discovery::DiscoveryEvent;
use testnet::{TestNet, Via};

#[tokio::test]
async fn a_listening_profile_advertises_while_up_and_withdraws_on_stop() {
    let net = TestNet::builder().daemon("desktop").start().await;
    let desktop = net.daemon("desktop");

    let events = net.discovery_events();
    let advert = events
        .iter()
        .find_map(|event| match event {
            DiscoveryEvent::Found(advert) if advert.host_id == desktop.host_id() => Some(advert),
            _ => None,
        })
        .expect("the listening profile advertises after binding");
    assert_eq!(advert.name, "desktop");
    assert_eq!(advert.version, node::PROTOCOL_VERSION);
    assert_eq!(advert.addrs.len(), 1);
    assert_ne!(advert.addrs[0].port(), 0);

    desktop.stop().await;

    assert_eq!(
        net.discovery_events(),
        vec![DiscoveryEvent::Lost {
            host_id: desktop.host_id(),
        }]
    );
}

#[tokio::test]
async fn a_profile_with_the_listener_off_advertises_nothing() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .start()
        .await;

    assert!(net.discovery_events().is_empty());
}

#[tokio::test]
async fn a_paired_host_is_dialed_the_moment_it_is_found() {
    let net = TestNet::builder()
        .daemon("laptop")
        .daemon("desktop")
        .trusted("laptop", "desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.connects_to(&desktop).via_direct().await;
    laptop.can_call(&desktop).await;
}

#[tokio::test]
async fn a_found_address_wins_over_a_stale_stored_one() {
    let net = TestNet::builder()
        .cloud()
        .daemon("laptop")
        .cloud_only()
        .daemon("desktop")
        .paired_with_stale_direct("laptop", "desktop")
        .start()
        .await;
    let [laptop, desktop] = net.daemons(["laptop", "desktop"]);

    laptop.connects_to(&desktop).via_direct().await;
    assert_eq!(
        laptop.stored_direct_addrs_to(desktop.host_id()).await,
        vec![desktop.direct_addr()]
    );
}

#[tokio::test]
async fn an_embedded_runtime_dials_on_resume_and_drops_direct_links_on_suspend() {
    let net = TestNet::builder()
        .daemon("desktop")
        .installation("phone")
        .embedded()
        .profile("main")
        .cloud_only()
        .paired("phone/main", "desktop", Via::Direct)
        .start()
        .await;
    let phone_installation = net.installation("phone");
    let phone = phone_installation.profile("main");
    let desktop = net.daemon("desktop");

    phone.connects_to(&desktop).via_direct().await;

    phone_installation.front_door().host_suspend().await;
    phone.cannot_see(&desktop).await;
    desktop.cannot_see(&phone).await;

    phone_installation.front_door().host_resume().await;
    phone.connects_to(&desktop).via_direct().await;
    phone.can_call(&desktop).await;
}

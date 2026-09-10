use amux::discovery::DiscoveryEvent;
use amux::testnet::TestNet;

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
    assert_eq!(advert.version, amux::PROTOCOL_VERSION);
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

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

fn advert(name: &str) -> Advertisement {
    Advertisement {
        host_id: HostId::new_v4(),
        name: name.to_string(),
        version: 7,
        addrs: vec![SocketAddr::from((Ipv4Addr::new(192, 0, 2, 8), 4819))],
    }
}

#[test]
#[cfg(not(target_os = "ios"))]
fn txt_encoding_contains_only_version_and_host_id() {
    let advert = advert("Studio");
    let service = mdns::service_info_for_test(&advert).unwrap();
    let properties = service.get_properties();

    assert_eq!(properties.len(), 2);
    assert_eq!(service.get_property_val_str("v"), Some("7"));
    assert_eq!(
        service.get_property_val_str("hid"),
        Some(advert.host_id.simple().to_string().as_str())
    );
    assert_eq!(service.get_port(), 4819);
    assert!(service.get_fullname().ends_with(SERVICE_TYPE));
}

#[test]
#[cfg(not(target_os = "ios"))]
fn txt_decoding_builds_an_advertisement() {
    let expected = advert("Studio.local");
    let resolved = mdns::service_info_for_test(&expected)
        .unwrap()
        .as_resolved_service();

    let decoded = mdns::advertisement_from_service_for_test(&resolved).unwrap();

    assert_eq!(decoded, expected);
}

#[tokio::test]
async fn scripted_announce_reaches_every_browser() {
    let discovery = ScriptedDiscovery::new();
    let peer = discovery.clone();
    let mut first = discovery.browse();
    let mut second = peer.browse();
    let advert = advert("Studio");

    discovery.announce(advert.clone());

    assert_eq!(
        first.recv().await.unwrap(),
        DiscoveryEvent::Found(advert.clone())
    );
    assert_eq!(second.recv().await.unwrap(), DiscoveryEvent::Found(advert));
}

#[tokio::test]
async fn scripted_withdraw_reaches_every_browser() {
    let discovery = ScriptedDiscovery::new();
    let mut first = discovery.browse();
    let mut second = discovery.browse();
    let host_id = HostId::new_v4();

    discovery.withdraw_host(host_id);

    assert_eq!(
        first.recv().await.unwrap(),
        DiscoveryEvent::Lost { host_id }
    );
    assert_eq!(
        second.recv().await.unwrap(),
        DiscoveryEvent::Lost { host_id }
    );
}

#[test]
fn found_hosts_returns_the_latest_addresses() {
    let hosts = FoundHosts::default();
    let host = HostId::new_v4();
    let first = SocketAddr::from((Ipv4Addr::LOCALHOST, 1001));
    let second = SocketAddr::from((Ipv4Addr::LOCALHOST, 1002));

    hosts.found(Advertisement {
        host_id: host,
        name: "Studio".to_string(),
        version: 1,
        addrs: vec![first],
    });
    hosts.found(Advertisement {
        host_id: host,
        name: "Studio".to_string(),
        version: 1,
        addrs: vec![second],
    });

    assert_eq!(hosts.addrs_for(host), vec![second]);
    assert!(hosts.addrs_for(HostId::new_v4()).is_empty());
    hosts.lost(host);
    assert!(hosts.addrs_for(host).is_empty());
}

struct MockClock {
    now: Mutex<Instant>,
}

impl MockClock {
    fn advance(&self, duration: Duration) {
        let mut now = self.now.lock().unwrap();
        *now += duration;
    }
}

impl MonotonicClock for MockClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
}

#[test]
fn wake_gap_requeries_after_a_monotonic_pause() {
    let clock = Arc::new(MockClock {
        now: Mutex::new(Instant::now()),
    });
    let monitor = WakeMonitor::new(clock.clone());
    let requeries = AtomicUsize::new(0);

    clock.advance(WAKE_GAP);
    monitor.tick(|| {
        requeries.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(requeries.load(Ordering::Relaxed), 0);

    clock.advance(WAKE_GAP + Duration::from_millis(1));
    monitor.tick(|| {
        requeries.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(requeries.load(Ordering::Relaxed), 1);
}

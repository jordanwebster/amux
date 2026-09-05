//! Device identities and revocation as observed through the client API.

use std::time::Duration;

use amux::testnet::{TestNet, Via};
use amux::{Client, SendInputRequest, SubscribeSessionEvent, SubscribeSessionRequest};
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(5);

async fn send(client: &Client, agent: Uuid, text: &str) -> Result<(), amux::ClientError> {
    client
        .send_input(SendInputRequest {
            agent: agent.into(),
            input_id: Uuid::new_v4().as_bytes().to_vec(),
            io_protocol: "test_echo_v1".to_string(),
            payload: text.to_owned().into(),
            pin: Vec::new(),
        })
        .await
}

/// Local identity inspection never opens pair-mode and returns the same
/// fingerprint that another device sees in its persisted peer list.
#[tokio::test]
async fn revocation_device_identity_and_peer_fingerprints_survive_restart() {
    let net = TestNet::builder()
        .cloud()
        .daemon("phone")
        .cloud_only()
        .daemon("host")
        .cloud_only()
        .paired("phone", "host", Via::Cloud)
        .start()
        .await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let client = phone.admin_client().await;
    let identity = client.device_identity().await.unwrap();
    assert_eq!(identity.host_id, phone.host_id());
    assert_eq!(identity.name, "phone");
    let hash = ring::digest::digest(&ring::digest::SHA256, &phone.identity_on_disk().1);
    let fingerprint = hash
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(identity.fingerprint, fingerprint);
    assert!(!client.pairing_is_active().await.unwrap());
    let peers = host.admin_client().await.list_peers().await.unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].host_id, identity.host_id);
    assert_eq!(peers[0].name, identity.name);
    assert_eq!(peers[0].fingerprint, identity.fingerprint);
    let peer = host
        .admin_client()
        .await
        .get_peer(phone.host_id())
        .await
        .unwrap();
    assert_eq!(peer, peers[0]);
    phone.restart().await;
    assert_eq!(
        phone.admin_client().await.device_identity().await.unwrap(),
        identity
    );
    phone.can_call(&host).await;
    host.rejects_remote_trust_admin_from(&phone).await;
    println!(
        "revocation identity: {}",
        serde_json::json!({
            "host_id": identity.host_id, "name": identity.name, "fingerprint": identity.fingerprint,
            "paired_device_fingerprint": peer.fingerprint, "stable_after_restart": true,
            "pair_mode_active": false, "remote_identity_admin_denied": true,
        })
    );
}

#[tokio::test]
async fn revocation_closes_the_revoked_phones_live_session_over_the_relay() {
    revoked_session(Via::Cloud).await;
}

#[tokio::test]
async fn revocation_closes_the_revoked_phones_live_session_over_direct_with_relay_fallback() {
    revoked_session(Via::Tcp).await;
}

async fn revoked_session(via: Via) {
    let builder = TestNet::builder().cloud().daemon("phone");
    let builder = if matches!(via, Via::Cloud) {
        builder.cloud_only()
    } else {
        builder
    };
    let builder = builder.daemon("host");
    let builder = if matches!(via, Via::Cloud) {
        builder.cloud_only()
    } else {
        builder
    };
    let net = builder.paired("phone", "host", via).start().await;
    let [phone, host] = net.daemons(["phone", "host"]);
    let agent = host.spawn_echo_agent("worker").await;
    phone.sees_agent_on(&host, "worker").await;
    if matches!(via, Via::Cloud) {
        phone.connects_to(&host).via_cloud().await;
    } else {
        phone.connects_to(&host).via_direct().await;
    }
    let client = phone.admin_client().await;
    let request = SubscribeSessionRequest {
        agent: agent.id.into(),
        io_protocol: "test_echo_v1".to_string(),
        args: None,
    };
    let mut stream = client.subscribe_session(request).await.unwrap();
    send(&client, agent.id, "before-revocation").await.unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            match stream.recv().await.unwrap() {
                SubscribeSessionEvent::Output { payload } => {
                    assert_eq!(payload.as_slice(), b"before-revocation");
                    break;
                }
                SubscribeSessionEvent::Closed { reason } => {
                    panic!("session closed early: {reason:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("phone must receive a live echo before revocation");

    let admin = host.admin_client().await;
    let removed = admin
        .unpair(phone.host_id(), "remove this device")
        .await
        .unwrap();
    assert_eq!(removed.host_id, phone.host_id());
    assert_eq!(
        removed.fingerprint,
        client.device_identity().await.unwrap().fingerprint
    );
    assert!(admin.list_peers().await.unwrap().is_empty());

    // Do not retry a successful call: the very first send after Unpair's
    // acknowledgement must fail, even if the caller still caches its channel.
    let error = tokio::time::timeout(DEADLINE, send(&client, agent.id, "after-revocation"))
        .await
        .expect("revoked send must return promptly")
        .expect_err("revoked send succeeded");
    let closed = tokio::time::timeout(DEADLINE, async {
        loop {
            match stream.recv().await {
                Err(error) => break error.to_string(),
                Ok(SubscribeSessionEvent::Closed { reason }) => break format!("{reason:?}"),
                Ok(SubscribeSessionEvent::Output { .. }) => {
                    panic!("revoked session delivered new output")
                }
                Ok(_) => {}
            }
        }
    })
    .await
    .expect("revoked phone's open session must close promptly");
    phone.cannot_call(&host).await;
    host.cannot_call(&phone).await;
    phone.trusts(&host).await;
    // The host and agent remain usable by their owner.
    send(&admin, agent.id, "owner-still-has-access")
        .await
        .unwrap();
    println!(
        "revocation session: {}",
        serde_json::json!({
            "route": if matches!(via, Via::Cloud) { "relay" } else { "direct-with-relay-fallback" },
            "echo_before_revocation": "before-revocation", "removed_device": removed.name,
            "revoked_stream_closed": closed, "first_send_after_unpair": error.to_string(),
            "fresh_call_refused": true, "owner_can_still_send": true,
        })
    );
}

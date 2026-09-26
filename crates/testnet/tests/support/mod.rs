//! What the network specifications share: reaching a host's edge and front
//! door, waiting for a condition, pairing through the front doors, and
//! reading a peer's inventory over the link.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use node::harness::HostVia;
use node::{Edge, Observed, RelayCarrier};
use testnet::{HostDecl, Net, PATIENCE};
use tonic::transport::Channel;
use uuid::Uuid;
use wire::profile_service_client::ProfileServiceClient;
use wire::{
    BeginPairRequest, Empty, InventoryEvent, PeerRef, PendingPairRequest, ProfileBeginPairRequest,
    ProfileOperation, ProfilePendingPairRequest, ProfileStartPairingRequest, StartPairingRequest,
    StartPairingResponse, begin_pair_request, inventory_event, peer_ref, start_pairing_request,
    start_pairing_response,
};

/// A host listening for direct links on loopback and on the net's
/// discovery bus, in `scope`.
pub fn lan_host(name: &str, scope: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        lan: true,
        discovery: true,
        scope: Some(scope.to_owned()),
        ..HostDecl::default()
    }
}

pub fn edge(net: &Net, host: &str) -> Arc<Edge> {
    net.edge(host).expect("the edge runs")
}

pub fn host_id(net: &Net, host: &str) -> Uuid {
    net.host(host).expect("the host").host_id
}

pub fn profile(net: &Net, host: &str) -> String {
    net.host(host).expect("the host").profile.to_string()
}

pub async fn door(net: &Net, host: &str) -> ProfileServiceClient<Channel> {
    net.front_door(host).await.expect("the front door answers")
}

pub fn host_ref(host: Uuid) -> Option<PeerRef> {
    Some(PeerRef {
        identifier: Some(peer_ref::Identifier::HostId(host.as_bytes().to_vec())),
    })
}

pub fn operation(profile: String) -> ProfileOperation {
    ProfileOperation {
        profile_id: profile,
        ..ProfileOperation::default()
    }
}

/// Polls `check` until it holds, failing the test after the patience runs
/// out with `what` it was waiting for.
pub async fn until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until each host reaches the other over `via`.
pub async fn until_via(net: &Net, a: &str, b: &str, via: HostVia) {
    let (a_id, b_id) = (host_id(net, a), host_id(net, b));
    let (near, far) = (edge(net, a), edge(net, b));
    until(&format!("{a} and {b} to reach each other {via:?}"), || {
        let (near, far) = (near.clone(), far.clone());
        async move { near.via(b_id).await == via && far.via(a_id).await == via }
    })
    .await;
}

/// The carrier the host's relay link rides, once it is connected.
pub fn carrier(net: &Net, host: &str) -> Option<RelayCarrier> {
    match edge(net, host).observed() {
        Observed::Connected { carrier, .. } => Some(carrier),
        _ => None,
    }
}

/// Reads a peer's inventory to its CaughtUp and returns the hosts it named.
pub async fn peer_inventory_hosts(from: &Edge, to: Uuid) -> Result<Vec<Vec<u8>>, tonic::Status> {
    let mut client = from
        .peer(to)
        .await
        .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
    let mut events = client.subscribe_inventory(Empty {}).await?.into_inner();
    let mut hosts = Vec::new();
    loop {
        let InventoryEvent { of } = events
            .message()
            .await?
            .ok_or_else(|| tonic::Status::aborted("the inventory ended before CaughtUp"))?;
        match of {
            Some(inventory_event::Of::Host(host)) => hosts.push(host.host_id),
            Some(inventory_event::Of::CaughtUp(_)) => return Ok(hosts),
            _ => {}
        }
    }
}

/// Puts `responder`'s profile into pairing mode.
pub async fn start_pairing(
    net: &Net,
    responder: &str,
    pairing: StartPairingRequest,
) -> Result<StartPairingResponse, tonic::Status> {
    door(net, responder)
        .await
        .start_pairing(ProfileStartPairingRequest {
            profile_id: profile(net, responder),
            pairing: Some(pairing),
            ..ProfileStartPairingRequest::default()
        })
        .await
        .map(tonic::Response::into_inner)
}

pub fn pin_mode() -> StartPairingRequest {
    StartPairingRequest {
        mode: start_pairing_request::Mode::Pin as i32,
        ..StartPairingRequest::default()
    }
}

pub fn qr_mode() -> StartPairingRequest {
    StartPairingRequest {
        mode: start_pairing_request::Mode::Qr as i32,
        ..StartPairingRequest::default()
    }
}

pub fn pin_of(started: &StartPairingResponse) -> String {
    match &started.secret {
        Some(start_pairing_response::Secret::Pin(pin)) => pin.clone(),
        other => panic!("a PIN pairing hands out a PIN, not {other:?}"),
    }
}

/// `initiator` begins pairing with the host at `addrs` using `secret`,
/// naming `host` when it knows it.
pub async fn begin_pair(
    net: &Net,
    initiator: &str,
    host: Option<Uuid>,
    secret: begin_pair_request::Secret,
    addrs: Vec<String>,
) -> Result<wire::PendingPairResponse, tonic::Status> {
    door(net, initiator)
        .await
        .begin_pair(ProfileBeginPairRequest {
            profile_id: profile(net, initiator),
            pairing: Some(BeginPairRequest {
                host_id: host
                    .map(|host| host.as_bytes().to_vec())
                    .unwrap_or_default(),
                secret: Some(secret),
                addrs,
            }),
            ..ProfileBeginPairRequest::default()
        })
        .await
        .map(tonic::Response::into_inner)
}

pub async fn confirm_pair(
    net: &Net,
    initiator: &str,
    token: Vec<u8>,
) -> Result<wire::PeerEntry, tonic::Status> {
    door(net, initiator)
        .await
        .confirm_pair(ProfilePendingPairRequest {
            profile_id: profile(net, initiator),
            pairing: Some(PendingPairRequest { token }),
            ..ProfilePendingPairRequest::default()
        })
        .await
        .map(|answer| answer.into_inner().peer.expect("the confirmed peer"))
}

/// Pairs `initiator` with `responder` through both front doors with a PIN
/// over the responder's LAN listener.
pub async fn pair(net: &Net, initiator: &str, responder: &str) -> wire::PeerEntry {
    let started = start_pairing(net, responder, pin_mode())
        .await
        .expect("the responder enters pairing mode");
    let pending = begin_pair(
        net,
        initiator,
        Some(host_id(net, responder)),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        started.addrs,
    )
    .await
    .expect("the SPAKE2 exchange completes");
    confirm_pair(net, initiator, pending.token)
        .await
        .expect("the person confirms the peer")
}

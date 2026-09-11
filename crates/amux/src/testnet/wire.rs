//! `WirePeer`: a scripted protocol-v2 actor over a real `MuxCarrier`.
//!
//! The actor drives the connector side of an in-memory ordered carrier while
//! the victim daemon's real routing state runs the acceptor side. The acceptor
//! context receives the peer identity established by its carrier boundary, so
//! spoofing exercises the same Hello-to-carrier binding as a network carrier.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::TestNet;
use super::assertions::DEFAULT_TIMEOUT;
use crate::HostId;
use crate::link::{
    AuthenticatedLinkUser, CarrierKind, LinkCarrier, LinkCtx, LinkTokenAuthenticator, MuxCarrier,
    MuxRole, read_message, run_link, write_message,
};
use crate::protocol::PROTOCOL_VERSION;
use crate::protocol::wire::pb;
use crate::routing::{Capabilities, ConnectRole, Host, host_to_wire};

/// Exercises the native stream boundary without gRPC obscuring its lifecycle:
/// graceful finish is EOF, while refusal remains a named reset reason.
pub async fn native_stream_lifecycle() -> (bool, &'static str) {
    let (connector_io, acceptor_io) = tokio::io::duplex(1024 * 1024);
    let connector = Arc::new(MuxCarrier::new(
        connector_io,
        MuxRole::Connector,
        CarrierKind::RelayTcp,
    ));
    let acceptor = Arc::new(MuxCarrier::new(
        acceptor_io,
        MuxRole::Acceptor,
        CarrierKind::RelayTcp,
    ));
    let (mut connector_sink, connector_source) = connector.control();
    let (acceptor_sink, mut acceptor_source) = acceptor.control();
    write_message(&mut connector_sink, &pb::Message { body: None })
        .await
        .expect("activate native carrier control stream");
    read_message(&mut acceptor_source)
        .await
        .expect("read native carrier control stream")
        .expect("native carrier control stream stays open");

    let open = {
        let connector = connector.clone();
        tokio::spawn(async move {
            connector
                .open_stream(pb::StreamPreface { dst: vec![1; 16] })
                .await
        })
    };
    let (_, mut accepted) = acceptor
        .accept_stream()
        .await
        .expect("accept stream to finish");
    accepted
        .write_all(b"accepted")
        .await
        .expect("accept native stream");
    let mut opened = open
        .await
        .expect("stream-open task")
        .expect("stream is accepted");
    let mut marker = [0_u8; 8];
    opened
        .read_exact(&mut marker)
        .await
        .expect("read acceptance marker");
    accepted.finish().await.expect("finish native stream");
    let finished_reads_eof = opened
        .read(&mut [0_u8; 1])
        .await
        .expect("read graceful stream finish")
        == 0;

    let refused_open = {
        let connector = connector.clone();
        tokio::spawn(async move {
            connector
                .open_stream(pb::StreamPreface { dst: vec![2; 16] })
                .await
        })
    };
    let (_, mut refused) = acceptor
        .accept_stream()
        .await
        .expect("accept stream to refuse");
    refused
        .reset(pb::StreamRefusal::PaymentRequired)
        .await
        .expect("reset native stream");
    let refusal_name = match refused_open.await.expect("refused stream-open task") {
        Err(crate::link::OpenError::Refused(pb::StreamRefusal::PaymentRequired)) => {
            "payment_required"
        }
        Err(error) => panic!("expected PAYMENT_REQUIRED refusal, got {error}"),
        Ok(_) => panic!("expected PAYMENT_REQUIRED refusal, but the stream was accepted"),
    };

    // The control handles own the link while the application streams are
    // exercised; keep both directions alive until the observations finish.
    drop((
        connector_sink,
        connector_source,
        acceptor_sink,
        acceptor_source,
    ));
    (finished_reads_eof, refusal_name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkCloseReason {
    Unspecified,
    ProtocolError,
    AuthExpired,
    UserShutdown,
    UpdateRequired,
    Other(i32),
}

impl LinkCloseReason {
    fn from_wire(reason: i32) -> Self {
        match pb::LinkCloseReason::try_from(reason) {
            Ok(pb::LinkCloseReason::Unspecified) => Self::Unspecified,
            Ok(pb::LinkCloseReason::ProtocolError) => Self::ProtocolError,
            Ok(pb::LinkCloseReason::AuthExpired) => Self::AuthExpired,
            Ok(pb::LinkCloseReason::UserShutdown) => Self::UserShutdown,
            Ok(pb::LinkCloseReason::UpdateRequired) => Self::UpdateRequired,
            _ => Self::Other(reason),
        }
    }
}

pub struct WirePeer {
    victim_name: String,
    bound: HostId,
    carrier: Arc<MuxCarrier>,
    sink: crate::link::ControlSink,
    source: crate::link::ControlSource,
    hello_auth_token: Option<String>,
}

#[derive(Clone)]
struct StaticTokenAuthenticator;

#[tonic::async_trait]
impl LinkTokenAuthenticator for StaticTokenAuthenticator {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        if token != "wire-valid" {
            return Err(tonic::Status::unauthenticated("expired wire token"));
        }
        Ok(AuthenticatedLinkUser {
            user_id: uuid::Uuid::from_u128(0xfeed),
            client_id: "wire-peer".to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(3600),
            tier: crate::Tier::Pro,
        })
    }
}

impl WirePeer {
    pub async fn connect_runtime_pair(net: &TestNet, connector: &str, acceptor: &str) {
        let connector_daemon = net.daemon(connector);
        let acceptor_daemon = net.daemon(acceptor);
        let connector_parts = connector_daemon
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("connector '{connector}' is not running"));
        let acceptor_parts = acceptor_daemon
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("acceptor '{acceptor}' is not running"));
        let connector_host = Host {
            id: connector_daemon.host_id(),
            name: connector.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: Capabilities::default(),
            signed_in: Some(false),
        };
        let acceptor_host = Host {
            id: acceptor_daemon.host_id(),
            name: acceptor.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: Capabilities::default(),
            signed_in: Some(false),
        };
        let (connector_io, acceptor_io) = tokio::io::duplex(1024 * 1024);
        let connector_carrier = Arc::new(MuxCarrier::new(
            connector_io,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ));
        let acceptor_carrier = Arc::new(MuxCarrier::new(
            acceptor_io,
            MuxRole::Acceptor,
            CarrierKind::RelayTcp,
        ));
        let connector_ctx = LinkCtx::new(
            connector_host,
            connector_parts.routing,
            connector_parts.channels.link_registry(),
        )
        .with_expected_peer(acceptor_daemon.host_id());
        let acceptor_ctx = LinkCtx::new(
            acceptor_host,
            acceptor_parts.routing,
            acceptor_parts.channels.link_registry(),
        )
        .with_authenticated_peer(connector_daemon.host_id());

        tokio::spawn(run_link(
            acceptor_ctx,
            acceptor_carrier,
            ConnectRole::Acceptor,
        ));
        tokio::spawn(run_link(
            connector_ctx,
            connector_carrier,
            ConnectRole::Connector,
        ));
    }

    pub async fn connect_trusted(net: &TestNet, victim: &str) -> Self {
        Self::connect(net, victim, false).await
    }

    pub async fn connect_authenticated(net: &TestNet, victim: &str) -> Self {
        Self::connect(net, victim, true).await
    }

    async fn connect(net: &TestNet, victim: &str, authenticated: bool) -> Self {
        let victim_daemon = net.daemon(victim);
        let victim_parts = victim_daemon
            .try_parts()
            .await
            .unwrap_or_else(|| panic!("victim '{victim}' is not running"));
        let bound = HostId::new_v4();
        let local = Host {
            id: victim_daemon.host_id(),
            name: victim.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: Capabilities::default(),
            signed_in: Some(false),
        };
        let (connector_io, acceptor_io) = tokio::io::duplex(1024 * 1024);
        let acceptor = Arc::new(MuxCarrier::new(
            acceptor_io,
            MuxRole::Acceptor,
            CarrierKind::RelayTcp,
        ));
        let mut acceptor_ctx = LinkCtx::new(
            local,
            victim_parts.routing,
            victim_parts.channels.link_registry(),
        )
        .with_authenticated_peer(bound);
        if authenticated {
            acceptor_ctx = acceptor_ctx
                .with_link_role(crate::routing::LinkRole::CloudRelay)
                .with_token_authenticator(Arc::new(StaticTokenAuthenticator), None);
        }
        tokio::spawn(run_link(acceptor_ctx, acceptor, ConnectRole::Acceptor));

        let carrier = Arc::new(MuxCarrier::new(
            connector_io,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ));
        let (sink, source) = carrier.control();
        Self {
            victim_name: victim.to_string(),
            bound,
            carrier,
            sink,
            source,
            hello_auth_token: authenticated.then(|| "wire-valid".to_string()),
        }
    }

    pub async fn hello(&mut self) {
        self.send_hello(self.bound, vec![PROTOCOL_VERSION]).await;
        self.expect_hello_ack_accepted().await;
    }

    pub async fn send_hello_spoofing_host_id(&mut self) {
        let mut spoofed = HostId::new_v4();
        while spoofed == self.bound {
            spoofed = HostId::new_v4();
        }
        self.send_hello(spoofed, vec![PROTOCOL_VERSION]).await;
    }

    pub async fn send_hello_with_unsupported_version(&mut self) {
        self.send_hello(self.bound, vec![PROTOCOL_VERSION + 1])
            .await;
    }

    pub async fn send_hello_with_auth_token(&mut self, token: &str) {
        self.hello_auth_token = Some(token.to_string());
        self.send_hello(self.bound, vec![PROTOCOL_VERSION]).await;
    }

    pub async fn send_malformed_first_frame(&mut self) {
        self.send_message(pb::Message { body: None }).await;
    }

    pub async fn send_hello(&mut self, host_id: HostId, versions: Vec<u32>) {
        self.send_hello_with_neighbors(host_id, versions, &[]).await;
    }

    pub async fn send_hello_with_neighbors(
        &mut self,
        host_id: HostId,
        versions: Vec<u32>,
        neighbors: &[(HostId, &str)],
    ) {
        let host = Host {
            id: host_id,
            name: "wire-peer".to_string(),
            version: "0.0.0-wire".to_string(),
            capabilities: Capabilities::default(),
            signed_in: Some(false),
        };
        self.send_message(pb::Message {
            body: Some(pb::message::Body::Hello(pb::Hello {
                supported_protocol_versions: versions,
                host: Some(host_to_wire(&host)),
                neighbors: neighbors
                    .iter()
                    .map(|(id, name)| {
                        host_to_wire(&Host {
                            id: *id,
                            name: (*name).to_string(),
                            version: "test".to_string(),
                            capabilities: Capabilities::default(),
                            signed_in: Some(false),
                        })
                    })
                    .collect(),
                auth_token: self.hello_auth_token.clone(),
            })),
        })
        .await;
    }

    pub fn host_id(&self) -> HostId {
        self.bound
    }

    pub async fn send_message(&mut self, message: pb::Message) {
        write_message(&mut self.sink, &message)
            .await
            .expect("wire peer control stream closed");
    }

    pub async fn send_oversize_control_message(&mut self) {
        crate::link::carrier::write_raw_control_frame(
            &mut self.sink,
            (crate::protocol::wire::MESSAGE_SIZE_LIMIT + 1) as u32,
        )
        .await
        .expect("write oversize control frame");
    }

    pub async fn open_stream_without_preface(&self) -> pb::StreamRefusal {
        self.carrier
            .open_without_preface()
            .await
            .expect("stream without preface should be refused")
    }

    pub async fn send_neighbor_up(&mut self, host_id: HostId, name: &str) {
        let host = Host {
            id: host_id,
            name: name.to_string(),
            version: "test".to_string(),
            capabilities: Capabilities::default(),
            signed_in: Some(false),
        };
        self.send_message(pb::Message {
            body: Some(pb::message::Body::NeighborUp(pb::NeighborUp {
                host: Some(host_to_wire(&host)),
            })),
        })
        .await;
    }

    pub async fn send_neighbor_down(&mut self, host_id: HostId) {
        self.send_message(pb::Message {
            body: Some(pb::message::Body::NeighborDown(pb::NeighborDown {
                host_id: host_id.as_bytes().to_vec(),
                reason: None,
            })),
        })
        .await;
    }

    pub async fn send_reauth(&mut self, token: &str) {
        self.send_message(pb::Message {
            body: Some(pb::message::Body::Reauth(pb::Reauth {
                auth_token: token.to_string(),
            })),
        })
        .await;
    }

    pub async fn expect_hello_ack_accepted(&mut self) {
        match self.recv_message().await {
            Some(pb::Message {
                body:
                    Some(pb::message::Body::HelloAck(pb::HelloAck {
                        outcome: Some(pb::hello_ack::Outcome::Accepted(accepted)),
                    })),
            }) => assert_eq!(accepted.protocol_version, PROTOCOL_VERSION),
            other => panic!("expected accepted HelloAck, got {other:?}"),
        }
    }

    pub async fn expect_hello_ack_error(&mut self) -> pb::Error {
        match self.recv_message().await {
            Some(pb::Message {
                body:
                    Some(pb::message::Body::HelloAck(pb::HelloAck {
                        outcome: Some(pb::hello_ack::Outcome::Error(error)),
                    })),
            }) => error,
            other => panic!("expected error HelloAck, got {other:?}"),
        }
    }

    pub async fn expect_link_close(&mut self, reason: LinkCloseReason) -> Option<pb::Error> {
        let deadline = tokio::time::Instant::now() + DEFAULT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, read_message(&mut self.source)).await {
                Ok(Ok(Some(pb::Message {
                    body: Some(pb::message::Body::LinkClose(close)),
                }))) => {
                    assert_eq!(LinkCloseReason::from_wire(close.reason), reason);
                    return close.error;
                }
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => panic!(
                    "'{}' closed before sending LinkClose({reason:?})",
                    self.victim_name
                ),
                Err(_) => panic!("timed out waiting for LinkClose({reason:?})"),
            }
        }
    }

    pub async fn expect_stream_closed(&self) {
        tokio::time::timeout(DEFAULT_TIMEOUT, self.carrier.closed())
            .await
            .expect("link stayed open");
    }

    pub async fn expect_stream_stays_open(&mut self) {
        const WINDOW: Duration = Duration::from_millis(200);
        match tokio::time::timeout(WINDOW, read_message(&mut self.source)).await {
            Err(_) => {}
            Ok(Ok(Some(message))) => panic!("unexpected control message: {message:?}"),
            Ok(Ok(None)) | Ok(Err(_)) => panic!("link closed unexpectedly"),
        }
    }

    async fn recv_message(&mut self) -> Option<pb::Message> {
        tokio::time::timeout(DEFAULT_TIMEOUT, read_message(&mut self.source))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for '{}'", self.victim_name))
            .unwrap_or_else(|error| panic!("control stream failed: {error}"))
    }
}

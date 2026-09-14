//! Relay forwarding for native link streams.

use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use wire::pb;

use super::{ByteStream, OpenError};
use crate::HostId;
use crate::resource_limits::{
    CLOUD_INBOUND_TUNNEL_RATE_LIMIT, CLOUD_INBOUND_TUNNEL_RATE_WINDOW, SlidingWindowRateLimiter,
};
use crate::routing::{LinkAdmission, LinkId, LinkRegistry};

#[derive(Clone)]
pub(crate) struct Piper {
    local_host: HostId,
    links: Arc<LinkRegistry>,
    limiter: Arc<Mutex<SlidingWindowRateLimiter<HostId>>>,
}

impl Piper {
    pub(crate) fn new(local_host: HostId, links: Arc<LinkRegistry>) -> Self {
        Self {
            local_host,
            links,
            limiter: Arc::new(Mutex::new(SlidingWindowRateLimiter::new(
                CLOUD_INBOUND_TUNNEL_RATE_LIMIT,
                CLOUD_INBOUND_TUNNEL_RATE_WINDOW,
            ))),
        }
    }

    pub(crate) async fn pipe(
        &self,
        origin: LinkId,
        preface: pb::StreamPreface,
        mut incoming: ByteStream,
    ) -> Result<JoinHandle<()>, pb::StreamRefusal> {
        let destination = match HostId::from_slice(&preface.dst) {
            Ok(destination) if destination != self.local_host => destination,
            _ => return refuse(&mut incoming, pb::StreamRefusal::NotAdjacent).await,
        };

        let Some(origin_admission) = self.links.admission(&origin).await else {
            return refuse(&mut incoming, pb::StreamRefusal::NoRoute).await;
        };
        if is_free_cloud(origin_admission) {
            return refuse(&mut incoming, pb::StreamRefusal::PaymentRequired).await;
        }
        if !self.limiter.lock().await.allow(origin.peer()) {
            return refuse(&mut incoming, pb::StreamRefusal::RateLimited).await;
        }

        let Some((_, outgoing, destination_admission)) =
            self.links.native_route_to_peer(destination).await
        else {
            return refuse(&mut incoming, pb::StreamRefusal::NoRoute).await;
        };
        if is_free_cloud(destination_admission) {
            return refuse(&mut incoming, pb::StreamRefusal::PaymentRequired).await;
        }

        let outgoing = match outgoing.open_stream(preface).await {
            Ok(stream) => stream,
            Err(OpenError::Refused(reason)) => return refuse(&mut incoming, reason).await,
            Err(OpenError::LinkClosed | OpenError::Io(_)) => {
                return refuse(&mut incoming, pb::StreamRefusal::NoRoute).await;
            }
        };
        if incoming.flush().await.is_err() {
            return Err(pb::StreamRefusal::NoRoute);
        }
        Ok(tokio::spawn(async move {
            let (mut incoming_read, mut incoming_write) = tokio::io::split(incoming);
            let (mut outgoing_read, mut outgoing_write) = tokio::io::split(outgoing);
            tokio::select! {
                _ = tokio::io::copy(&mut incoming_read, &mut outgoing_write) => {}
                _ = tokio::io::copy(&mut outgoing_read, &mut incoming_write) => {}
            }
            let mut incoming = incoming_read.unsplit(incoming_write);
            let mut outgoing = outgoing_read.unsplit(outgoing_write);
            let _ = incoming.finish().await;
            let _ = outgoing.finish().await;
        }))
    }
}

fn is_free_cloud(admission: LinkAdmission) -> bool {
    matches!(
        admission,
        LinkAdmission::CloudToken {
            tier: crate::Tier::Free
        }
    )
}

async fn refuse(
    stream: &mut ByteStream,
    reason: pb::StreamRefusal,
) -> Result<JoinHandle<()>, pb::StreamRefusal> {
    let _ = stream.reset(reason).await;
    Err(reason)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;

    use super::*;
    use crate::link::{
        CarrierKind, ControlSink, ControlSource, LinkCarrier as NativeLinkCarrier, MuxCarrier,
        MuxRole,
    };
    use crate::routing::{
        Capabilities, Host, LinkCarrier, LinkProperties, LinkRole, SupportedAgentType,
    };

    fn host(id: u128) -> Host {
        Host {
            id: HostId::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types: vec![SupportedAgentType {
                    agent_type: "test-agent".to_string(),
                }],
            },
            signed_in: Some(false),
            platform: None,
        }
    }

    struct CarrierPair {
        connector: Arc<MuxCarrier>,
        acceptor: Arc<MuxCarrier>,
        _connector_control: (ControlSink, ControlSource),
        _acceptor_control: (ControlSink, ControlSource),
    }

    async fn carriers() -> CarrierPair {
        let (connector_io, acceptor_io) = tokio::io::duplex(2 * 1024 * 1024);
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
        crate::link::write_message(&mut connector_sink, &pb::Message { body: None })
            .await
            .unwrap();
        crate::link::read_message(&mut acceptor_source)
            .await
            .unwrap();
        CarrierPair {
            connector,
            acceptor,
            _connector_control: (connector_sink, connector_source),
            _acceptor_control: (acceptor_sink, acceptor_source),
        }
    }

    async fn register_link(
        links: &LinkRegistry,
        peer: u128,
        admission: LinkAdmission,
        carrier: Option<Arc<dyn NativeLinkCarrier>>,
    ) -> LinkId {
        let link = LinkId::new(HostId::from_u128(peer));
        let (outgoing, _) = mpsc::channel(8);
        links
            .register_with_details(
                link,
                host(peer),
                outgoing,
                LinkProperties {
                    role: LinkRole::Peer,
                    admission,
                    carrier: LinkCarrier::Direct,
                    direct_order: None,
                },
                &[],
                carrier,
            )
            .await
            .expect("piper test links are unique");
        link
    }

    async fn incoming(
        opener: Arc<MuxCarrier>,
        receiver: Arc<MuxCarrier>,
        destination: HostId,
    ) -> (
        tokio::task::JoinHandle<Result<ByteStream, OpenError>>,
        pb::StreamPreface,
        ByteStream,
    ) {
        let preface = pb::StreamPreface {
            dst: destination.as_bytes().to_vec(),
        };
        let opening = tokio::spawn({
            let preface = preface.clone();
            async move { opener.open_stream(preface).await }
        });
        let (received, stream) =
            tokio::time::timeout(Duration::from_secs(1), receiver.accept_stream())
                .await
                .expect("timed out waiting for the relay-side stream")
                .unwrap();
        assert_eq!(received, preface);
        (opening, preface, stream)
    }

    async fn assert_refusal(
        piper: &Piper,
        origin_link: LinkId,
        opener: Arc<MuxCarrier>,
        receiver: Arc<MuxCarrier>,
        destination: HostId,
        expected: pb::StreamRefusal,
    ) {
        let (opening, preface, stream) = incoming(opener, receiver, destination).await;
        assert!(matches!(
            piper.pipe(origin_link, preface, stream).await,
            Err(actual) if actual == expected
        ));
        assert!(matches!(
            opening.await.unwrap(),
            Err(OpenError::Refused(actual)) if actual == expected
        ));
    }

    #[tokio::test]
    async fn every_policy_refusal_is_reset_before_bytes_are_copied() {
        let local = HostId::from_u128(1);

        let links = Arc::new(LinkRegistry::default());
        let origin = register_link(&links, 2, LinkAdmission::PinnedKey, None).await;
        let piper = Piper::new(local, links.clone());
        let pair = carriers().await;
        assert_refusal(
            &piper,
            origin,
            pair.connector.clone(),
            pair.acceptor.clone(),
            local,
            pb::StreamRefusal::NotAdjacent,
        )
        .await;
        assert_refusal(
            &piper,
            origin,
            pair.connector.clone(),
            pair.acceptor.clone(),
            HostId::from_u128(99),
            pb::StreamRefusal::NoRoute,
        )
        .await;

        let free_links = Arc::new(LinkRegistry::default());
        let free_origin = register_link(
            &free_links,
            2,
            LinkAdmission::CloudToken {
                tier: crate::Tier::Free,
            },
            None,
        )
        .await;
        let free_piper = Piper::new(local, free_links);
        let free_pair = carriers().await;
        assert_refusal(
            &free_piper,
            free_origin,
            free_pair.connector.clone(),
            free_pair.acceptor.clone(),
            HostId::from_u128(99),
            pb::StreamRefusal::PaymentRequired,
        )
        .await;

        let rate_links = Arc::new(LinkRegistry::default());
        let rate_origin = register_link(&rate_links, 2, LinkAdmission::PinnedKey, None).await;
        let rate_piper = Piper::new(local, rate_links);
        let rate_pair = carriers().await;
        for _ in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT {
            assert_refusal(
                &rate_piper,
                rate_origin,
                rate_pair.connector.clone(),
                rate_pair.acceptor.clone(),
                HostId::from_u128(99),
                pb::StreamRefusal::NoRoute,
            )
            .await;
        }
        assert_refusal(
            &rate_piper,
            rate_origin,
            rate_pair.connector.clone(),
            rate_pair.acceptor.clone(),
            HostId::from_u128(99),
            pb::StreamRefusal::RateLimited,
        )
        .await;
    }

    #[tokio::test]
    async fn two_pinned_key_links_pipe_with_no_tier_anywhere() {
        let local = HostId::from_u128(1);
        let links = Arc::new(LinkRegistry::default());
        let origin_link = register_link(&links, 2, LinkAdmission::PinnedKey, None).await;
        let destination_pair = carriers().await;
        register_link(
            &links,
            3,
            LinkAdmission::PinnedKey,
            Some(destination_pair.connector.clone()),
        )
        .await;
        let piper = Piper::new(local, links);
        let origin_pair = carriers().await;
        let (opening, preface, incoming) = incoming(
            origin_pair.connector.clone(),
            origin_pair.acceptor.clone(),
            HostId::from_u128(3),
        )
        .await;

        let copy_task = tokio::spawn(async move {
            piper
                .pipe(origin_link, preface, incoming)
                .await
                .unwrap()
                .await
                .unwrap();
        });
        let (received, mut destination_stream) = tokio::time::timeout(
            Duration::from_secs(1),
            destination_pair.acceptor.accept_stream(),
        )
        .await
        .expect("timed out waiting for the destination stream")
        .unwrap();
        assert_eq!(received.dst, HostId::from_u128(3).as_bytes());
        tokio::time::timeout(Duration::from_secs(1), destination_stream.flush())
            .await
            .expect("timed out accepting the destination stream")
            .unwrap();
        let mut origin_stream = tokio::time::timeout(Duration::from_secs(1), opening)
            .await
            .expect("timed out accepting the origin stream")
            .unwrap()
            .unwrap();

        origin_stream.write_all(b"outbound").await.unwrap();
        let mut outbound = [0; 8];
        tokio::time::timeout(
            Duration::from_secs(1),
            destination_stream.read_exact(&mut outbound),
        )
        .await
        .expect("timed out copying toward the destination")
        .unwrap();
        assert_eq!(&outbound, b"outbound");

        destination_stream.write_all(b"inbound").await.unwrap();
        let mut inbound = [0; 7];
        tokio::time::timeout(
            Duration::from_secs(1),
            origin_stream.read_exact(&mut inbound),
        )
        .await
        .expect("timed out copying toward the origin")
        .unwrap();
        assert_eq!(&inbound, b"inbound");

        destination_stream.finish().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), copy_task)
            .await
            .expect("the piper copy task must end when either stream ends")
            .unwrap();
    }
}

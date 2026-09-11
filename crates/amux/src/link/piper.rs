//! Relay forwarding for native link streams.

use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::{ByteStream, OpenError};
use crate::protocol::wire::pb;
use crate::resource_limits::{
    CLOUD_INBOUND_TUNNEL_RATE_LIMIT, CLOUD_INBOUND_TUNNEL_RATE_WINDOW, SlidingWindowRateLimiter,
};
use crate::routing::{LinkAdmission, LinkId, LinkRegistry};
use crate::HostId;

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

        let mut outgoing = match outgoing.open_stream(preface).await {
            Ok(stream) => stream,
            Err(OpenError::Refused(reason)) => return refuse(&mut incoming, reason).await,
            Err(OpenError::LinkClosed | OpenError::Io(_)) => {
                return refuse(&mut incoming, pb::StreamRefusal::NoRoute).await;
            }
        };
        Ok(tokio::spawn(async move {
            let (mut incoming_read, mut incoming_write) = tokio::io::split(incoming);
            let (mut outgoing_read, mut outgoing_write) = tokio::io::split(outgoing);
            tokio::select! {
                _ = tokio::io::copy(&mut incoming_read, &mut outgoing_write) => {}
                _ = tokio::io::copy(&mut outgoing_read, &mut incoming_write) => {}
            }
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

mod agent;
mod host;
mod reauth;

use tokio::sync::mpsc;

use crate::protocol::message::{Message, ReauthRequest, RoutingEvent};
use crate::server::connection::{ConnectionContext, ConnectionError};
use crate::transport::TransportError;

pub(super) async fn handle_ping(
    tx: &mpsc::Sender<Message>,
) -> crate::server::connection::Result<()> {
    tx.send(Message::Pong).await.map_err(|_| {
        ConnectionError::Transport(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "outgoing channel closed while sending heartbeat ack",
        )))
    })?;
    Ok(())
}

pub(super) async fn handle_reauth(
    tx: &mpsc::Sender<Message>,
    request: ReauthRequest,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    reauth::handle(tx, request.token, ctx).await
}

pub(super) async fn handle_peer_event(
    event: RoutingEvent,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    match event {
        event @ RoutingEvent::AgentUp { .. } => agent::handle_announce(event, ctx).await,
        RoutingEvent::AgentDown { agent_id } => agent::handle_withdraw(agent_id, ctx).await,
        RoutingEvent::HostUp { host, route } => host::handle_announce(host, route, ctx).await,
        RoutingEvent::HostDown { id, route } => host::handle_withdraw(id, route, ctx).await,
        RoutingEvent::SnapshotComplete => Ok(()),
        RoutingEvent::Unknown => {
            tracing::warn!("dropping unknown peer routing event");
            Ok(())
        }
    }
}

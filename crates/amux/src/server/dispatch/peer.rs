mod agent;
mod host;
mod reauth;

use tokio::sync::mpsc;

use crate::protocol::message::{AgentEvent, Message, ReauthRequest, RoutingEvent};
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
        RoutingEvent::HostUp { host, route } => host::handle_announce(host, route, ctx).await,
        RoutingEvent::HostDown { id, route } => host::handle_withdraw(id, route, ctx).await,
        RoutingEvent::SnapshotComplete => Ok(()),
        RoutingEvent::Unknown => {
            tracing::warn!("dropping unknown peer routing event");
            Ok(())
        }
    }
}

pub(super) async fn handle_agent_event(
    event: AgentEvent,
    subscribed_host_id: uuid::Uuid,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    match event {
        event @ AgentEvent::AgentUp { host_id, .. } => {
            if host_id != subscribed_host_id {
                tracing::warn!(
                    host_id = %host_id,
                    subscribed_host_id = %subscribed_host_id,
                    "dropping AgentUp for host outside subscription"
                );
                return Ok(());
            }
            agent::handle_announce(event, ctx).await
        }
        AgentEvent::AgentDown { agent_id } => {
            agent::handle_withdraw(agent_id, subscribed_host_id, ctx).await
        }
        AgentEvent::SnapshotComplete => Ok(()),
        AgentEvent::Unknown => {
            tracing::warn!("dropping unknown agent event");
            Ok(())
        }
    }
}

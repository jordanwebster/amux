use uuid::Uuid;

use crate::agent::Agent;
use crate::protocol::Route;
use crate::protocol::message::RoutingEvent;
use crate::server::connection::ConnectionContext;
use crate::server::routing::{PeerAgentDownIgnored, PeerAgentUpIgnored, broadcast_topology_event};

/// Process an AgentUp routing event. Caller must ensure this is the
/// `RoutingEvent::AgentUp` variant; other variants are a programmer error.
pub(super) async fn handle_announce(
    event: RoutingEvent,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let RoutingEvent::AgentUp {
        agent_id,
        host_id,
        name,
        command,
        working_dir,
        agent_type,
        io_protocols,
        readonly,
        args,
        created_at,
    } = event
    else {
        unreachable!("handle_announce called with non-AgentUp event");
    };

    let mut us = ctx.user_state.write().await;
    let info = Agent {
        id: agent_id,
        host_id,
        name: name.clone(),
        command,
        working_dir,
        route: Route::empty(),
        agent_type,
        io_protocols,
        readonly,
        args,
        created_at,
    };
    let change = us.apply_peer_agent_up(&ctx.link, info);
    if let Some(ignored) = change.ignored {
        match ignored {
            PeerAgentUpIgnored::LocalAgent => {
                tracing::debug!(agent_id = %agent_id, "ignoring announce for local agent");
            }
            PeerAgentUpIgnored::UnknownHost => {
                tracing::warn!(agent_id = %agent_id, host_id = %host_id, peer = %ctx.link, "ignoring remote agent announcement: unknown host");
            }
            PeerAgentUpIgnored::NonSelectedHostRoute => {
                tracing::warn!(agent_id = %agent_id, host_id = %host_id, peer = %ctx.link, "ignoring remote agent announcement: non-selected host route");
            }
        }
        return Ok(());
    }

    tracing::info!(agent_id = %agent_id, name = ?name, "stored remote agent");

    if let Some(event) = change.event {
        broadcast_topology_event(&mut us, &event, Some(&ctx.link));
    }

    Ok(())
}

pub(super) async fn handle_withdraw(
    agent_id: Uuid,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let mut us = ctx.user_state.write().await;
    let change = us.apply_peer_agent_down(&ctx.link, agent_id);

    if let Some(removed) = change.removed {
        tracing::info!(agent_id = %agent_id, "withdrew remote agent");
        broadcast_topology_event(&mut us, &removed.event, Some(&ctx.link));
    } else if let Some(ignored) = change.ignored {
        match ignored {
            PeerAgentDownIgnored::UnknownAgent => {
                tracing::debug!(agent_id = %agent_id, "ignoring withdraw for unknown agent");
            }
            PeerAgentDownIgnored::LocalAgent => {
                tracing::debug!(agent_id = %agent_id, "ignoring withdraw for local agent");
            }
            PeerAgentDownIgnored::NonSelectedHostRoute => {
                tracing::debug!(agent_id = %agent_id, "ignoring withdraw (link mismatch)");
            }
        }
    }

    Ok(())
}

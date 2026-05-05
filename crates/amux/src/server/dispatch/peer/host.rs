use uuid::Uuid;

use crate::protocol::route::Route;
use crate::server::connection::{
    ConnectionContext, drain_local_origin_routed_unreachable_for_route,
};
use crate::server::routing::{TopologyEffect, broadcast_topology_event};
use crate::server::{cancel_open_sessions_for_route_prefix, finish_open_session_cleanup_jobs};

pub(super) async fn handle_announce(
    id: Uuid,
    name: String,
    received_route: Route,
    version: String,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let host_id = {
        let state = ctx.state.read().await;
        state.host_id
    };

    // Skip our own host announcement
    if id == host_id {
        tracing::debug!("ignoring announce for own host");
        return Ok(());
    }

    let mut us = ctx.user_state.write().await;
    let change =
        us.topology
            .apply_peer_host_up(&ctx.link, id, name.clone(), received_route, version);

    tracing::info!(
        host_id = %id,
        name = %name,
        rewritten_descendants = change.rewritten_descendants,
        "stored remote host"
    );

    for event in &change.events {
        broadcast_topology_event(&mut us, event, Some(&ctx.link));
    }

    Ok(())
}

pub(super) async fn handle_withdraw(
    id: Uuid,
    received_route: Route,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let rpc = ctx.rpc();
    let (cleanup_jobs, local_origin_messages, withdraw_message) = {
        let mut us = ctx.user_state.write().await;

        let change = us
            .topology
            .apply_peer_host_down(&ctx.link, id, received_route);

        tracing::info!(
            host_id = %id,
            root_matches = change.root_matches,
            "received withdraw host"
        );

        if change.removed_agents != 0 {
            tracing::info!(count = change.removed_agents, host_id = %id, "removed agents for withdrawn host");
        }

        let (cleanup_jobs, local_origin_messages) = if let Some(effect) = &change.effect {
            let TopologyEffect::CancelSessionsForRoute { route_prefix } = effect else {
                unreachable!("host-down topology change returned non-host-route effect");
            };
            let local_origin_messages = drain_local_origin_routed_unreachable_for_route(
                &rpc,
                &us,
                route_prefix,
                "route withdrawn",
            );
            let (cancelled_open_sessions, cleanup_jobs) =
                cancel_open_sessions_for_route_prefix(&mut us, route_prefix);
            if cancelled_open_sessions != 0 {
                tracing::info!(
                    count = cancelled_open_sessions,
                    host_id = %id,
                    "cancelled OpenSession calls for withdrawn host"
                );
            }
            (cleanup_jobs, local_origin_messages)
        } else {
            (Vec::new(), Vec::new())
        };

        if change.removed_descendants > 0 {
            tracing::info!(
                count = change.removed_descendants,
                host_id = %id,
                "removed descendant hosts for withdrawn host"
            );
        }

        if change.root_matches {
            tracing::info!(host_id = %id, "withdrew remote host");
        } else {
            tracing::debug!(host_id = %id, "ignoring withdraw host without matching local root");
        }

        (cleanup_jobs, local_origin_messages, change.event)
    };
    for (handle, message) in local_origin_messages {
        let _ = handle.send(message).await;
    }
    {
        if let Some(withdraw_message) = withdraw_message {
            let mut us = ctx.user_state.write().await;
            broadcast_topology_event(&mut us, &withdraw_message, Some(&ctx.link));
        }
    }
    finish_open_session_cleanup_jobs(&ctx.user_state, cleanup_jobs).await;

    Ok(())
}

use uuid::Uuid;

use crate::protocol::message::Host;
use crate::protocol::route::Route;
use crate::server::connection::{
    ConnectionContext, ConnectionError, drain_local_origin_routed_unreachable_for_route,
};
use crate::server::routing::{broadcast_topology_event, maybe_start_agent_subscription};
use crate::server::{
    cancel_open_sessions_for_route_prefix, finish_open_session_cleanup_jobs, validate_remote_host,
};

pub(super) async fn handle_announce(
    host: Host,
    received_route: Route,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if received_route.is_empty() {
        tracing::warn!(
            host_id = %host.id,
            peer = %ctx.link,
            "invalid HostUp for direct peer route"
        );
        return Err(ConnectionError::Protocol(
            "HostUp route must not be empty".to_string(),
        ));
    }

    let id = host.id;
    let (host_id, is_cloud_server) = {
        let state = ctx.state.read().await;
        (state.host_id, state.is_cloud_server)
    };

    // Skip our own host announcement
    if id == host_id {
        tracing::debug!("ignoring announce for own host");
        return Ok(());
    }

    if let Err(message) = validate_remote_host(&host) {
        tracing::warn!(host_id = %id, peer = %ctx.link, reason = %message, "invalid remote host announcement");
        return Err(ConnectionError::Protocol(format!(
            "invalid HostUp host: {message}"
        )));
    }

    let mut us = ctx.user_state.write().await;
    let change = us.apply_peer_host_up(&ctx.link, host.clone(), received_route);

    tracing::info!(
        host_id = %id,
        name = %host.name,
        rewritten_descendants = change.rewritten_descendants,
        "stored remote host"
    );

    for event in &change.events {
        broadcast_topology_event(&mut us, event, Some(&ctx.link));
        if let crate::server::routing::TopologyEvent::HostUp { host, .. } = event {
            maybe_start_agent_subscription(&mut us, host.id, is_cloud_server);
        }
    }

    Ok(())
}

pub(super) async fn handle_withdraw(
    id: Uuid,
    received_route: Route,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if received_route.is_empty() {
        tracing::warn!(
            host_id = %id,
            peer = %ctx.link,
            "invalid HostDown for direct peer route"
        );
        return Err(ConnectionError::Protocol(
            "HostDown route must not be empty".to_string(),
        ));
    }

    let rpc = ctx.rpc();
    let (cleanup_jobs, local_origin_messages, withdraw_message) = {
        let mut us = ctx.user_state.write().await;
        let mut route_prefix = received_route.clone();
        route_prefix.push(ctx.link.clone());
        let root_matches = us
            .routes
            .get(&route_prefix)
            .is_some_and(|context| context.host_id == id);

        let (cleanup_jobs, local_origin_messages) = if root_matches {
            let local_origin_messages = drain_local_origin_routed_unreachable_for_route(
                &rpc,
                &us,
                &route_prefix,
                "route withdrawn",
            );
            let (cancelled_open_sessions, cleanup_jobs) =
                cancel_open_sessions_for_route_prefix(&mut us, &route_prefix);
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

        let change = us.apply_peer_host_down(&ctx.link, id, received_route);

        tracing::info!(
            host_id = %id,
            root_matches = change.root_matches,
            "received withdraw host"
        );

        if change.removed_agents != 0 {
            tracing::info!(count = change.removed_agents, host_id = %id, "removed agents for withdrawn host");
        }

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
            let is_cloud_server = ctx.state.read().await.is_cloud_server;
            let mut us = ctx.user_state.write().await;
            broadcast_topology_event(&mut us, &withdraw_message, Some(&ctx.link));
            maybe_start_agent_subscription(&mut us, id, is_cloud_server);
        }
    }
    finish_open_session_cleanup_jobs(&ctx.user_state, cleanup_jobs).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::protocol::Link;
    use crate::server::{LOCAL_USER_ID, local_host, test_helpers};

    async fn test_context() -> (ConnectionContext, ArcUserState) {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let link = Link::new("peer").unwrap();
        let rpc = {
            let mut us = user_state.write().await;
            let (_handle, _rx) = us.try_reserve_link(link.clone()).unwrap();
            us.mark_peer_link(link.clone());
            us.rpc_for_link(&link).unwrap()
        };
        let ctx = ConnectionContext {
            state,
            rpc,
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link,
            is_local: false,
            heartbeat: None,
            routing_role: crate::protocol::handshake::RoutingRole::Host,
        };
        (ctx, user_state)
    }

    type ArcUserState = std::sync::Arc<tokio::sync::RwLock<crate::server::ServerUserState>>;

    #[tokio::test]
    async fn invalid_remote_host_up_is_rejected_without_storing_host() {
        let (ctx, user_state) = test_context().await;
        let host = Host {
            id: Uuid::from_u128(123),
            name: "remote".to_string(),
            version: String::new(),
            capabilities: Default::default(),
        };

        let result = handle_announce(
            host,
            Route::from_link(Link::new("behind-peer").unwrap()),
            &ctx,
        )
        .await;

        assert!(
            matches!(result, Err(ConnectionError::Protocol(message)) if message.contains("version"))
        );
        assert_eq!(user_state.read().await.host_count(), 0);
    }

    #[tokio::test]
    async fn empty_route_host_up_is_rejected_without_changing_direct_host() {
        let (ctx, user_state) = test_context().await;
        let direct_host = local_host(Uuid::from_u128(123), "peer", false);
        let announced_host = local_host(Uuid::from_u128(456), "other", false);
        {
            let mut us = user_state.write().await;
            us.apply_direct_peer_host_up(&ctx.link, direct_host.clone());
        }

        let result = handle_announce(announced_host, Route::empty(), &ctx).await;

        assert!(
            matches!(result, Err(ConnectionError::Protocol(message)) if message.contains("HostUp route"))
        );
        assert_eq!(
            user_state
                .read()
                .await
                .host_for_link(&ctx.link)
                .expect("direct host should remain")
                .id,
            direct_host.id
        );
    }

    #[tokio::test]
    async fn empty_route_host_down_is_rejected_without_removing_direct_host() {
        let (ctx, user_state) = test_context().await;
        let direct_host = local_host(Uuid::from_u128(123), "peer", false);
        {
            let mut us = user_state.write().await;
            us.apply_direct_peer_host_up(&ctx.link, direct_host.clone());
        }

        let result = handle_withdraw(direct_host.id, Route::empty(), &ctx).await;

        assert!(
            matches!(result, Err(ConnectionError::Protocol(message)) if message.contains("HostDown route"))
        );
        assert_eq!(
            user_state
                .read()
                .await
                .host_for_link(&ctx.link)
                .expect("direct host should remain")
                .id,
            direct_host.id
        );
    }
}

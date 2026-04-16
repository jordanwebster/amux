use uuid::Uuid;

use crate::protocol::message::DirectMessage;
use crate::protocol::route::Route;
use crate::server::ServerUserState;
use crate::server::connection::{ConnectionContext, cancel_subscriptions_matching};
use crate::server::routing::broadcast_to_peers;

fn descendant_host_ids(
    us: &ServerUserState,
    root_host_id: Uuid,
    route_prefix: &Route,
) -> Vec<Uuid> {
    let mut ids: Vec<_> = us
        .hosts
        .iter()
        .filter(|(id, host)| **id != root_host_id && host.route.starts_with_route(route_prefix))
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable_by_key(|id| id.as_u128());
    ids
}

fn rewrite_descendant_host_routes(
    us: &mut ServerUserState,
    root_host_id: Uuid,
    old_route: &Route,
    new_route: &Route,
) -> usize {
    if old_route == new_route {
        return 0;
    }

    descendant_host_ids(us, root_host_id, old_route)
        .into_iter()
        .map(|id| {
            let host = us
                .hosts
                .get_mut(&id)
                .expect("descendant host should still exist while rewriting routes");
            let replaced = host.route.replace_prefix(old_route, new_route);
            debug_assert!(replaced, "descendant route should still match old prefix");
            1usize
        })
        .sum()
}

fn remove_descendant_hosts(
    us: &mut ServerUserState,
    root_host_id: Uuid,
    route_prefix: &Route,
) -> usize {
    descendant_host_ids(us, root_host_id, route_prefix)
        .into_iter()
        .filter(|id| us.hosts.remove(id).is_some())
        .count()
}

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
    let old_route = us.hosts.get(&id).map(|host| host.route.clone());

    let mut our_route = received_route;
    our_route.push(ctx.link.clone());

    let info = crate::protocol::message::Host {
        id,
        name: name.clone(),
        route: our_route.clone(),
        version: version.clone(),
    };

    us.hosts.insert(id, info);

    // Keep descendant host routes aligned with the selected route for this host.
    let rewritten_descendants = old_route
        .as_ref()
        .map(|old_route| rewrite_descendant_host_routes(&mut us, id, old_route, &our_route))
        .unwrap_or(0);

    tracing::info!(
        host_id = %id,
        name = %name,
        rewritten_descendants,
        "stored remote host"
    );

    broadcast_to_peers(
        &mut us,
        &DirectMessage::AnnounceHost {
            id,
            name,
            route: our_route,
            version,
        },
        Some(&ctx.link),
    );

    Ok(())
}

pub(super) async fn handle_withdraw(
    id: Uuid,
    received_route: Route,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let mut us = ctx.user_state.write().await;

    let mut withdrawn_route = received_route;
    withdrawn_route.push(ctx.link.clone());

    let root_matches = us
        .hosts
        .get(&id)
        .is_some_and(|h| h.route == withdrawn_route);
    tracing::info!(host_id = %id, root_matches, "received withdraw host");

    let ServerUserState {
        ref hosts,
        ref mut registry,
        ..
    } = *us;
    let removed = registry.remove_where(
        |hid| hosts.get(&hid).map(|h| h.route.clone()),
        |r| r.starts_with_route(&withdrawn_route),
    );
    if !removed.is_empty() {
        tracing::info!(count = removed.len(), host_id = %id, "removed agents for withdrawn host");
    }

    let cancelled = cancel_subscriptions_matching(&mut us, |entry| {
        entry.dst.starts_with_route(&withdrawn_route)
    });
    if !cancelled.is_empty() {
        tracing::info!(
            count = cancelled.len(),
            host_id = %id,
            "cancelled subscriptions for withdrawn host"
        );
    }

    let removed_descendants = remove_descendant_hosts(&mut us, id, &withdrawn_route);
    if removed_descendants > 0 {
        tracing::info!(
            count = removed_descendants,
            host_id = %id,
            "removed descendant hosts for withdrawn host"
        );
    }

    if root_matches {
        us.hosts.remove(&id);
        tracing::info!(host_id = %id, "withdrew remote host");
    } else {
        tracing::debug!(host_id = %id, "propagating withdraw host without matching local root");
    }

    broadcast_to_peers(
        &mut us,
        &DirectMessage::WithdrawHost {
            id,
            route: withdrawn_route,
        },
        Some(&ctx.link),
    );

    Ok(())
}

use uuid::Uuid;

use crate::agent::Agent;
use crate::protocol::message::DirectMessage;
use crate::server::connection::ConnectionContext;
use crate::server::routing::{announce_agent_message, broadcast_to_peers};

/// Process an AnnounceAgent message. Caller must ensure this is the
/// `DirectMessage::AnnounceAgent` variant; other variants are a programmer error.
pub(super) async fn handle_announce(
    announce: DirectMessage,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let DirectMessage::AnnounceAgent {
        agent_id,
        host_id,
        name,
        command,
        working_dir,
        agent_type,
        structured_protocol,
        readonly,
        args,
        created_at,
    } = announce
    else {
        unreachable!("handle_announce called with non-AnnounceAgent variant");
    };

    let mut us = ctx.user_state.write().await;

    // Local agent takes precedence — skip if we own this agent
    if us.agents.contains_key(&agent_id) {
        tracing::debug!(agent_id = %agent_id, "ignoring announce for local agent");
        return Ok(());
    }

    // Only accept agent metadata from the selected next hop for this host.
    // This prevents stale or alternate paths from republishing the agent on
    // a route we no longer consider canonical, which would then cause the
    // real sender's later WithdrawAgent to be ignored as a link mismatch.
    let host_ok = us
        .hosts
        .get(&host_id)
        .is_some_and(|host| matches!(host.route.peek(), Some(link) if *link == ctx.link));
    if !host_ok {
        let reason = if us.hosts.contains_key(&host_id) {
            "non-selected host route"
        } else {
            "unknown host"
        };
        tracing::warn!(agent_id = %agent_id, host_id = %host_id, peer = %ctx.link, "ignoring remote agent announcement: {reason}");
        return Ok(());
    }

    let route = us
        .hosts
        .get(&host_id)
        .expect("host_ok implies host exists")
        .route
        .clone();
    let info = Agent {
        id: agent_id,
        host_id,
        name: name.clone(),
        command,
        working_dir,
        route,
        agent_type,
        structured_protocol,
        readonly,
        args,
        created_at,
    };

    let announce = announce_agent_message(&info);
    if let Err(e) = us.registry.register_remote(info) {
        tracing::warn!(error = %e, agent_id = %agent_id, "ignoring invalid remote announcement");
        return Ok(());
    }

    tracing::info!(agent_id = %agent_id, name = ?name, "stored remote agent");

    // Propagate to other peers with our stored route
    broadcast_to_peers(&mut us, &announce, Some(&ctx.link));

    Ok(())
}

pub(super) async fn handle_withdraw(
    agent_id: Uuid,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let mut us = ctx.user_state.write().await;

    // Only remove if the stored link matches the sender
    let should_remove = us.registry.get(&agent_id).is_some_and(|e| {
        e.is_remote()
            && us
                .hosts
                .get(&e.host_id)
                .is_some_and(|host| matches!(host.route.peek(), Some(link) if *link == ctx.link))
    });

    if should_remove {
        us.registry.remove(&agent_id);
        tracing::info!(agent_id = %agent_id, "withdrew remote agent");

        // Propagate to other peers
        broadcast_to_peers(
            &mut us,
            &DirectMessage::WithdrawAgent { agent_id },
            Some(&ctx.link),
        );
    } else {
        tracing::debug!(agent_id = %agent_id, "ignoring withdraw (link mismatch)");
    }

    Ok(())
}

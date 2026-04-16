use std::sync::Arc;

use tokio::sync::{RwLock, oneshot};
use tokio::time::Instant;
use uuid::Uuid;

use super::super::{ServerUserState, SubscriptionEntry, SubscriptionMode};
use crate::protocol::message::SubscriptionId;
use crate::protocol::route::Route;

/// Register a subscription entry in active_subscriptions.
pub(in crate::server) fn register_subscription(
    us: &mut ServerUserState,
    subscription_id: SubscriptionId,
    agent_id: Uuid,
    mode: SubscriptionMode,
    cancel_tx: oneshot::Sender<()>,
    dst: Route,
    lease_deadline: Instant,
) {
    us.active_subscriptions.insert(
        subscription_id,
        SubscriptionEntry {
            subscription_id,
            agent_id,
            mode,
            cancel: cancel_tx,
            dst,
            lease_deadline,
        },
    );
}

/// Remove a subscription entry after the task exits.
pub(in crate::server) async fn cleanup_subscription(
    user_state: &Arc<RwLock<ServerUserState>>,
    subscription_id: SubscriptionId,
) -> Option<SubscriptionEntry> {
    let removed = user_state
        .write()
        .await
        .active_subscriptions
        .remove(&subscription_id);
    tracing::trace!(subscription_id = %subscription_id, "subscription cleaned up");
    removed
}

/// Push a subscription deadline out. Returns the owning agent_id when found.
pub(in crate::server) async fn extend_subscription(
    user_state: &Arc<RwLock<ServerUserState>>,
    subscription_id: SubscriptionId,
    lease_deadline: Instant,
) -> Option<Uuid> {
    let mut us = user_state.write().await;
    let entry = us.active_subscriptions.get_mut(&subscription_id)?;
    entry.lease_deadline = lease_deadline;
    Some(entry.agent_id)
}

/// Explicitly remove a subscription and cancel its stream task.
pub(in crate::server) async fn unsubscribe_subscription(
    user_state: &Arc<RwLock<ServerUserState>>,
    subscription_id: SubscriptionId,
) -> Option<SubscriptionEntry> {
    cleanup_subscription(user_state, subscription_id).await
}

/// Cancel all active subscriptions matching a predicate.
pub(in crate::server) fn cancel_subscriptions_matching(
    us: &mut ServerUserState,
    predicate: impl Fn(&SubscriptionEntry) -> bool,
) -> Vec<SubscriptionEntry> {
    let cancelled_ids: Vec<_> = us
        .active_subscriptions
        .iter()
        .filter_map(|(subscription_id, entry)| predicate(entry).then_some(*subscription_id))
        .collect();

    cancelled_ids
        .into_iter()
        .filter_map(|subscription_id| us.active_subscriptions.remove(&subscription_id))
        .collect()
}

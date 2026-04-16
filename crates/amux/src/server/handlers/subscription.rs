use std::future::Future;
use std::sync::atomic::Ordering;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::Instrument;
use uuid::Uuid;

use crate::buffer::{BroadcastReader, BufferPolicy};
use crate::protocol::message::{
    Message, ProtocolError, RoutableMessage, SubscriptionCloseReason, SubscriptionId,
};
use crate::protocol::route::Route;
use crate::server::connection::{
    ConnectionContext, cleanup_subscription, register_subscription, unsubscribe_subscription,
};
use crate::server::routing::{SubscribeError, connection_tx};
use crate::server::{SUBSCRIPTION_LEASE_DURATION, SubscriptionMode};

/// A readable subscription stream. Implemented for all [`BroadcastReader`]
/// instantiations to enable shared stream spawning logic.
pub(super) trait SubscriptionReader: Send + 'static {
    type Item: Send;
    fn recv(&mut self) -> impl Future<Output = Option<Self::Item>> + Send;
}

impl<P: BufferPolicy> SubscriptionReader for BroadcastReader<P> {
    type Item = P::Item;

    fn recv(&mut self) -> impl Future<Output = Option<P::Item>> + Send {
        self.read()
    }
}

pub(super) struct SubscriptionHandle {
    pub(super) subscription_id: SubscriptionId,
    pub(super) agent_id: Uuid,
    pub(super) mode: SubscriptionMode,
    pub(super) reply_src: Route,
    pub(super) reply_dst: Route,
}

impl SubscriptionHandle {
    pub(super) async fn register(
        ctx: &ConnectionContext,
        subscription_id: SubscriptionId,
        agent_id: Uuid,
        mode: SubscriptionMode,
        reply_src: Route,
        reply_dst: Route,
    ) -> (Self, oneshot::Receiver<()>) {
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let mut full_dst = reply_dst.clone();
        full_dst.push(ctx.link.clone());
        let mut us = ctx.user_state.write().await;
        register_subscription(
            &mut us,
            subscription_id,
            agent_id,
            mode,
            cancel_tx,
            full_dst,
            Instant::now() + SUBSCRIPTION_LEASE_DURATION,
        );
        (
            Self {
                subscription_id,
                agent_id,
                mode,
                reply_src,
                reply_dst,
            },
            cancel_rx,
        )
    }

    fn stream_span(&self) -> tracing::Span {
        tracing::info_span!(
            "stream",
            subscription_id = %self.subscription_id,
            agent_id = %self.agent_id,
            mode = self.mode.as_str()
        )
    }

    async fn send_source_closed(&self, tx: &mpsc::Sender<Message>, request_id: u64) {
        let _ = tx
            .send(Message::routable(
                self.reply_src.clone(),
                self.reply_dst.clone(),
                request_id,
                &RoutableMessage::SubscriptionClosed {
                    subscription_id: self.subscription_id,
                    reason: SubscriptionCloseReason::SourceClosed,
                },
            ))
            .await;
    }
}

/// Spawn a subscription stream task that reads from a buffer, wraps each item
/// into a RoutableMessage, and forwards it to the subscriber. Handles cancellation
/// and cleanup automatically.
pub(super) async fn spawn_subscription_stream<R: SubscriptionReader>(
    mut reader: R,
    handle: SubscriptionHandle,
    cancel_rx: oneshot::Receiver<()>,
    wrap_item: fn(SubscriptionId, R::Item) -> RoutableMessage,
    ctx: &ConnectionContext,
) {
    let Some(tx) = connection_tx(&ctx.user_state, &ctx.link).await else {
        let _ = cleanup_subscription(&ctx.user_state, handle.subscription_id).await;
        return;
    };

    let stream_span = handle.stream_span();
    let stream_user_state = ctx.user_state.clone();
    let next_rid = ctx.next_request_id.clone();
    tokio::spawn(
        async move {
            let subscription_id = handle.subscription_id;
            tokio::select! {
                source_closed = async {
                    while let Some(item) = reader.recv().await {
                        let rid = next_rid.fetch_add(1, Ordering::Relaxed);
                        if tx
                            .send(Message::routable(
                                handle.reply_src.clone(),
                                handle.reply_dst.clone(),
                                rid,
                                &wrap_item(subscription_id, item),
                            ))
                            .await
                            .is_err()
                        {
                            return false;
                        }
                    }
                    true
                } => {
                    if source_closed {
                        if cleanup_subscription(&stream_user_state, subscription_id)
                            .await
                            .is_some()
                        {
                            let rid = next_rid.fetch_add(1, Ordering::Relaxed);
                            handle.send_source_closed(&tx, rid).await;
                        }
                    } else {
                        let _ = cleanup_subscription(&stream_user_state, subscription_id).await;
                    }
                }
                _ = cancel_rx => {
                    tracing::debug!("stream cancelled");
                    let _ = cleanup_subscription(&stream_user_state, subscription_id).await;
                }
            }
            tracing::debug!("stream ended");
        }
        .instrument(stream_span),
    );
}

pub(super) fn subscribe_error(err: &SubscribeError) -> ProtocolError {
    match err {
        SubscribeError::AgentNotFound(_) => ProtocolError::NoAgentFound,
        _ => ProtocolError::ServerError {
            message: err.to_string(),
        },
    }
}

pub(super) async fn remove_subscription_if_reply_failed(
    user_state: &std::sync::Arc<tokio::sync::RwLock<crate::server::ServerUserState>>,
    subscription_id: SubscriptionId,
) {
    let _ = unsubscribe_subscription(user_state, subscription_id).await;
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};
    use tokio::time::Instant;
    use uuid::Uuid;

    use super::super::tests::*;
    use super::*;
    use crate::agent::AgentSession;
    use crate::protocol::link::Link;
    use crate::protocol::message::{DirectMessage, Message};
    use crate::protocol::route::Route;
    use crate::protocol::{RoutableMessage, SubscriptionCloseReason};
    use crate::server::connection::cancel_subscriptions_matching;
    use crate::server::routing::withdraw_agent;
    use crate::server::{
        ConnectionHandle, SUBSCRIPTION_LEASE_DURATION, SubscriptionMode,
        sweep_expired_subscriptions,
    };

    struct MockReader {
        rx: mpsc::Receiver<Vec<u8>>,
    }

    impl SubscriptionReader for MockReader {
        type Item = Vec<u8>;

        fn recv(&mut self) -> impl Future<Output = Option<Vec<u8>>> + Send {
            self.rx.recv()
        }
    }

    async fn register_test_subscription(
        ctx: &ConnectionContext,
        agent_id: Uuid,
        subscription_id: SubscriptionId,
        reply_src: Route,
        reply_dst: &Route,
        mode: SubscriptionMode,
    ) -> (SubscriptionHandle, oneshot::Receiver<()>) {
        SubscriptionHandle::register(
            ctx,
            subscription_id,
            agent_id,
            mode,
            reply_src,
            reply_dst.clone(),
        )
        .await
    }

    #[tokio::test]
    async fn stream_source_eof_sends_source_closed_and_cleans_up_subscription() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = SubscriptionId::random();
        let reply_dst = Route::from_link(Link::new("client").unwrap());
        let (handle, cancel_rx) = register_test_subscription(
            &ctx,
            agent_id,
            subscription_id,
            Route::from_link(Link::new("server").unwrap()),
            &reply_dst,
            SubscriptionMode::Raw,
        )
        .await;
        let (mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        mock_tx.send(b"hello".to_vec()).await.unwrap();
        mock_tx.send(b"world".to_vec()).await.unwrap();
        drop(mock_tx);

        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::RawOutput { subscription_id: id, data }
                if id == subscription_id && data == b"hello"
        ));
        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::RawOutput { subscription_id: id, data }
                if id == subscription_id && data == b"world"
        ));
        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::SourceClosed,
            } if id == subscription_id
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;
        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }

    #[tokio::test]
    async fn withdrawn_agent_stream_still_sends_source_closed_on_eof() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let session = AgentSession::new_readonly_claude(agent_id, PathBuf::from("/tmp"));
        let info = session.to_agent(Uuid::new_v4());
        {
            let mut us = user_state.write().await;
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let subscription_id = SubscriptionId::random();
        let reply_dst = Route::from_link(Link::new("client").unwrap());
        let (handle, cancel_rx) = register_test_subscription(
            &ctx,
            agent_id,
            subscription_id,
            Route::from_link(Link::new("server").unwrap()),
            &reply_dst,
            SubscriptionMode::Raw,
        )
        .await;
        let (mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        {
            let mut us = user_state.write().await;
            let removed = withdraw_agent(&mut us, agent_id);
            assert!(
                removed.is_some(),
                "withdrawal should return the removed session"
            );
            assert!(
                us.active_subscriptions.contains_key(&subscription_id),
                "active subscription should remain registered until EOF"
            );
        }

        drop(mock_tx);

        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::SourceClosed,
            } if id == subscription_id
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;
        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }

    #[tokio::test]
    async fn stream_cancelled_stops_without_subscription_closed() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = SubscriptionId::random();
        let reply_dst = Route::from_link(Link::new("client").unwrap());
        let (handle, cancel_rx) = register_test_subscription(
            &ctx,
            agent_id,
            subscription_id,
            Route::from_link(Link::new("server").unwrap()),
            &reply_dst,
            SubscriptionMode::Raw,
        )
        .await;
        let (_mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        {
            let mut us = user_state.write().await;
            let cancelled = cancel_subscriptions_matching(&mut us, |_| true);
            assert_eq!(cancelled.len(), 1);
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            route_rx.try_recv().is_err(),
            "cancelled stream should not send synthetic SubscriptionClosed"
        );
    }

    #[tokio::test]
    async fn stream_no_route_skips_spawn() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());

        let agent_id = Uuid::new_v4();
        let subscription_id = SubscriptionId::random();
        let (_cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let (_mock_tx, mock_rx) = mpsc::channel::<Vec<u8>>(16);
        let handle = SubscriptionHandle {
            subscription_id,
            agent_id,
            mode: SubscriptionMode::Raw,
            reply_src: Route::from_link(Link::new("server").unwrap()),
            reply_dst: Route::from_link(Link::new("client").unwrap()),
        };

        spawn_subscription_stream(
            MockReader { rx: mock_rx },
            handle,
            cancel_rx,
            |subscription_id, data| RoutableMessage::RawOutput {
                subscription_id,
                data,
            },
            &ctx,
        )
        .await;

        let us = user_state.read().await;
        assert!(
            !us.active_subscriptions.contains_key(&subscription_id),
            "no subscription should be registered when route doesn't exist"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lease_expiry_removes_subscription_and_emits_closed() {
        let (state, user_state) = test_state().await;
        let mut route_rx = setup_route(&user_state).await;

        let agent_id = Uuid::new_v4();
        let subscription_id = SubscriptionId::random();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link(Link::new("client").unwrap());
        full_dst.push(Link::new("test-link").unwrap());

        {
            let mut us = user_state.write().await;
            crate::server::connection::register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        tokio::time::advance(SUBSCRIPTION_LEASE_DURATION + Duration::from_secs(1)).await;
        sweep_expired_subscriptions(&state).await;

        assert!(
            cancel_rx.try_recv().is_err(),
            "lease expiry should drop the cancellation sender"
        );
        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::LeaseExpired,
            } if id == subscription_id
        ));

        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }

    #[tokio::test]
    async fn lease_expiry_does_not_block_on_full_route_channel() {
        let (state, user_state) = test_state().await;

        let (route_tx, _route_rx) = mpsc::channel::<Message>(1);
        route_tx
            .try_send(Message::Direct {
                message: DirectMessage::InitialSyncComplete,
            })
            .unwrap();
        {
            let mut us = user_state.write().await;
            us.routes.insert(
                Link::new("test-link").unwrap(),
                ConnectionHandle::new(route_tx, Arc::new(AtomicU64::new(1))),
            );
        }

        let agent_id = Uuid::new_v4();
        let subscription_id = SubscriptionId::random();
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link(Link::new("client").unwrap());
        full_dst.push(Link::new("test-link").unwrap());

        {
            let mut us = user_state.write().await;
            crate::server::connection::register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now(),
            );
        }

        tokio::time::timeout(
            Duration::from_millis(50),
            sweep_expired_subscriptions(&state),
        )
        .await
        .expect("sweep should not block on a full route channel");

        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }

    #[tokio::test(start_paused = true)]
    async fn owner_lease_expiry_cleans_up_remote_shaped_subscription_without_intermediate_state() {
        let (state, user_state) = test_state().await;
        let mut route_rx = setup_named_route(&user_state, "peer-hop").await;

        let agent_id = Uuid::new_v4();
        let subscription_id = SubscriptionId::random();
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let mut full_dst = Route::from_link(Link::new("client-link").unwrap());
        full_dst.push(Link::new("peer-hop").unwrap());

        {
            let mut us = user_state.write().await;
            crate::server::connection::register_subscription(
                &mut us,
                subscription_id,
                agent_id,
                SubscriptionMode::Raw,
                cancel_tx,
                full_dst,
                Instant::now() + SUBSCRIPTION_LEASE_DURATION,
            );
        }

        tokio::time::advance(SUBSCRIPTION_LEASE_DURATION + Duration::from_secs(1)).await;
        sweep_expired_subscriptions(&state).await;

        assert!(matches!(
            recv_routable(&mut route_rx).await,
            RoutableMessage::SubscriptionClosed {
                subscription_id: id,
                reason: SubscriptionCloseReason::LeaseExpired,
            } if id == subscription_id
        ));
        let us = user_state.read().await;
        assert!(!us.active_subscriptions.contains_key(&subscription_id));
    }
}

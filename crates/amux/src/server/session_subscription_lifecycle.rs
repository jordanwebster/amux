use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use super::RpcDispatcher;
use super::state::ServerUserState;
use crate::protocol::message::{CallId, ProtocolError};
use crate::protocol::method;
use crate::protocol::route::Route;
use crate::rpc::{InboundCallState, RpcCallCancellation, RpcInboundCallHandle};
use crate::server::{InboundCallResources, RpcInboundClosing, ServerStreamSendError};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::server) struct SessionSubscriptionKey {
    pub(crate) counterparty: Route,
    pub(crate) call_id: CallId,
}

impl SessionSubscriptionKey {
    pub(in crate::server) fn new(counterparty: Route, call_id: CallId) -> Self {
        Self {
            counterparty,
            call_id,
        }
    }
}

#[derive(Debug, Clone)]
struct SessionSubscriptionRuntimeHandle {
    call: RpcInboundCallHandle,
    counterparty: Route,
}

impl SessionSubscriptionRuntimeHandle {
    fn new(call: RpcInboundCallHandle, counterparty: Route) -> Option<Self> {
        (call.method == method::AGENT_SUBSCRIBE_SESSION).then_some(Self { call, counterparty })
    }

    fn call(&self) -> &RpcInboundCallHandle {
        &self.call
    }

    fn counterparty(&self) -> &Route {
        &self.counterparty
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SessionSubscriptionRuntime {
    handle: SessionSubscriptionRuntimeHandle,
    cancellation: RpcCallCancellation,
    rpc: RpcDispatcher,
}

impl SessionSubscriptionRuntime {
    pub(crate) fn new(
        call: RpcInboundCallHandle,
        counterparty_route: Route,
        cancellation: RpcCallCancellation,
        rpc: RpcDispatcher,
    ) -> Option<Self> {
        Some(Self {
            handle: SessionSubscriptionRuntimeHandle::new(call, counterparty_route)?,
            cancellation,
            rpc,
        })
    }

    pub(crate) fn call_id(&self) -> &CallId {
        &self.handle.call.call_id
    }

    pub(crate) fn counterparty(&self) -> &Route {
        self.handle.counterparty()
    }

    pub(crate) async fn is_active(&self, user_state: &Arc<RwLock<ServerUserState>>) -> bool {
        let _ = user_state;
        self.rpc
            .inbound_call_is_active_for_handle(self.handle.call())
    }

    pub(crate) fn activate(&self) -> bool {
        self.rpc.activate_inbound_for_handle(self.handle.call())
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub(crate) async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }

    pub(crate) async fn terminate(
        &self,
        user_state: &Arc<RwLock<ServerUserState>>,
        error: ProtocolError,
    ) -> Result<(), ServerStreamSendError> {
        let closing = self
            .rpc
            .begin_inbound_closing_for_handle_if(self.handle.call(), |_, _| true)
            .and_then(|closing| {
                session_subscription_closing_from_rpc_closing(self.rpc.clone(), closing)
            });

        let Some(closing) = closing else {
            return Ok(());
        };

        send_terminal_and_finish_session_subscription(user_state, closing, Err(error)).await
    }

    pub(crate) async fn finish_output_source(
        &self,
        user_state: &Arc<RwLock<ServerUserState>>,
        source_result: Result<bool, ProtocolError>,
    ) -> Result<(), ServerStreamSendError> {
        let closing = self
            .rpc
            .begin_inbound_closing_for_handle_if(self.handle.call(), |_, _| true)
            .and_then(|closing| {
                session_subscription_closing_from_rpc_closing(self.rpc.clone(), closing)
            });

        let Some(closing) = closing else {
            return Ok(());
        };

        match source_result {
            Ok(true) => {
                send_terminal_and_finish_session_subscription(user_state, closing, Ok(())).await
            }
            Ok(false) => {
                finish_session_subscription_closing_silently(user_state, closing).await;
                Ok(())
            }
            Err(error) => {
                send_terminal_and_finish_session_subscription(user_state, closing, Err(error)).await
            }
        }
    }
}

pub(crate) struct SessionSubscriptionClosing {
    pub(crate) call: RpcInboundClosing,
    rpc: RpcDispatcher,
}

#[cfg(test)]
pub(crate) enum SessionSubscriptionCloseTarget {
    Absent,
    Closing,
    AlreadyClosing,
}

pub(in crate::server) struct SessionSubscriptionCleanupFinish {
    call: RpcInboundClosing,
    rpc: RpcDispatcher,
}

#[cfg(test)]
fn session_subscription_resources(
    us: &ServerUserState,
    key: &SessionSubscriptionKey,
) -> Option<InboundCallResources> {
    let rpc = us.route_rpc(&key.counterparty)?;
    rpc.inbound_for_call(&key.call_id)
        .filter(|call| call.method == method::AGENT_SUBSCRIBE_SESSION)?;
    rpc.inbound_resources_for_call(&key.call_id)
}

#[cfg(test)]
pub(crate) fn session_subscription_active_resources(
    us: &ServerUserState,
    key: &SessionSubscriptionKey,
) -> Option<InboundCallResources> {
    let rpc = us.route_rpc(&key.counterparty)?;
    rpc.inbound_for_call(&key.call_id).filter(|call| {
        call.method == method::AGENT_SUBSCRIBE_SESSION
            && matches!(call.state, InboundCallState::Active)
    })?;
    rpc.inbound_resources_for_call(&key.call_id)
}

#[cfg(test)]
fn session_subscription_call_is_closing(
    us: &ServerUserState,
    key: &SessionSubscriptionKey,
) -> bool {
    us.route_rpc(&key.counterparty).is_some_and(|rpc| {
        rpc.inbound_for_call(&key.call_id).is_some_and(|call| {
            call.method == method::AGENT_SUBSCRIBE_SESSION
                && matches!(call.state, InboundCallState::Closing)
        })
    })
}

#[cfg(test)]
fn begin_session_subscription_closing_if(
    us: &mut ServerUserState,
    key: &SessionSubscriptionKey,
    predicate: impl FnOnce(&InboundCallResources) -> bool,
) -> Option<SessionSubscriptionClosing> {
    let rpc = us.route_rpc(&key.counterparty)?;
    let call = us
        .route_rpc(&key.counterparty)?
        .begin_inbound_closing_for_call_if(&key.call_id, |call, resources| {
            call.method == method::AGENT_SUBSCRIBE_SESSION
                && matches!(call.state, InboundCallState::Active)
                && predicate(resources)
        })?;

    Some(SessionSubscriptionClosing { call, rpc })
}

fn session_subscription_counterparty_route(
    us: &ServerUserState,
    call: &crate::rpc::InboundCall,
    fallback: &Route,
) -> Route {
    us.session_subscriptions
        .get(&call.call_id)
        .map(|subscription| subscription.counterparty.clone())
        .unwrap_or_else(|| fallback.clone())
}

pub(crate) fn begin_session_subscriptions_closing_for_agent(
    us: &mut ServerUserState,
    agent_id: Uuid,
) -> Vec<SessionSubscriptionClosing> {
    let call_ids: std::collections::HashSet<_> = us
        .session_subscriptions
        .iter()
        .filter_map(|(call_id, subscription)| {
            (subscription.agent_id == agent_id).then_some(call_id.clone())
        })
        .collect();
    let mut closings = Vec::new();
    for (_, rpc) in us.rpc_contexts_sorted() {
        closings.extend(
            rpc.begin_inbound_closing_calls_if(|call, _| {
                call.method == method::AGENT_SUBSCRIBE_SESSION
                    && matches!(call.state, InboundCallState::Active)
                    && call_ids.contains(&call.call_id)
            })
            .into_iter()
            .filter_map(|closing| {
                session_subscription_closing_from_rpc_closing(rpc.clone(), closing)
            }),
        );
    }
    closings
}

pub(in crate::server) fn session_subscription_closing_from_rpc_closing(
    rpc: RpcDispatcher,
    call: RpcInboundClosing,
) -> Option<SessionSubscriptionClosing> {
    (call.handle.method == method::AGENT_SUBSCRIBE_SESSION)
        .then_some(SessionSubscriptionClosing { call, rpc })
}

#[cfg(test)]
pub(crate) fn begin_session_subscription_closing_or_absent(
    us: &mut ServerUserState,
    key: &SessionSubscriptionKey,
) -> SessionSubscriptionCloseTarget {
    if begin_session_subscription_closing_if(us, key, |_| true).is_some() {
        SessionSubscriptionCloseTarget::Closing
    } else if session_subscription_call_is_closing(us, key) {
        SessionSubscriptionCloseTarget::AlreadyClosing
    } else {
        SessionSubscriptionCloseTarget::Absent
    }
}

pub(crate) async fn send_terminal_and_finish_session_subscription(
    user_state: &Arc<RwLock<ServerUserState>>,
    closing: SessionSubscriptionClosing,
    terminal: Result<(), ProtocolError>,
) -> Result<(), ServerStreamSendError> {
    let result = closing.call.send_empty_response_result(terminal).await;

    user_state
        .write()
        .await
        .session_subscriptions
        .remove(&closing.call.handle.call_id);
    closing.rpc.finish_inbound_closing(&closing.call);

    result
}

pub(crate) async fn finish_session_subscriptions_with_error(
    user_state: &Arc<RwLock<ServerUserState>>,
    closings: Vec<SessionSubscriptionClosing>,
    error: ProtocolError,
) {
    for closing in closings {
        let _ =
            send_terminal_and_finish_session_subscription(user_state, closing, Err(error.clone()))
                .await;
    }
}

async fn finish_session_subscription_closing_silently(
    user_state: &Arc<RwLock<ServerUserState>>,
    closing: SessionSubscriptionClosing,
) {
    user_state
        .write()
        .await
        .session_subscriptions
        .remove(&closing.call.handle.call_id);
    closing.rpc.finish_inbound_closing(&closing.call);
}

pub(crate) async fn finish_session_subscription_closing_after_output_flush(
    user_state: &Arc<RwLock<ServerUserState>>,
    call: RpcInboundClosing,
    rpc: RpcDispatcher,
) {
    call.with_send_gate(|| async {
        user_state
            .write()
            .await
            .session_subscriptions
            .remove(&call.handle.call_id);
        rpc.finish_inbound_closing(&call);
    })
    .await;
}

pub(in crate::server) async fn finish_session_subscription_cleanup_jobs(
    user_state: &Arc<RwLock<ServerUserState>>,
    jobs: Vec<SessionSubscriptionCleanupFinish>,
) {
    for job in jobs {
        finish_session_subscription_closing_after_output_flush(user_state, job.call, job.rpc).await;
    }
}

/// Cancel all active or starting SubscribeSession calls matching a predicate.
///
/// SubscribeSession calls are enumerated and moved to `Closing` in `RpcState`.
/// The generic RPC call cancellation signal asks the service task to stop its
/// domain work; cleanup jobs only wait for the per-call send gate and remove the
/// RPC call once any in-flight output send is clear.
pub(in crate::server) fn cancel_session_subscriptions_matching(
    us: &mut ServerUserState,
    predicate: impl Fn(&SessionSubscriptionKey, &InboundCallResources) -> bool,
) -> (usize, Vec<SessionSubscriptionCleanupFinish>) {
    let mut finish_jobs = Vec::new();
    for (route, rpc) in us.rpc_contexts_sorted() {
        let closings = rpc.begin_inbound_closing_calls_if(|call, resources| {
            if call.method != method::AGENT_SUBSCRIBE_SESSION
                || !matches!(
                    call.state,
                    InboundCallState::Starting | InboundCallState::Active
                )
            {
                return false;
            }

            let key = SessionSubscriptionKey::new(
                session_subscription_counterparty_route(us, call, &route),
                call.call_id.clone(),
            );
            predicate(&key, resources)
        });
        finish_jobs.extend(
            closings
                .into_iter()
                .filter_map(|closing| {
                    session_subscription_closing_from_rpc_closing(rpc.clone(), closing)
                })
                .map(|closing| SessionSubscriptionCleanupFinish {
                    call: closing.call,
                    rpc: closing.rpc,
                }),
        );
    }

    (finish_jobs.len(), finish_jobs)
}

pub(in crate::server) fn cancel_session_subscriptions_for_closed_link(
    us: &mut ServerUserState,
    closed_link: &crate::protocol::Link,
) -> (usize, Vec<SessionSubscriptionCleanupFinish>) {
    cancel_session_subscriptions_matching(us, |key, resources| {
        resources.owner_link == *closed_link || key.counterparty.contains_link(closed_link.as_str())
    })
}

pub(in crate::server) fn cancel_session_subscriptions_for_owner_link(
    us: &mut ServerUserState,
    owner_link: &crate::protocol::Link,
) -> (usize, Vec<SessionSubscriptionCleanupFinish>) {
    cancel_session_subscriptions_matching(us, |_, resources| resources.owner_link == *owner_link)
}

pub(in crate::server) fn cancel_session_subscriptions_for_route_prefix(
    us: &mut ServerUserState,
    route_prefix: &Route,
) -> (usize, Vec<SessionSubscriptionCleanupFinish>) {
    cancel_session_subscriptions_matching(us, |key, _| {
        key.counterparty.starts_with_route(route_prefix)
    })
}

pub(in crate::server) fn cancel_session_subscription_for_route_and_call(
    us: &mut ServerUserState,
    counterparty: &Route,
    call_id: &CallId,
) -> (usize, Vec<SessionSubscriptionCleanupFinish>) {
    cancel_session_subscriptions_matching(us, |key, _| {
        key.call_id == *call_id && key.counterparty == *counterparty
    })
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::protocol::link::Link;
    use crate::protocol::message::CallId;
    use crate::protocol::route::Route;
    use crate::server::EndpointServerStreamStart;

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn route_stack(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    fn call_id(n: u128) -> CallId {
        CallId::from(Uuid::from_u128(n))
    }

    fn register_call(
        us: &mut ServerUserState,
        key: &SessionSubscriptionKey,
        owner_link: Link,
    ) -> Uuid {
        let (tx, _rx) = mpsc::channel(1);
        let stream = us
            .ensure_route_rpc(key.counterparty.clone())
            .register_endpoint_server_stream(EndpointServerStreamStart {
                tx,
                owner_link,
                reply_src: route("server"),
                reply_dst: route("client"),
                call_id: key.call_id.clone(),
                method: method::AGENT_SUBSCRIBE_SESSION,
                dedup_key: None,
            })
            .unwrap();
        us.ensure_route_rpc(key.counterparty.clone())
            .activate_inbound_for_handle(&stream.handle);
        stream.handle.generation
    }

    fn register_starting_call(
        us: &mut ServerUserState,
        key: &SessionSubscriptionKey,
        owner_link: Link,
    ) -> Uuid {
        let (tx, _rx) = mpsc::channel(1);
        let stream = us
            .ensure_route_rpc(key.counterparty.clone())
            .register_endpoint_server_stream(EndpointServerStreamStart {
                tx,
                owner_link,
                reply_src: route("server"),
                reply_dst: route("client"),
                call_id: key.call_id.clone(),
                method: method::AGENT_SUBSCRIBE_SESSION,
                dedup_key: None,
            })
            .unwrap();
        stream.handle.generation
    }

    #[test]
    fn active_resources_ignore_closing_call() {
        let mut us = ServerUserState::new();
        let key = SessionSubscriptionKey::new(route("client"), call_id(42));
        let generation = register_call(&mut us, &key, Link::new("owner").unwrap());

        let closing = begin_session_subscription_closing_if(&mut us, &key, |_| true)
            .expect("active call should begin closing");

        assert_eq!(closing.call.handle.generation, generation);
        assert!(session_subscription_active_resources(&us, &key).is_none());
        assert!(session_subscription_resources(&us, &key).is_some());
    }

    #[test]
    fn already_closing_call_is_not_absent() {
        let mut us = ServerUserState::new();
        let key = SessionSubscriptionKey::new(route("client"), call_id(42));
        register_call(&mut us, &key, Link::new("owner").unwrap());

        assert!(matches!(
            begin_session_subscription_closing_or_absent(&mut us, &key),
            SessionSubscriptionCloseTarget::Closing
        ));
        assert!(matches!(
            begin_session_subscription_closing_or_absent(&mut us, &key),
            SessionSubscriptionCloseTarget::AlreadyClosing
        ));
    }

    #[tokio::test]
    async fn cleanup_matching_link_cancels_and_finishes_session_subscription_call() {
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let key = SessionSubscriptionKey::new(route("client"), call_id(42));
        let owner_link = Link::new("owner").unwrap();

        let jobs = {
            let mut us = user_state.write().await;
            register_call(&mut us, &key, owner_link.clone());

            let (cancelled, jobs) =
                cancel_session_subscriptions_matching(&mut us, |_, resources| {
                    resources.owner_link == owner_link
                });

            assert_eq!(cancelled, 1);
            assert_eq!(jobs.len(), 1);
            assert!(matches!(
                us.route_rpc(&key.counterparty)
                    .unwrap()
                    .inbound_for_call(&key.call_id),
                Some(call)
                    if call.method == method::AGENT_SUBSCRIBE_SESSION
                        && matches!(call.state, InboundCallState::Closing)
            ));
            jobs
        };

        finish_session_subscription_cleanup_jobs(&user_state, jobs).await;

        let us = user_state.read().await;
        assert!(
            us.route_rpc(&key.counterparty)
                .unwrap()
                .inbound_for_call(&key.call_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn cleanup_matching_link_cancels_starting_session_subscription_call() {
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let key = SessionSubscriptionKey::new(route("client"), call_id(43));
        let owner_link = Link::new("owner").unwrap();

        let jobs = {
            let mut us = user_state.write().await;
            register_starting_call(&mut us, &key, owner_link.clone());

            let (cancelled, jobs) =
                cancel_session_subscriptions_matching(&mut us, |_, resources| {
                    resources.owner_link == owner_link
                });

            assert_eq!(cancelled, 1);
            assert_eq!(jobs.len(), 1);
            assert!(matches!(
                us.route_rpc(&key.counterparty)
                    .unwrap()
                    .inbound_for_call(&key.call_id),
                Some(call)
                    if call.method == method::AGENT_SUBSCRIBE_SESSION
                        && matches!(call.state, InboundCallState::Closing)
            ));
            jobs
        };

        finish_session_subscription_cleanup_jobs(&user_state, jobs).await;

        let us = user_state.read().await;
        assert!(
            us.route_rpc(&key.counterparty)
                .unwrap()
                .inbound_for_call(&key.call_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn route_and_call_cleanup_uses_exact_subscription_counterparty() {
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let route_context = route("peer");
        let exact_counterparty = route_stack(&["peer", "client"]);
        let key = SessionSubscriptionKey::new(route_context.clone(), call_id(44));
        let owner_link = Link::new("owner").unwrap();

        let jobs = {
            let mut us = user_state.write().await;
            register_call(&mut us, &key, owner_link);
            us.session_subscriptions.insert(
                key.call_id.clone(),
                crate::server::SessionSubscriptionState {
                    agent_id: Uuid::new_v4(),
                    counterparty: exact_counterparty.clone(),
                },
            );

            let (cancelled, jobs) = cancel_session_subscription_for_route_and_call(
                &mut us,
                &exact_counterparty,
                &key.call_id,
            );

            assert_eq!(cancelled, 1);
            assert_eq!(jobs.len(), 1);
            jobs
        };

        finish_session_subscription_cleanup_jobs(&user_state, jobs).await;

        let us = user_state.read().await;
        assert!(
            us.route_rpc(&route_context)
                .unwrap()
                .inbound_for_call(&key.call_id)
                .is_none()
        );
    }
}

use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use super::RpcDispatcher;
use super::state::ServerUserState;
use crate::protocol::message::{CallId, ProtocolError};
use crate::protocol::method;
use crate::protocol::route::Route;
use crate::rpc::{
    DedupKey, InboundCallResources, InboundCallState, RegisterCallError, RpcCallCancellation,
    RpcInboundCallHandle, RpcInboundClosing, RpcRoutedSendError,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::server) struct OpenSessionKey {
    pub(crate) counterparty: Route,
    pub(crate) call_id: CallId,
}

impl OpenSessionKey {
    pub(in crate::server) fn new(counterparty: Route, call_id: CallId) -> Self {
        Self {
            counterparty,
            call_id,
        }
    }
}

pub(crate) struct OpenSessionStructuredInputPayload {
    pub(crate) client_seq: u64,
    pub(crate) payload: serde_json::Value,
}

pub(crate) struct OpenSessionStructuredInputJob {
    pub(crate) input_id: Vec<u8>,
    pub(crate) input: Result<OpenSessionStructuredInputPayload, ProtocolError>,
}

#[derive(Clone)]
pub(crate) struct OpenSessionStructuredInput {
    pub(crate) tx: mpsc::Sender<OpenSessionStructuredInputJob>,
}

impl OpenSessionStructuredInput {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<OpenSessionStructuredInputJob>) {
        let (tx, rx) = mpsc::channel(256);
        (Self { tx }, rx)
    }
}

#[derive(Debug, Clone)]
struct OpenSessionRuntimeHandle {
    call: RpcInboundCallHandle,
    counterparty_route: Route,
}

impl OpenSessionRuntimeHandle {
    fn new(call: RpcInboundCallHandle, counterparty_route: Route) -> Option<Self> {
        (call.method == method::AGENT_OPEN_SESSION).then_some(Self {
            call,
            counterparty_route,
        })
    }

    fn call(&self) -> &RpcInboundCallHandle {
        &self.call
    }

    fn counterparty_route(&self) -> &Route {
        &self.counterparty_route
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OpenSessionRuntime {
    handle: OpenSessionRuntimeHandle,
    cancellation: RpcCallCancellation,
    rpc: RpcDispatcher,
}

impl OpenSessionRuntime {
    pub(crate) fn new(
        call: RpcInboundCallHandle,
        counterparty_route: Route,
        cancellation: RpcCallCancellation,
        rpc: RpcDispatcher,
    ) -> Option<Self> {
        Some(Self {
            handle: OpenSessionRuntimeHandle::new(call, counterparty_route)?,
            cancellation,
            rpc,
        })
    }

    pub(crate) async fn is_active(&self, user_state: &Arc<RwLock<ServerUserState>>) -> bool {
        let _ = user_state;
        self.rpc
            .inbound_call_is_active_for_handle(self.handle.call())
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
    ) -> Result<(), RpcRoutedSendError> {
        let closing = self
            .rpc
            .begin_inbound_closing_for_handle_if(self.handle.call(), |_, _| true)
            .and_then(|closing| open_session_closing_from_rpc_closing(self.rpc.clone(), closing));

        let Some(closing) = closing else {
            return Ok(());
        };

        send_terminal_and_finish_open_session(user_state, closing, Err(error)).await
    }

    pub(crate) async fn reserve_dedup_for_agent(
        &self,
        user_state: &Arc<RwLock<ServerUserState>>,
        agent_id: Uuid,
    ) -> Result<bool, RegisterCallError> {
        let _ = user_state;
        self.rpc.reserve_inbound_dedup_for_handle(
            self.handle.call(),
            DedupKey::OpenSession {
                counterparty_route: self.handle.counterparty_route().clone(),
                agent_id,
            },
        )
    }

    pub(crate) async fn finish_output_source(
        &self,
        user_state: &Arc<RwLock<ServerUserState>>,
        source_result: Result<bool, ProtocolError>,
    ) -> Result<(), RpcRoutedSendError> {
        let closing = self
            .rpc
            .begin_inbound_closing_for_handle_if(self.handle.call(), |_, _| true)
            .and_then(|closing| open_session_closing_from_rpc_closing(self.rpc.clone(), closing));

        let Some(closing) = closing else {
            return Ok(());
        };

        match source_result {
            Ok(true) => send_terminal_and_finish_open_session(user_state, closing, Ok(())).await,
            Ok(false) => {
                finish_open_session_closing_silently(user_state, closing).await;
                Ok(())
            }
            Err(error) => {
                send_terminal_and_finish_open_session(user_state, closing, Err(error)).await
            }
        }
    }
}

pub(crate) struct OpenSessionClosing {
    pub(crate) call: RpcInboundClosing,
    rpc: RpcDispatcher,
}

#[cfg(test)]
pub(crate) enum OpenSessionCloseTarget {
    Absent,
    Closing,
    AlreadyClosing,
}

pub(in crate::server) struct OpenSessionCleanupFinish {
    call: RpcInboundClosing,
    rpc: RpcDispatcher,
}

#[cfg(test)]
fn open_session_resources(
    us: &ServerUserState,
    key: &OpenSessionKey,
) -> Option<InboundCallResources> {
    let call = us
        .route_rpc(&key.counterparty)?
        .inbound_for_call(&key.call_id)
        .filter(|call| call.method == method::AGENT_OPEN_SESSION)?;
    let _ = call;
    us.route_rpc(&key.counterparty)?
        .inbound_resources_for_call(&key.call_id)
}

#[cfg(test)]
pub(crate) fn open_session_active_resources(
    us: &ServerUserState,
    key: &OpenSessionKey,
) -> Option<InboundCallResources> {
    let rpc = us.route_rpc(&key.counterparty)?;
    let call = rpc.inbound_for_call(&key.call_id).filter(|call| {
        call.method == method::AGENT_OPEN_SESSION && matches!(call.state, InboundCallState::Active)
    })?;
    call.resources.clone()
}

#[cfg(test)]
fn open_session_call_is_closing(us: &ServerUserState, key: &OpenSessionKey) -> bool {
    us.route_rpc(&key.counterparty).is_some_and(|rpc| {
        rpc.inbound_for_call(&key.call_id).is_some_and(|call| {
            call.method == method::AGENT_OPEN_SESSION
                && matches!(call.state, InboundCallState::Closing)
        })
    })
}

#[cfg(test)]
fn begin_open_session_closing_if(
    us: &mut ServerUserState,
    key: &OpenSessionKey,
    predicate: impl FnOnce(&InboundCallResources) -> bool,
) -> Option<OpenSessionClosing> {
    let rpc = us.route_rpc(&key.counterparty)?;
    let call = us
        .route_rpc(&key.counterparty)?
        .begin_inbound_closing_for_call_if(&key.call_id, |call, resources| {
            call.method == method::AGENT_OPEN_SESSION
                && matches!(call.state, InboundCallState::Active)
                && predicate(resources)
        })?;

    Some(OpenSessionClosing { call, rpc })
}

fn open_session_call_matches_agent(call: &crate::rpc::InboundCall, agent_id: Uuid) -> bool {
    call.method == method::AGENT_OPEN_SESSION
        && matches!(call.state, InboundCallState::Active)
        && matches!(
            &call.dedup_key,
            Some(DedupKey::OpenSession {
                agent_id: dedup_agent_id,
                ..
            }) if *dedup_agent_id == agent_id
        )
}

fn open_session_counterparty_route(call: &crate::rpc::InboundCall, fallback: &Route) -> Route {
    match &call.dedup_key {
        Some(DedupKey::OpenSession {
            counterparty_route, ..
        }) => counterparty_route.clone(),
        _ => fallback.clone(),
    }
}

pub(crate) fn begin_open_sessions_closing_for_agent(
    us: &mut ServerUserState,
    agent_id: Uuid,
) -> Vec<OpenSessionClosing> {
    let mut closings = Vec::new();
    for (_, rpc) in us.rpc_contexts_sorted() {
        closings.extend(
            rpc.begin_inbound_closing_calls_if(|call, _| {
                open_session_call_matches_agent(call, agent_id)
            })
            .into_iter()
            .filter_map(|closing| open_session_closing_from_rpc_closing(rpc.clone(), closing)),
        );
    }
    closings
}

pub(in crate::server) fn open_session_closing_from_rpc_closing(
    rpc: RpcDispatcher,
    call: RpcInboundClosing,
) -> Option<OpenSessionClosing> {
    (call.handle.method == method::AGENT_OPEN_SESSION).then_some(OpenSessionClosing { call, rpc })
}

#[cfg(test)]
pub(crate) fn begin_open_session_closing_or_absent(
    us: &mut ServerUserState,
    key: &OpenSessionKey,
) -> OpenSessionCloseTarget {
    if begin_open_session_closing_if(us, key, |_| true).is_some() {
        OpenSessionCloseTarget::Closing
    } else if open_session_call_is_closing(us, key) {
        OpenSessionCloseTarget::AlreadyClosing
    } else {
        OpenSessionCloseTarget::Absent
    }
}

pub(crate) async fn send_terminal_and_finish_open_session(
    user_state: &Arc<RwLock<ServerUserState>>,
    closing: OpenSessionClosing,
    terminal: Result<(), ProtocolError>,
) -> Result<(), RpcRoutedSendError> {
    let _ = user_state;
    let result = closing.call.send_empty_response_result(terminal).await;

    closing.rpc.finish_inbound_closing(&closing.call);

    result
}

pub(crate) async fn finish_open_sessions_with_error(
    user_state: &Arc<RwLock<ServerUserState>>,
    closings: Vec<OpenSessionClosing>,
    error: ProtocolError,
) {
    for closing in closings {
        let _ =
            send_terminal_and_finish_open_session(user_state, closing, Err(error.clone())).await;
    }
}

async fn finish_open_session_closing_silently(
    user_state: &Arc<RwLock<ServerUserState>>,
    closing: OpenSessionClosing,
) {
    let _ = user_state;
    closing.rpc.finish_inbound_closing(&closing.call);
}

pub(crate) async fn finish_open_session_closing_after_output_flush(
    user_state: &Arc<RwLock<ServerUserState>>,
    call: RpcInboundClosing,
    rpc: RpcDispatcher,
) {
    let _ = user_state;
    call.with_send_gate(|| async {
        rpc.finish_inbound_closing(&call);
    })
    .await;
}

pub(in crate::server) async fn finish_open_session_cleanup_jobs(
    user_state: &Arc<RwLock<ServerUserState>>,
    jobs: Vec<OpenSessionCleanupFinish>,
) {
    for job in jobs {
        finish_open_session_closing_after_output_flush(user_state, job.call, job.rpc).await;
    }
}

/// Cancel all active OpenSession calls matching a predicate.
///
/// OpenSession calls are enumerated and moved to `Closing` in `RpcState`.
/// The generic RPC call cancellation signal asks the service task to stop its
/// domain work; cleanup jobs only wait for the per-call send gate and remove the
/// RPC call once any in-flight output send is clear.
pub(in crate::server) fn cancel_open_sessions_matching(
    us: &mut ServerUserState,
    predicate: impl Fn(&OpenSessionKey, &InboundCallResources) -> bool,
) -> (usize, Vec<OpenSessionCleanupFinish>) {
    let mut finish_jobs = Vec::new();
    for (route, rpc) in us.rpc_contexts_sorted() {
        let closings = rpc.begin_inbound_closing_calls_if(|call, resources| {
            if call.method != method::AGENT_OPEN_SESSION
                || !matches!(call.state, InboundCallState::Active)
            {
                return false;
            }

            let key = OpenSessionKey::new(
                open_session_counterparty_route(call, &route),
                call.call_id.clone(),
            );
            predicate(&key, resources)
        });
        finish_jobs.extend(
            closings
                .into_iter()
                .filter_map(|closing| open_session_closing_from_rpc_closing(rpc.clone(), closing))
                .map(|closing| OpenSessionCleanupFinish {
                    call: closing.call,
                    rpc: closing.rpc,
                }),
        );
    }

    (finish_jobs.len(), finish_jobs)
}

pub(in crate::server) fn cancel_open_sessions_for_closed_link(
    us: &mut ServerUserState,
    closed_link: &crate::protocol::Link,
) -> (usize, Vec<OpenSessionCleanupFinish>) {
    cancel_open_sessions_matching(us, |key, resources| {
        resources.owner_link == *closed_link || key.counterparty.contains_link(closed_link.as_str())
    })
}

pub(in crate::server) fn cancel_open_sessions_for_owner_link(
    us: &mut ServerUserState,
    owner_link: &crate::protocol::Link,
) -> (usize, Vec<OpenSessionCleanupFinish>) {
    cancel_open_sessions_matching(us, |_, resources| resources.owner_link == *owner_link)
}

pub(in crate::server) fn cancel_open_sessions_for_route_prefix(
    us: &mut ServerUserState,
    route_prefix: &Route,
) -> (usize, Vec<OpenSessionCleanupFinish>) {
    cancel_open_sessions_matching(us, |key, _| {
        key.counterparty.starts_with_route(route_prefix)
    })
}

pub(in crate::server) fn cancel_open_session_for_route_and_call(
    us: &mut ServerUserState,
    counterparty: &Route,
    call_id: &CallId,
) -> (usize, Vec<OpenSessionCleanupFinish>) {
    cancel_open_sessions_matching(us, |key, _| {
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
    use crate::rpc::RpcRoutedBidiStart;

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn call_id(n: u128) -> CallId {
        CallId::from(Uuid::from_u128(n))
    }

    fn register_call(us: &mut ServerUserState, key: &OpenSessionKey, owner_link: Link) -> Uuid {
        let (tx, _rx) = mpsc::channel(1);
        us.ensure_route_rpc(key.counterparty.clone())
            .register_routed_bidi(RpcRoutedBidiStart {
                tx,
                owner_link,
                reply_src: route("server"),
                reply_dst: route("client"),
                call_id: key.call_id.clone(),
                method: method::AGENT_OPEN_SESSION,
                dedup_key: Some(DedupKey::OpenSession {
                    counterparty_route: key.counterparty.clone(),
                    agent_id: Uuid::from_u128(1),
                }),
                stream_capacity: 1,
            })
            .unwrap()
            .handle
            .generation
    }

    #[test]
    fn active_resources_ignore_closing_call() {
        let mut us = ServerUserState::new();
        let key = OpenSessionKey::new(route("client"), call_id(42));
        let generation = register_call(&mut us, &key, Link::new("owner").unwrap());

        let closing = begin_open_session_closing_if(&mut us, &key, |_| true)
            .expect("active call should begin closing");

        assert_eq!(closing.call.handle.generation, generation);
        assert!(open_session_active_resources(&us, &key).is_none());
        assert!(open_session_resources(&us, &key).is_some());
    }

    #[test]
    fn already_closing_call_is_not_absent() {
        let mut us = ServerUserState::new();
        let key = OpenSessionKey::new(route("client"), call_id(42));
        register_call(&mut us, &key, Link::new("owner").unwrap());

        assert!(matches!(
            begin_open_session_closing_or_absent(&mut us, &key),
            OpenSessionCloseTarget::Closing
        ));
        assert!(matches!(
            begin_open_session_closing_or_absent(&mut us, &key),
            OpenSessionCloseTarget::AlreadyClosing
        ));
    }

    #[tokio::test]
    async fn cleanup_matching_link_cancels_and_finishes_open_session_call() {
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let key = OpenSessionKey::new(route("client"), call_id(42));
        let owner_link = Link::new("owner").unwrap();

        let jobs = {
            let mut us = user_state.write().await;
            register_call(&mut us, &key, owner_link.clone());

            let (cancelled, jobs) = cancel_open_sessions_matching(&mut us, |_, resources| {
                resources.owner_link == owner_link
            });

            assert_eq!(cancelled, 1);
            assert_eq!(jobs.len(), 1);
            assert!(matches!(
                us.route_rpc(&key.counterparty)
                    .unwrap()
                    .inbound_for_call(&key.call_id),
                Some(call)
                    if call.method == method::AGENT_OPEN_SESSION
                        && matches!(call.state, InboundCallState::Closing)
            ));
            jobs
        };

        finish_open_session_cleanup_jobs(&user_state, jobs).await;

        let us = user_state.read().await;
        assert!(
            us.route_rpc(&key.counterparty)
                .unwrap()
                .inbound_for_call(&key.call_id)
                .is_none()
        );
    }
}

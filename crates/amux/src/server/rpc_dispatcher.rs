use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::protocol::Route;
use crate::protocol::link::Link;
use crate::protocol::message::CallId;
use crate::protocol::method::MethodSpec;
use crate::rpc::{
    DedupKey, InboundCall, InboundCallResources, InboundCallState, OutboundCall, OutboundCallState,
    RegisterCallError, RpcDebugSnapshot, RpcInboundBidi, RpcInboundCallHandle, RpcInboundClosing,
    RpcInboundFrameTarget, RpcInboundServerStream, RpcInboundUnary, RpcLocalOriginOutboundCall,
    RpcLocalOriginOutboundStart, RpcPeerStreamOutboundStart, RpcRoutedBidiStart,
    RpcRoutedUnaryStart, RpcServerStreamStart, RpcState,
};

#[derive(Clone, Default, Debug)]
pub(crate) struct RpcDispatcher {
    state: Arc<RwLock<RpcState>>,
}

pub(in crate::server) enum RpcInboundCloseTarget {
    Absent,
    AlreadyClosing,
    Closing(RpcInboundClosing),
}

impl RpcDispatcher {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> RwLockReadGuard<'_, RpcState> {
        self.state.read().expect("RPC state lock poisoned")
    }

    fn write(&self) -> RwLockWriteGuard<'_, RpcState> {
        self.state.write().expect("RPC state lock poisoned")
    }

    pub(in crate::server) fn active_inbound_call_id_for_dedup_key(
        &self,
        key: &DedupKey,
    ) -> Option<CallId> {
        self.read().active_inbound_call_id_for_dedup_key(key)
    }

    pub(in crate::server) fn inbound_len(&self) -> usize {
        self.read().inbound_len()
    }

    pub(in crate::server) fn outbound_len(&self) -> usize {
        self.read().outbound_len()
    }

    pub(in crate::server) fn dedup_len(&self) -> usize {
        self.read().dedup_len()
    }

    pub(in crate::server) fn debug_snapshot(&self) -> RpcDebugSnapshot {
        self.read().debug_snapshot()
    }

    pub(in crate::server) fn cancel_all(&self) {
        self.write().cancel_all();
    }

    pub(in crate::server) fn register_routed_unary(
        &self,
        start: RpcRoutedUnaryStart,
    ) -> Result<RpcInboundUnary, RegisterCallError> {
        self.write().register_routed_unary(start)
    }

    pub(crate) fn register_routed_bidi(
        &self,
        start: RpcRoutedBidiStart,
    ) -> Result<RpcInboundBidi, RegisterCallError> {
        self.write().register_routed_bidi(start)
    }

    pub(in crate::server) fn register_server_stream(
        &self,
        start: RpcServerStreamStart,
    ) -> Result<RpcInboundServerStream, RegisterCallError> {
        self.write().register_server_stream(start)
    }

    pub(in crate::server) fn register_local_origin_outbound(
        &self,
        start: RpcLocalOriginOutboundStart,
    ) -> Result<(), RegisterCallError> {
        self.write()
            .register_local_origin_outbound(start)
            .map(|_| ())
    }

    pub(in crate::server) fn register_peer_stream_outbound(
        &self,
        start: RpcPeerStreamOutboundStart,
    ) -> Result<(), RegisterCallError> {
        self.write()
            .register_peer_stream_outbound(start)
            .map(|_| ())
    }

    pub(in crate::server) fn outbound_for_call_matches(
        &self,
        call_id: &CallId,
        predicate: impl FnOnce(&OutboundCall) -> bool,
    ) -> bool {
        self.read()
            .outbound_for_call(call_id)
            .is_some_and(predicate)
    }

    #[cfg(test)]
    pub(crate) fn outbound_for_call(&self, call_id: &CallId) -> Option<OutboundCall> {
        self.read().outbound_for_call(call_id).cloned()
    }

    pub(in crate::server) fn set_outbound_state_for_call(
        &self,
        call_id: &CallId,
        state: OutboundCallState,
    ) -> bool {
        self.write().set_outbound_state_for_call(call_id, state)
    }

    pub(crate) fn inbound_frame_target_for_call(
        &self,
        call_id: &CallId,
    ) -> Option<RpcInboundFrameTarget> {
        self.read().inbound_frame_target_for_call(call_id)
    }

    #[cfg(test)]
    pub(crate) fn inbound_for_call(&self, call_id: &CallId) -> Option<InboundCall> {
        self.read().inbound_for_call(call_id).cloned()
    }

    #[cfg(test)]
    pub(in crate::server) fn inbound_call_ids_if(
        &self,
        predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<CallId> {
        self.read().inbound_call_ids_if(predicate)
    }

    #[cfg(test)]
    pub(in crate::server) fn inbound_resources_for_call(
        &self,
        call_id: &CallId,
    ) -> Option<InboundCallResources> {
        self.read().inbound_resources_for_call(call_id)
    }

    pub(in crate::server) fn activate_inbound_for_handle(
        &self,
        handle: &RpcInboundCallHandle,
    ) -> bool {
        self.write().activate_inbound_for_handle(handle)
    }

    pub(in crate::server) fn remove_inbound_for_handle(
        &self,
        handle: &RpcInboundCallHandle,
    ) -> Option<InboundCall> {
        self.write().remove_inbound_for_handle(handle)
    }

    pub(in crate::server) fn inbound_call_is_active_for_handle(
        &self,
        handle: &RpcInboundCallHandle,
    ) -> bool {
        self.read().inbound_call_is_active_for_handle(handle)
    }

    pub(in crate::server) fn reserve_inbound_dedup_for_handle(
        &self,
        handle: &RpcInboundCallHandle,
        key: DedupKey,
    ) -> Result<bool, RegisterCallError> {
        self.write().reserve_inbound_dedup_for_handle(handle, key)
    }

    #[cfg(test)]
    pub(in crate::server) fn begin_inbound_closing_for_call_if(
        &self,
        call_id: &CallId,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        self.write()
            .begin_inbound_closing_for_call_if(call_id, predicate)
    }

    pub(in crate::server) fn begin_inbound_closing_for_call(
        &self,
        call_id: &CallId,
    ) -> RpcInboundCloseTarget {
        let mut state = self.write();
        if let Some(closing) = state.begin_inbound_closing_for_call_if(call_id, |_, _| true) {
            return RpcInboundCloseTarget::Closing(closing);
        }

        if state
            .inbound_for_call(call_id)
            .is_some_and(|call| matches!(call.state, InboundCallState::Closing))
        {
            RpcInboundCloseTarget::AlreadyClosing
        } else {
            RpcInboundCloseTarget::Absent
        }
    }

    pub(in crate::server) fn begin_inbound_closing_calls_if(
        &self,
        mut predicate: impl FnMut(&InboundCall, &InboundCallResources) -> bool,
    ) -> Vec<RpcInboundClosing> {
        let mut state = self.write();
        let call_ids = state.inbound_call_ids_if(|call| {
            call.resources
                .as_ref()
                .is_some_and(|resources| predicate(call, resources))
        });
        call_ids
            .into_iter()
            .filter_map(|call_id| state.begin_inbound_closing_for_call_if(&call_id, |_, _| true))
            .collect()
    }

    pub(in crate::server) fn begin_inbound_closing_for_handle_if(
        &self,
        handle: &RpcInboundCallHandle,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        self.write()
            .begin_inbound_closing_for_handle_if(handle, predicate)
    }

    pub(in crate::server) fn finish_inbound_closing(
        &self,
        closing: &RpcInboundClosing,
    ) -> Option<InboundCall> {
        self.write().finish_inbound_closing(closing)
    }

    pub(in crate::server) fn finish_outbound_peer_routing_subscription(
        &self,
        link: &Link,
        call_id: &CallId,
    ) -> bool {
        self.write()
            .remove_outbound_for_call_if(call_id, |call| {
                call.method == crate::protocol::method::ROUTING_SUBSCRIBE_EVENTS
                    && matches!(
                        &call.resources,
                        Some(crate::rpc::OutboundCallResources::PeerRoutingSubscription {
                            link: call_link
                        }) if call_link == link
                    )
            })
            .is_some()
    }

    pub(in crate::server) fn finish_inbound_peer_routing_subscription(
        &self,
        link: &Link,
        call_id: &CallId,
    ) -> bool {
        self.write()
            .remove_inbound_for_call_if(call_id, |call| {
                call.method == crate::protocol::method::ROUTING_SUBSCRIBE_EVENTS
                    && call.dedup_key
                        == Some(DedupKey::PeerRoutingSubscription { link: link.clone() })
            })
            .is_some()
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_return_hop(
        &self,
        call_id: &CallId,
        owner_link: &Link,
        response_route: &Route,
    ) -> Option<OutboundCall> {
        self.write().remove_outbound_for_call_if(call_id, |call| {
            call.resources
                .as_ref()
                .and_then(|resources| resources.local_origin())
                .is_some_and(|(call_owner_link, request_src, request_dst)| {
                    call_owner_link == owner_link
                        && local_origin_request_route_matches(
                            request_src,
                            request_dst,
                            response_route,
                        )
                })
        })
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_return_hop_and_failed_route(
        &self,
        call_id: &CallId,
        owner_link: &Link,
        failed_route: &Route,
    ) -> Option<OutboundCall> {
        self.write().remove_outbound_for_call_if(call_id, |call| {
            call.resources
                .as_ref()
                .and_then(|resources| resources.local_origin())
                .is_some_and(|(call_owner_link, request_src, request_dst)| {
                    call_owner_link == owner_link
                        && local_origin_request_route_matches(
                            request_src,
                            request_dst,
                            failed_route,
                        )
                })
        })
    }

    pub(in crate::server) fn remove_tracked_outbound_for_call(
        &self,
        call_id: &CallId,
        failed_route: &Route,
    ) -> Option<OutboundCall> {
        self.write().remove_outbound_for_call_if(call_id, |call| {
            call.resources
                .as_ref()
                .and_then(|resources| resources.local_origin())
                .is_some_and(|(_, request_src, request_dst)| {
                    local_origin_request_route_matches(request_src, request_dst, failed_route)
                })
        })
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_owner_link(
        &self,
        owner_link: &Link,
    ) -> Vec<RpcLocalOriginOutboundCall> {
        self.write()
            .remove_local_origin_outbound_for_owner_link(owner_link)
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_route_prefix(
        &self,
        route_prefix: &Route,
    ) -> Vec<RpcLocalOriginOutboundCall> {
        self.write()
            .remove_local_origin_outbound_for_route_prefix(route_prefix)
    }

    pub(in crate::server) fn remove_inbound_for_owner_link_except_method(
        &self,
        owner_link: &Link,
        excluded_method: MethodSpec,
    ) -> Vec<InboundCall> {
        self.write()
            .remove_inbound_for_owner_link_except_method(owner_link, excluded_method)
    }
}

fn local_origin_request_route_matches(
    request_src: &Route,
    request_dst: &Route,
    failed_route: &Route,
) -> bool {
    Route::from_links(
        request_src
            .iter()
            .chain(request_dst.iter())
            .map(|link| link.as_str().to_string()),
    )
    .is_ok_and(|route| route == *failed_route)
}

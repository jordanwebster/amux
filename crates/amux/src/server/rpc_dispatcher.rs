use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::protocol::Route;
use crate::protocol::link::Link;
use crate::protocol::message::RoutedCallId;
use crate::protocol::method::MethodSpec;
use crate::rpc::{
    DedupKey, InboundCall, InboundCallResources, InboundCallState, OutboundCall, OutboundCallState,
    RegisterCallError, RpcDebugSnapshot, RpcInboundBidi, RpcInboundCallHandle, RpcInboundClosing,
    RpcInboundFrameTarget, RpcInboundServerStream, RpcInboundUnary, RpcLocalOriginOutboundCall,
    RpcLocalOriginOutboundStart, RpcPeerStreamOutboundStart, RpcRoutedBidiStart,
    RpcRoutedUnaryStart, RpcServerStreamStart, RpcState,
};

#[derive(Clone, Default)]
pub(crate) struct RpcDispatcher {
    state: Arc<RwLock<RpcState>>,
}

pub(in crate::server) enum RpcInboundCloseTarget {
    Absent,
    AlreadyClosing,
    Closing(RpcInboundClosing),
}

impl RpcDispatcher {
    pub(in crate::server) fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> RwLockReadGuard<'_, RpcState> {
        self.state.read().expect("RPC state lock poisoned")
    }

    fn write(&self) -> RwLockWriteGuard<'_, RpcState> {
        self.state.write().expect("RPC state lock poisoned")
    }

    pub(in crate::server) fn active_inbound_call_id_for_route_and_method(
        &self,
        counterparty_route: &Route,
        method: MethodSpec,
    ) -> Option<RoutedCallId> {
        self.read()
            .active_inbound_call_id_for_route_and_method(counterparty_route, method)
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

    pub(in crate::server) fn outbound_for_route_matches(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        predicate: impl FnOnce(&OutboundCall) -> bool,
    ) -> bool {
        self.read()
            .outbound_for_route(counterparty_route, call_id)
            .is_some_and(predicate)
    }

    #[cfg(test)]
    pub(crate) fn outbound_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<OutboundCall> {
        self.read()
            .outbound_for_route(counterparty_route, call_id)
            .cloned()
    }

    pub(in crate::server) fn set_outbound_state_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        state: OutboundCallState,
    ) -> bool {
        self.write()
            .set_outbound_state_for_route(counterparty_route, call_id, state)
    }

    pub(crate) fn inbound_frame_target_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<RpcInboundFrameTarget> {
        self.read()
            .inbound_frame_target_for_route(counterparty_route, call_id)
    }

    #[cfg(test)]
    pub(crate) fn inbound_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<InboundCall> {
        self.read()
            .inbound_for_route(counterparty_route, call_id)
            .cloned()
    }

    #[cfg(test)]
    pub(in crate::server) fn inbound_call_keys_if(
        &self,
        predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<(Route, RoutedCallId)> {
        self.read().inbound_call_keys_if(predicate)
    }

    #[cfg(test)]
    pub(in crate::server) fn inbound_resources_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<InboundCallResources> {
        self.read()
            .inbound_resources_for_route(counterparty_route, call_id)
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
    pub(in crate::server) fn begin_inbound_closing_for_route_if(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        self.write()
            .begin_inbound_closing_for_route_if(counterparty_route, call_id, predicate)
    }

    pub(in crate::server) fn begin_inbound_closing_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> RpcInboundCloseTarget {
        let mut state = self.write();
        if let Some(closing) =
            state.begin_inbound_closing_for_route_if(counterparty_route, call_id, |_, _| true)
        {
            return RpcInboundCloseTarget::Closing(closing);
        }

        if state
            .inbound_for_route(counterparty_route, call_id)
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
        let keys = state.inbound_call_keys_if(|call| {
            call.resources
                .as_ref()
                .is_some_and(|resources| predicate(call, resources))
        });
        keys.into_iter()
            .filter_map(|(counterparty_route, call_id)| {
                state.begin_inbound_closing_for_route_if(&counterparty_route, &call_id, |_, _| true)
            })
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
        call_id: &RoutedCallId,
    ) -> bool {
        let route = Route::from_link(link.clone());
        self.write()
            .remove_outbound_for_route_if(&route, call_id, |call| {
                call.method == crate::protocol::method::ROUTING_SUBSCRIBE_EVENTS
            })
            .is_some()
    }

    pub(in crate::server) fn finish_inbound_peer_routing_subscription(
        &self,
        link: &Link,
        call_id: &RoutedCallId,
    ) -> bool {
        self.write()
            .remove_inbound_for_route_if(&Route::from_link(link.clone()), call_id, |call| {
                call.method == crate::protocol::method::ROUTING_SUBSCRIBE_EVENTS
            })
            .is_some()
    }

    pub(in crate::server) fn remove_peer_routing_calls_for_link(&self, link: &Link) {
        let route = Route::from_link(link.clone());
        let mut state = self.write();
        state.remove_inbound_calls_if(|call| {
            call.counterparty_route == route
                && call.method == crate::protocol::method::ROUTING_SUBSCRIBE_EVENTS
        });
        state.remove_outbound_calls_if(|call| {
            call.counterparty_route == route
                && call.method == crate::protocol::method::ROUTING_SUBSCRIBE_EVENTS
        });
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_return_hop(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        owner_link: &Link,
    ) -> Option<OutboundCall> {
        self.write()
            .remove_outbound_for_route_if(counterparty_route, call_id, |call| {
                call.resources
                    .as_ref()
                    .and_then(|resources| resources.local_origin())
                    .is_some_and(|(call_owner_link, _, _)| call_owner_link == owner_link)
            })
    }

    pub(in crate::server) fn remove_tracked_outbound_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<OutboundCall> {
        self.write()
            .remove_outbound_for_route_if(counterparty_route, call_id, |call| {
                call.resources.is_some()
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

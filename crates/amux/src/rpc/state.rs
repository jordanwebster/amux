use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use super::call::*;
use super::stream::{RpcCallCancellation, RpcPeerStreamSink, RpcRoutedSink, RpcStreamWriter};
use crate::protocol::link::Link;
use crate::protocol::message::{Message, RoutedFrame, RoutedFrameMessage};
use crate::protocol::method::{MethodKind, MethodSpec};
use crate::protocol::{Route, RoutedCallId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RpcCallKey {
    counterparty_route: Route,
    call_id: RoutedCallId,
}

impl RpcCallKey {
    fn new(counterparty_route: Route, call_id: RoutedCallId) -> Self {
        Self {
            counterparty_route,
            call_id,
        }
    }
}

fn inbound_call_matches_handle(call: &InboundCall, handle: &RpcInboundCallHandle) -> bool {
    call.counterparty_route == handle.counterparty_route
        && call.call_id == handle.call_id
        && call.method == handle.method
        && call.generation == handle.generation
}

fn outbound_call_matches_handle(call: &OutboundCall, handle: &RpcOutboundCallHandle) -> bool {
    call.counterparty_route == handle.counterparty_route
        && call.call_id == handle.call_id
        && call.method == handle.method
}

fn client_inbox(call: &OutboundCall) -> Option<mpsc::Sender<Message>> {
    match &call.resources {
        Some(OutboundCallResources::ClientInbox { tx }) => Some(tx.clone()),
        Some(OutboundCallResources::LocalOriginRouted { .. }) | None => None,
    }
}

#[derive(Debug, Default)]
pub(crate) struct RpcState {
    inbound_calls: HashMap<RpcCallKey, InboundCall>,
    outbound_calls: HashMap<RpcCallKey, OutboundCall>,
    inbound_dedup_index: HashMap<DedupKey, RpcCallKey>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RpcDebugSnapshot {
    pub(in crate::rpc) inbound_calls: RpcCallDebugSnapshot,
    pub(in crate::rpc) outbound_calls: RpcCallDebugSnapshot,
    pub(in crate::rpc) inbound_dedup_keys: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RpcCallDebugSnapshot {
    pub(in crate::rpc) total: usize,
    pub(in crate::rpc) by_state: BTreeMap<&'static str, usize>,
    pub(in crate::rpc) by_method: BTreeMap<&'static str, usize>,
    pub(in crate::rpc) by_counterparty: BTreeMap<String, usize>,
}

impl RpcState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn inbound_len(&self) -> usize {
        self.inbound_calls.len()
    }

    pub(crate) fn outbound_len(&self) -> usize {
        self.outbound_calls.len()
    }

    pub(crate) fn dedup_len(&self) -> usize {
        self.inbound_dedup_index.len()
    }

    pub(crate) fn debug_snapshot(&self) -> RpcDebugSnapshot {
        let mut inbound = RpcCallDebugSnapshot::new([
            InboundCallState::Starting.as_str(),
            InboundCallState::Active.as_str(),
            InboundCallState::Closing.as_str(),
        ]);
        for call in self.inbound_calls.values() {
            inbound.record(
                call.state.as_str(),
                call.method.name,
                call.counterparty_route.to_string(),
            );
        }

        let mut outbound = RpcCallDebugSnapshot::new([
            OutboundCallState::AwaitingResponse.as_str(),
            OutboundCallState::ActiveStream.as_str(),
            OutboundCallState::Closing.as_str(),
        ]);
        for (key, call) in &self.outbound_calls {
            debug_assert_eq!(key.call_id, call.call_id);
            debug_assert_eq!(key.counterparty_route, call.counterparty_route);
            outbound.record(
                call.state.as_str(),
                call.method.name,
                call.counterparty_route.to_string(),
            );
        }

        RpcDebugSnapshot {
            inbound_calls: inbound,
            outbound_calls: outbound,
            inbound_dedup_keys: self.dedup_len(),
        }
    }

    pub(crate) fn register_routed_bidi(
        &mut self,
        start: RpcRoutedBidiStart,
    ) -> Result<RpcInboundBidi, RegisterCallError> {
        let (stream_writer, stream_reader) = RpcStreamWriter::channel(start.stream_capacity);
        let generation = Uuid::new_v4();
        let send_gate = Arc::new(Mutex::new(()));
        let cancellation = RpcCallCancellation::new();
        let output = RpcRoutedSink::new(
            start.tx,
            start.reply_src,
            start.reply_dst,
            start.call_id.clone(),
            send_gate,
        );
        let resources = InboundCallResources {
            owner_link: start.owner_link,
            output: output.clone(),
        };
        let handle = RpcInboundCallHandle {
            counterparty_route: start.counterparty_route.clone(),
            call_id: start.call_id.clone(),
            method: start.method,
            generation,
        };
        self.register_inbound(InboundCall {
            call_id: start.call_id.clone(),
            counterparty_route: start.counterparty_route,
            method: start.method,
            generation,
            state: InboundCallState::Active,
            dedup_key: start.dedup_key,
            stream_writer: Some(stream_writer),
            resources: Some(resources),
            cancellation: cancellation.clone(),
        })?;

        Ok(RpcInboundBidi {
            handle,
            input: stream_reader,
            output,
            cancellation,
        })
    }

    pub(crate) fn register_routed_unary(
        &mut self,
        start: RpcRoutedUnaryStart,
    ) -> Result<RpcInboundUnary, RegisterCallError> {
        let generation = Uuid::new_v4();
        let send_gate = Arc::new(Mutex::new(()));
        let cancellation = RpcCallCancellation::new();
        let output = RpcRoutedSink::new(
            start.tx,
            start.reply_src,
            start.reply_dst,
            start.call_id.clone(),
            send_gate,
        );
        let handle = RpcInboundCallHandle {
            counterparty_route: start.counterparty_route.clone(),
            call_id: start.call_id.clone(),
            method: start.method,
            generation,
        };
        self.register_inbound(InboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            generation,
            state: InboundCallState::Active,
            dedup_key: None,
            stream_writer: None,
            resources: Some(InboundCallResources {
                owner_link: start.owner_link,
                output,
            }),
            cancellation,
        })?;

        Ok(RpcInboundUnary { handle })
    }

    pub(crate) fn register_server_stream(
        &mut self,
        start: RpcServerStreamStart,
    ) -> Result<RpcInboundServerStream, RegisterCallError> {
        debug_assert_eq!(start.method.kind, MethodKind::ServerStreaming);
        let generation = Uuid::new_v4();
        let cancellation = RpcCallCancellation::new();
        let output = RpcPeerStreamSink {
            tx: start.tx,
            call_id: start.call_id.clone(),
        };
        let handle = RpcInboundCallHandle {
            counterparty_route: start.counterparty_route.clone(),
            call_id: start.call_id.clone(),
            method: start.method,
            generation,
        };
        self.register_inbound(InboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            generation,
            state: InboundCallState::Starting,
            dedup_key: start.dedup_key,
            stream_writer: None,
            resources: None,
            cancellation: cancellation.clone(),
        })?;

        Ok(RpcInboundServerStream { handle, output })
    }

    pub(in crate::rpc) fn register_inbound(
        &mut self,
        call: InboundCall,
    ) -> Result<(), RegisterCallError> {
        let call_key = RpcCallKey::new(call.counterparty_route.clone(), call.call_id.clone());
        if self.inbound_calls.contains_key(&call_key) {
            return Err(RegisterCallError::DuplicateCallId {
                counterparty_route: call_key.counterparty_route,
                call_id: call_key.call_id,
            });
        }

        if let Some(key) = &call.dedup_key {
            if let Some(existing_call_key) = self.inbound_dedup_index.get(key)
                && self.inbound_calls.contains_key(existing_call_key)
            {
                return Err(RegisterCallError::DuplicateDedupKey {
                    key: key.clone(),
                    counterparty_route: existing_call_key.counterparty_route.clone(),
                    call_id: existing_call_key.call_id.clone(),
                });
            }
            self.inbound_dedup_index
                .insert(key.clone(), call_key.clone());
        }

        self.inbound_calls.insert(call_key, call);
        Ok(())
    }

    pub(crate) fn inbound_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<&InboundCall> {
        self.inbound_calls.get(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn inbound_resources_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<InboundCallResources> {
        self.inbound_for_route(counterparty_route, call_id)
            .and_then(|call| call.resources.clone())
    }

    pub(crate) fn inbound_for_handle(&self, handle: &RpcInboundCallHandle) -> Option<&InboundCall> {
        self.inbound_for_route(&handle.counterparty_route, &handle.call_id)
            .filter(|call| inbound_call_matches_handle(call, handle))
    }

    pub(crate) fn inbound_call_is_active_for_handle(&self, handle: &RpcInboundCallHandle) -> bool {
        self.inbound_for_handle(handle)
            .is_some_and(|call| matches!(call.state, InboundCallState::Active))
    }

    pub(crate) fn activate_inbound_for_handle(&mut self, handle: &RpcInboundCallHandle) -> bool {
        let Some(call) = self.inbound_calls.get_mut(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        )) else {
            return false;
        };
        if !inbound_call_matches_handle(call, handle)
            || !matches!(call.state, InboundCallState::Starting)
        {
            return false;
        }
        call.state = InboundCallState::Active;
        true
    }

    pub(crate) fn reserve_inbound_dedup_for_handle(
        &mut self,
        handle: &RpcInboundCallHandle,
        key: DedupKey,
    ) -> Result<bool, RegisterCallError> {
        let call_key = RpcCallKey::new(handle.counterparty_route.clone(), handle.call_id.clone());
        let Some(call) = self.inbound_calls.get(&call_key) else {
            return Ok(false);
        };
        if !inbound_call_matches_handle(call, handle) {
            return Ok(false);
        }

        if let Some(existing_call_key) = self.inbound_dedup_index.get(&key) {
            if *existing_call_key == call_key {
                return Ok(true);
            }
            if self.inbound_calls.contains_key(existing_call_key) {
                return Err(RegisterCallError::DuplicateDedupKey {
                    key,
                    counterparty_route: existing_call_key.counterparty_route.clone(),
                    call_id: existing_call_key.call_id.clone(),
                });
            }
        }

        let call = self
            .inbound_calls
            .get_mut(&call_key)
            .expect("call existence checked above");
        call.dedup_key = Some(key.clone());
        self.inbound_dedup_index.insert(key, call_key);
        Ok(true)
    }

    pub(crate) fn inbound_call_keys_if(
        &self,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<(Route, RoutedCallId)> {
        self.inbound_calls
            .values()
            .filter(|call| predicate(call))
            .map(|call| (call.counterparty_route.clone(), call.call_id.clone()))
            .collect()
    }

    pub(crate) fn active_inbound_call_id_for_route_and_method(
        &self,
        counterparty_route: &Route,
        method: MethodSpec,
    ) -> Option<RoutedCallId> {
        self.inbound_calls.values().find_map(|call| {
            (call.counterparty_route == *counterparty_route
                && call.method == method
                && matches!(call.state, InboundCallState::Active))
            .then(|| call.call_id.clone())
        })
    }

    pub(crate) fn inbound_frame_target_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<RpcInboundFrameTarget> {
        self.inbound_for_route(counterparty_route, call_id)
            .map(|call| match call.state {
                InboundCallState::Active => match &call.stream_writer {
                    Some(stream_writer) => RpcInboundFrameTarget::ActiveStream {
                        method: call.method,
                        stream_writer: stream_writer.clone(),
                    },
                    None => RpcInboundFrameTarget::ActiveNoInput {
                        method: call.method,
                    },
                },
                InboundCallState::Starting | InboundCallState::Closing => {
                    RpcInboundFrameTarget::NotAccepting { state: call.state }
                }
            })
    }

    pub(crate) fn begin_inbound_closing_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        let call_key = RpcCallKey::new(counterparty_route.clone(), call_id.clone());
        let call = self.inbound_calls.get_mut(&call_key)?;
        let resources = call.resources.clone()?;
        if matches!(call.state, InboundCallState::Closing) || !predicate(call, &resources) {
            return None;
        }
        call.state = InboundCallState::Closing;
        call.cancellation.cancel();
        Some(RpcInboundClosing {
            handle: RpcInboundCallHandle {
                counterparty_route: call.counterparty_route.clone(),
                call_id: call.call_id.clone(),
                method: call.method,
                generation: call.generation,
            },
            output: resources.output,
        })
    }

    pub(crate) fn begin_inbound_closing_for_handle_if(
        &mut self,
        handle: &RpcInboundCallHandle,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        self.begin_inbound_closing_for_route_if(
            &handle.counterparty_route,
            &handle.call_id,
            |call, resources| {
                inbound_call_matches_handle(call, handle) && predicate(call, resources)
            },
        )
    }

    pub(crate) fn finish_inbound_closing(
        &mut self,
        closing: &RpcInboundClosing,
    ) -> Option<InboundCall> {
        let call_key = RpcCallKey::new(
            closing.handle.counterparty_route.clone(),
            closing.handle.call_id.clone(),
        );
        let call = self.inbound_calls.get(&call_key)?;
        let generation_matches = call.generation == closing.handle.generation;
        if !matches!(call.state, InboundCallState::Closing)
            || !generation_matches
            || call.method != closing.handle.method
        {
            return None;
        }
        self.remove_inbound_by_key(&call_key)
    }

    #[cfg(test)]
    pub(crate) fn set_inbound_state_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        mut predicate: impl FnMut(&InboundCall) -> bool,
        state: InboundCallState,
    ) -> bool {
        let Some(call) = self.inbound_calls.get_mut(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        )) else {
            return false;
        };
        if !predicate(call) {
            return false;
        }
        call.state = state;
        true
    }

    #[cfg(test)]
    pub(crate) fn remove_inbound_for_route(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<InboundCall> {
        self.remove_inbound_by_key(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        ))
    }

    pub(crate) fn remove_inbound_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Option<InboundCall> {
        let key = RpcCallKey::new(counterparty_route.clone(), call_id.clone());
        if !self.inbound_calls.get(&key).is_some_and(&mut predicate) {
            return None;
        }
        self.remove_inbound_by_key(&key)
    }

    pub(crate) fn remove_inbound_for_handle(
        &mut self,
        handle: &RpcInboundCallHandle,
    ) -> Option<InboundCall> {
        let key = RpcCallKey::new(handle.counterparty_route.clone(), handle.call_id.clone());
        if !self
            .inbound_calls
            .get(&key)
            .is_some_and(|call| inbound_call_matches_handle(call, handle))
        {
            return None;
        }
        self.remove_inbound_by_key(&key)
    }

    pub(crate) fn remove_inbound_calls_if(
        &mut self,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<InboundCall> {
        let keys: Vec<_> = self
            .inbound_calls
            .iter()
            .filter_map(|(key, call)| predicate(call).then_some(key.clone()))
            .collect();
        keys.into_iter()
            .filter_map(|key| self.remove_inbound_by_key(&key))
            .collect()
    }

    fn remove_inbound_by_key(&mut self, key: &RpcCallKey) -> Option<InboundCall> {
        let call = self.inbound_calls.remove(key)?;
        call.cancellation.cancel();
        if let Some(key) = &call.dedup_key
            && self
                .inbound_dedup_index
                .get(key)
                .is_some_and(|indexed_call_key| {
                    indexed_call_key.call_id == call.call_id
                        && indexed_call_key.counterparty_route == call.counterparty_route
                })
        {
            self.inbound_dedup_index.remove(key);
        }
        Some(call)
    }

    #[cfg(test)]
    pub(crate) fn dedup_call_key(&self, key: &DedupKey) -> Option<(&Route, &RoutedCallId)> {
        self.inbound_dedup_index
            .get(key)
            .map(|call_key| (&call_key.counterparty_route, &call_key.call_id))
    }

    pub(in crate::rpc) fn register_outbound(
        &mut self,
        call: OutboundCall,
    ) -> Result<(), RegisterCallError> {
        let call_key = RpcCallKey::new(call.counterparty_route.clone(), call.call_id.clone());
        if self.outbound_calls.contains_key(&call_key) {
            return Err(RegisterCallError::DuplicateCallId {
                counterparty_route: call_key.counterparty_route,
                call_id: call_key.call_id,
            });
        }
        self.outbound_calls.insert(call_key, call);
        Ok(())
    }

    pub(in crate::rpc) fn register_outbound_tracked(
        &mut self,
        call: OutboundCall,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        let handle = RpcOutboundCallHandle {
            counterparty_route: call.counterparty_route.clone(),
            call_id: call.call_id.clone(),
            method: call.method,
        };
        self.register_outbound(call)?;
        Ok(handle)
    }

    pub(crate) fn register_client_outbound(
        &mut self,
        start: RpcClientOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            state: start.state,
            resources: Some(OutboundCallResources::ClientInbox { tx: start.inbox_tx }),
        })
    }

    pub(crate) fn register_local_origin_outbound(
        &mut self,
        start: RpcLocalOriginOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            state: start.state,
            resources: Some(OutboundCallResources::LocalOriginRouted {
                owner_link: start.owner_link,
                request_src: start.request_src,
                request_dst: start.request_dst,
            }),
        })
    }

    pub(crate) fn register_peer_stream_outbound(
        &mut self,
        start: RpcPeerStreamOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        debug_assert_eq!(start.method.kind, MethodKind::ServerStreaming);
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            counterparty_route: start.counterparty_route,
            method: start.method,
            state: OutboundCallState::AwaitingResponse,
            resources: None,
        })
    }

    pub(crate) fn outbound_for_route(
        &self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
    ) -> Option<&OutboundCall> {
        self.outbound_calls.get(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        ))
    }

    pub(crate) fn set_outbound_state_for_route(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        state: OutboundCallState,
    ) -> bool {
        let Some(call) = self.outbound_calls.get_mut(&RpcCallKey::new(
            counterparty_route.clone(),
            call_id.clone(),
        )) else {
            return false;
        };
        call.state = state;
        true
    }

    pub(crate) fn set_outbound_state_for_handle(
        &mut self,
        handle: &RpcOutboundCallHandle,
        state: OutboundCallState,
    ) -> bool {
        let Some(call) = self.outbound_calls.get_mut(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        )) else {
            return false;
        };
        if !outbound_call_matches_handle(call, handle) {
            return false;
        }
        call.state = state;
        true
    }

    pub(crate) fn set_outbound_state_for_handle_if(
        &mut self,
        handle: &RpcOutboundCallHandle,
        predicate: impl FnOnce(OutboundCallState) -> bool,
        state: OutboundCallState,
    ) -> bool {
        let Some(call) = self.outbound_calls.get_mut(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        )) else {
            return false;
        };
        if !outbound_call_matches_handle(call, handle) || !predicate(call.state) {
            return false;
        }
        call.state = state;
        true
    }

    pub(crate) fn outbound_state_for_handle(
        &self,
        handle: &RpcOutboundCallHandle,
    ) -> Option<OutboundCallState> {
        let call = self.outbound_calls.get(&RpcCallKey::new(
            handle.counterparty_route.clone(),
            handle.call_id.clone(),
        ))?;
        outbound_call_matches_handle(call, handle).then_some(call.state)
    }

    pub(crate) fn remove_outbound_for_handle(
        &mut self,
        handle: &RpcOutboundCallHandle,
    ) -> Option<OutboundCall> {
        self.remove_outbound_for_route_if(&handle.counterparty_route, &handle.call_id, |call| {
            outbound_call_matches_handle(call, handle)
        })
    }

    pub(crate) fn remove_outbound_for_route_if(
        &mut self,
        counterparty_route: &Route,
        call_id: &RoutedCallId,
        mut predicate: impl FnMut(&OutboundCall) -> bool,
    ) -> Option<OutboundCall> {
        let key = RpcCallKey::new(counterparty_route.clone(), call_id.clone());
        if !self.outbound_calls.get(&key).is_some_and(&mut predicate) {
            return None;
        }
        self.outbound_calls.remove(&key)
    }

    pub(crate) fn remove_outbound_calls_if(
        &mut self,
        mut predicate: impl FnMut(&OutboundCall) -> bool,
    ) -> Vec<OutboundCall> {
        let keys: Vec<_> = self
            .outbound_calls
            .iter()
            .filter_map(|(key, call)| predicate(call).then_some(key.clone()))
            .collect();
        keys.into_iter()
            .filter_map(|key| self.outbound_calls.remove(&key))
            .collect()
    }

    pub(crate) fn remove_local_origin_outbound_for_owner_link(
        &mut self,
        owner_link: &Link,
    ) -> Vec<RpcLocalOriginOutboundCall> {
        self.remove_outbound_calls_if(|call| {
            call.resources
                .as_ref()
                .and_then(|resources| resources.local_origin())
                .is_some_and(|(call_owner_link, _, _)| call_owner_link == owner_link)
        })
        .into_iter()
        .filter_map(OutboundCall::into_local_origin)
        .collect()
    }

    pub(crate) fn remove_local_origin_outbound_for_route_prefix(
        &mut self,
        route_prefix: &Route,
    ) -> Vec<RpcLocalOriginOutboundCall> {
        self.remove_outbound_calls_if(|call| {
            call.resources
                .as_ref()
                .and_then(|resources| resources.local_origin())
                .is_some_and(|(_, _, request_dst)| request_dst.starts_with_route(route_prefix))
        })
        .into_iter()
        .filter_map(OutboundCall::into_local_origin)
        .collect()
    }

    pub(crate) fn remove_inbound_for_owner_link_except_method(
        &mut self,
        owner_link: &Link,
        excluded_method: MethodSpec,
    ) -> Vec<InboundCall> {
        self.remove_inbound_calls_if(|call| {
            call.method != excluded_method
                && call
                    .resources
                    .as_ref()
                    .is_some_and(|resources| resources.owner_link == *owner_link)
        })
    }

    pub(crate) fn client_inbox_for_message(
        &self,
        message: &Message,
    ) -> Option<mpsc::Sender<Message>> {
        match message {
            Message::Local(frame) => self
                .outbound_calls
                .values()
                .find(|call| call.call_id == frame.call_id)
                .and_then(client_inbox),
            Message::Routed(RoutedFrame {
                src,
                dst,
                call_id,
                message: RoutedFrameMessage::Payload(_),
            }) if dst.is_empty() => self
                .outbound_calls
                .get(&RpcCallKey::new(src.clone(), call_id.clone()))
                .and_then(client_inbox),
            Message::Routed(RoutedFrame {
                dst,
                call_id,
                message: RoutedFrameMessage::RoutingError { failed_route, .. },
                ..
            }) if dst.is_empty() => self
                .outbound_calls
                .get(&RpcCallKey::new(failed_route.clone(), call_id.clone()))
                .and_then(client_inbox),
            Message::Peer(_)
            | Message::Routed(_)
            | Message::Ping
            | Message::Pong
            | Message::Reauth(_)
            | Message::ReauthResponse(_)
            | Message::PeerSnapshot { .. }
            | Message::GoAway(_) => None,
        }
    }

    pub(crate) fn remove_client_inboxes(&mut self) -> Vec<mpsc::Sender<Message>> {
        self.remove_outbound_calls_if(|call| {
            matches!(
                call.resources,
                Some(OutboundCallResources::ClientInbox { .. })
            )
        })
        .into_iter()
        .filter_map(|call| client_inbox(&call))
        .collect()
    }
}

impl RpcCallDebugSnapshot {
    fn new<const N: usize>(states: [&'static str; N]) -> Self {
        Self {
            total: 0,
            by_state: states.into_iter().map(|state| (state, 0)).collect(),
            by_method: BTreeMap::new(),
            by_counterparty: BTreeMap::new(),
        }
    }

    fn record(&mut self, state: &'static str, method: &'static str, counterparty: String) {
        self.total += 1;
        *self.by_state.entry(state).or_default() += 1;
        *self.by_method.entry(method).or_default() += 1;
        *self.by_counterparty.entry(counterparty).or_default() += 1;
    }
}

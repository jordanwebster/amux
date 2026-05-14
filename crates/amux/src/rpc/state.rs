use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use uuid::Uuid;

use super::call::*;
use super::stream::RpcCallCancellation;
use crate::protocol::CallId;
use crate::protocol::method::MethodKind;

fn inbound_call_matches_handle(call: &InboundCall, handle: &RpcInboundCallHandle) -> bool {
    call.call_id == handle.call_id
        && call.method == handle.method
        && call.generation == handle.generation
}

fn outbound_call_matches_handle(call: &OutboundCall, handle: &RpcOutboundCallHandle) -> bool {
    call.call_id == handle.call_id && call.method == handle.method
}

#[derive(Debug, Default)]
pub(crate) struct RpcState {
    inbound_calls: HashMap<CallId, InboundCall>,
    outbound_calls: HashMap<CallId, OutboundCall>,
    inbound_dedup_index: HashMap<DedupKey, CallId>,
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
            inbound.record(call.state.as_str(), call.method.name);
        }

        let mut outbound = RpcCallDebugSnapshot::new([
            OutboundCallState::AwaitingResponse.as_str(),
            OutboundCallState::ActiveStream.as_str(),
            OutboundCallState::Closing.as_str(),
        ]);
        for (call_id, call) in &self.outbound_calls {
            debug_assert_eq!(*call_id, call.call_id);
            outbound.record(call.state.as_str(), call.method.name);
        }

        RpcDebugSnapshot {
            inbound_calls: inbound,
            outbound_calls: outbound,
            inbound_dedup_keys: self.dedup_len(),
        }
    }

    pub(crate) fn cancel_all(&mut self) {
        for call in self.inbound_calls.values() {
            call.cancellation.cancel();
        }
        self.inbound_calls.clear();
        self.outbound_calls.clear();
        self.inbound_dedup_index.clear();
    }

    pub(crate) fn register_inbound_unary(
        &mut self,
        start: RpcInboundStart,
    ) -> Result<RpcInboundUnary, RegisterCallError> {
        let handle = self.register_inbound(start, InboundCallState::Active)?;
        Ok(RpcInboundUnary { handle })
    }

    pub(crate) fn register_inbound_server_stream(
        &mut self,
        start: RpcInboundStart,
    ) -> Result<RpcInboundServerStream, RegisterCallError> {
        debug_assert_eq!(start.method.kind, MethodKind::ServerStreaming);
        let cancellation = RpcCallCancellation::new();
        let handle = self.register_inbound_with_cancellation(
            start,
            InboundCallState::Starting,
            cancellation.clone(),
        )?;
        Ok(RpcInboundServerStream {
            handle,
            cancellation,
        })
    }

    fn register_inbound(
        &mut self,
        start: RpcInboundStart,
        state: InboundCallState,
    ) -> Result<RpcInboundCallHandle, RegisterCallError> {
        let cancellation = RpcCallCancellation::new();
        self.register_inbound_with_cancellation(start, state, cancellation)
    }

    fn register_inbound_with_cancellation(
        &mut self,
        start: RpcInboundStart,
        state: InboundCallState,
        cancellation: RpcCallCancellation,
    ) -> Result<RpcInboundCallHandle, RegisterCallError> {
        let generation = Uuid::new_v4();
        let handle = RpcInboundCallHandle {
            call_id: start.call_id.clone(),
            method: start.method,
            generation,
        };
        self.insert_inbound(InboundCall {
            call_id: start.call_id,
            method: start.method,
            generation,
            state,
            dedup_key: start.dedup_key,
            cancellation,
        })?;
        Ok(handle)
    }

    fn insert_inbound(&mut self, call: InboundCall) -> Result<(), RegisterCallError> {
        let call_id = call.call_id.clone();
        if self.inbound_calls.contains_key(&call_id) || self.outbound_calls.contains_key(&call_id) {
            return Err(RegisterCallError::DuplicateCallId { call_id });
        }

        if let Some(key) = &call.dedup_key {
            if let Some(existing_call_id) = self.inbound_dedup_index.get(key)
                && self.inbound_calls.contains_key(existing_call_id)
            {
                return Err(RegisterCallError::DuplicateDedupKey {
                    key: key.clone(),
                    call_id: existing_call_id.clone(),
                });
            }
            self.inbound_dedup_index
                .insert(key.clone(), call.call_id.clone());
        }

        self.inbound_calls.insert(call.call_id.clone(), call);
        Ok(())
    }

    pub(crate) fn inbound_for_call(&self, call_id: &CallId) -> Option<&InboundCall> {
        self.inbound_calls.get(call_id)
    }

    pub(crate) fn inbound_for_handle(&self, handle: &RpcInboundCallHandle) -> Option<&InboundCall> {
        self.inbound_for_call(&handle.call_id)
            .filter(|call| inbound_call_matches_handle(call, handle))
    }

    pub(crate) fn inbound_call_is_active_for_handle(&self, handle: &RpcInboundCallHandle) -> bool {
        self.inbound_for_handle(handle)
            .is_some_and(|call| matches!(call.state, InboundCallState::Active))
    }

    pub(crate) fn activate_inbound_for_handle(&mut self, handle: &RpcInboundCallHandle) -> bool {
        let Some(call) = self.inbound_calls.get_mut(&handle.call_id) else {
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

    pub(crate) fn inbound_call_ids_if(
        &self,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<CallId> {
        self.inbound_calls
            .values()
            .filter(|call| predicate(call))
            .map(|call| call.call_id.clone())
            .collect()
    }

    pub(crate) fn inbound_call_target_for_call(
        &self,
        call_id: &CallId,
    ) -> Option<RpcInboundCallTarget> {
        self.inbound_for_call(call_id).map(|call| match call.state {
            InboundCallState::Active => RpcInboundCallTarget::ActiveNoInput {
                method: call.method,
            },
            InboundCallState::Starting | InboundCallState::Closing => {
                RpcInboundCallTarget::NotAccepting {
                    method: call.method,
                    state: call.state,
                }
            }
        })
    }

    pub(crate) fn begin_inbound_closing_for_call_if(
        &mut self,
        call_id: &CallId,
        predicate: impl FnOnce(&InboundCall) -> bool,
    ) -> Option<RpcInboundClosing> {
        let call = self.inbound_calls.get_mut(call_id)?;
        if matches!(call.state, InboundCallState::Closing) || !predicate(call) {
            return None;
        }
        call.state = InboundCallState::Closing;
        call.cancellation.cancel();
        Some(RpcInboundClosing {
            handle: RpcInboundCallHandle {
                call_id: call.call_id.clone(),
                method: call.method,
                generation: call.generation,
            },
        })
    }

    pub(crate) fn begin_inbound_closing_for_handle_if(
        &mut self,
        handle: &RpcInboundCallHandle,
        predicate: impl FnOnce(&InboundCall) -> bool,
    ) -> Option<RpcInboundClosing> {
        self.begin_inbound_closing_for_call_if(&handle.call_id, |call| {
            inbound_call_matches_handle(call, handle) && predicate(call)
        })
    }

    pub(crate) fn finish_inbound_closing(
        &mut self,
        closing: &RpcInboundClosing,
    ) -> Option<InboundCall> {
        let call = self.inbound_calls.get(&closing.handle.call_id)?;
        let generation_matches = call.generation == closing.handle.generation;
        if !matches!(call.state, InboundCallState::Closing)
            || !generation_matches
            || call.method != closing.handle.method
        {
            return None;
        }
        self.remove_inbound_by_id(&closing.handle.call_id)
    }

    pub(crate) fn remove_inbound_for_call_if(
        &mut self,
        call_id: &CallId,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Option<InboundCall> {
        if !self.inbound_calls.get(call_id).is_some_and(&mut predicate) {
            return None;
        }
        self.remove_inbound_by_id(call_id)
    }

    pub(crate) fn remove_inbound_for_handle(
        &mut self,
        handle: &RpcInboundCallHandle,
    ) -> Option<InboundCall> {
        if !self
            .inbound_calls
            .get(&handle.call_id)
            .is_some_and(|call| inbound_call_matches_handle(call, handle))
        {
            return None;
        }
        self.remove_inbound_by_id(&handle.call_id)
    }

    pub(crate) fn remove_inbound_calls_if(
        &mut self,
        mut predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Vec<InboundCall> {
        let call_ids: Vec<_> = self
            .inbound_calls
            .iter()
            .filter_map(|(call_id, call)| predicate(call).then_some(call_id.clone()))
            .collect();
        call_ids
            .into_iter()
            .filter_map(|call_id| self.remove_inbound_by_id(&call_id))
            .collect()
    }

    fn remove_inbound_by_id(&mut self, call_id: &CallId) -> Option<InboundCall> {
        let call = self.inbound_calls.remove(call_id)?;
        call.cancellation.cancel();
        if let Some(key) = &call.dedup_key
            && self
                .inbound_dedup_index
                .get(key)
                .is_some_and(|indexed_call_id| *indexed_call_id == call.call_id)
        {
            self.inbound_dedup_index.remove(key);
        }
        Some(call)
    }

    #[cfg(test)]
    pub(crate) fn dedup_call_id(&self, key: &DedupKey) -> Option<&CallId> {
        self.inbound_dedup_index.get(key)
    }

    pub(crate) fn register_outbound(
        &mut self,
        start: RpcOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        debug_assert_ne!(start.method.kind, MethodKind::ServerStreaming);
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            method: start.method,
            state: start.state,
        })
    }

    pub(crate) fn register_outbound_stream(
        &mut self,
        start: RpcOutboundStart,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        debug_assert_eq!(start.method.kind, MethodKind::ServerStreaming);
        self.register_outbound_tracked(OutboundCall {
            call_id: start.call_id,
            method: start.method,
            state: start.state,
        })
    }

    fn register_outbound_tracked(
        &mut self,
        call: OutboundCall,
    ) -> Result<RpcOutboundCallHandle, RegisterCallError> {
        let handle = RpcOutboundCallHandle {
            call_id: call.call_id.clone(),
            method: call.method,
        };
        let call_id = call.call_id.clone();
        if self.outbound_calls.contains_key(&call_id) || self.inbound_calls.contains_key(&call_id) {
            return Err(RegisterCallError::DuplicateCallId { call_id });
        }
        self.outbound_calls.insert(call.call_id.clone(), call);
        Ok(handle)
    }

    pub(crate) fn outbound_for_call(&self, call_id: &CallId) -> Option<&OutboundCall> {
        self.outbound_calls.get(call_id)
    }

    pub(crate) fn set_outbound_state_for_call(
        &mut self,
        call_id: &CallId,
        state: OutboundCallState,
    ) -> bool {
        let Some(call) = self.outbound_calls.get_mut(call_id) else {
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
        let Some(call) = self.outbound_calls.get_mut(&handle.call_id) else {
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
        let Some(call) = self.outbound_calls.get_mut(&handle.call_id) else {
            return false;
        };
        if !outbound_call_matches_handle(call, handle) || !predicate(call.state) {
            return false;
        }
        call.state = state;
        true
    }

    pub(crate) fn remove_outbound_for_handle(
        &mut self,
        handle: &RpcOutboundCallHandle,
    ) -> Option<OutboundCall> {
        self.remove_outbound_for_call_if(&handle.call_id, |call| {
            outbound_call_matches_handle(call, handle)
        })
    }

    pub(crate) fn remove_outbound_for_call_if(
        &mut self,
        call_id: &CallId,
        mut predicate: impl FnMut(&OutboundCall) -> bool,
    ) -> Option<OutboundCall> {
        if !self.outbound_calls.get(call_id).is_some_and(&mut predicate) {
            return None;
        }
        self.outbound_calls.remove(call_id)
    }

    pub(crate) fn remove_outbound_calls_if(
        &mut self,
        mut predicate: impl FnMut(&OutboundCall) -> bool,
    ) -> Vec<OutboundCall> {
        let call_ids: Vec<_> = self
            .outbound_calls
            .iter()
            .filter_map(|(call_id, call)| predicate(call).then_some(call_id.clone()))
            .collect();
        call_ids
            .into_iter()
            .filter_map(|call_id| self.outbound_calls.remove(&call_id))
            .collect()
    }
}

impl RpcCallDebugSnapshot {
    fn new<const N: usize>(states: [&'static str; N]) -> Self {
        Self {
            total: 0,
            by_state: states.into_iter().map(|state| (state, 0)).collect(),
            by_method: BTreeMap::new(),
        }
    }

    fn record(&mut self, state: &'static str, method: &'static str) {
        self.total += 1;
        *self.by_state.entry(state).or_default() += 1;
        *self.by_method.entry(method).or_default() += 1;
    }
}

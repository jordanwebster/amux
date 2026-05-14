use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tokio::sync::mpsc;

use crate::protocol::Route;
use crate::protocol::link::Link;
use crate::protocol::message::{CallId, Message, ProtocolError, ResponseFrame};
use crate::protocol::method::{self, MethodKind, MethodSpec};
use crate::rpc::{
    DedupKey, InboundCall, InboundCallState, OutboundCall, OutboundCallState, RegisterCallError,
    RpcCallCancellation, RpcDebugSnapshot, RpcInboundCallHandle, RpcInboundCallTarget,
    RpcInboundStart, RpcInboundUnary, RpcOutboundStart, RpcState,
};
use crate::server::{ServerStreamSendError, ServerStreamSink};

#[derive(Clone, Default, Debug)]
pub(crate) struct RpcDispatcher {
    state: Arc<RwLock<RpcState>>,
    resources: Arc<RwLock<RpcEndpointResources>>,
}

#[derive(Default, Debug)]
struct RpcEndpointResources {
    inbound: HashMap<CallId, InboundCallResources>,
    outbound: HashMap<CallId, OutboundCallResources>,
}

pub(crate) enum RpcInboundCloseTarget {
    Absent,
    AlreadyClosing,
    Closing(Box<RpcInboundClosing>),
}

#[derive(Debug)]
pub(crate) struct EndpointUnaryStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) owner_link: Link,
    pub(crate) reply_src: Route,
    pub(crate) reply_dst: Route,
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
}

#[derive(Debug)]
pub(crate) struct EndpointServerStreamStart {
    pub(crate) tx: mpsc::Sender<Message>,
    pub(crate) owner_link: Link,
    pub(crate) reply_src: Route,
    pub(crate) reply_dst: Route,
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) dedup_key: Option<DedupKey>,
}

#[derive(Debug)]
pub(crate) struct LocalOriginOutboundStart {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
    pub(crate) state: OutboundCallState,
    pub(crate) owner_link: Link,
    pub(crate) request_src: Route,
    pub(crate) request_dst: Route,
}

#[derive(Debug)]
pub(crate) struct PeerRoutingOutboundStart {
    pub(crate) link: Link,
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
}

#[derive(Debug)]
pub(crate) struct ServerOriginOutboundStart {
    pub(crate) call_id: CallId,
    pub(crate) method: MethodSpec,
}

#[derive(Debug)]
pub(crate) struct EndpointServerStream {
    pub(crate) handle: RpcInboundCallHandle,
    pub(crate) cancellation: RpcCallCancellation,
    pub(crate) output: ServerStreamSink,
}

#[derive(Debug, Clone)]
pub(crate) struct InboundCallResources {
    pub(crate) owner_link: Link,
    pub(crate) output: ServerStreamSink,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveEndpointStreamSink {
    pub(crate) handle: RpcInboundCallHandle,
    pub(crate) owner_link: Link,
    pub(crate) output: ServerStreamSink,
}

#[derive(Debug, Clone)]
pub(crate) enum OutboundCallResources {
    LocalOriginRouted {
        owner_link: Link,
        request_src: Route,
        request_dst: Route,
    },
    PeerRoutingSubscription {
        link: Link,
    },
    ServerOriginRouted,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalOriginOutboundCall {
    pub(crate) call_id: CallId,
    pub(crate) owner_link: Link,
    pub(crate) request_src: Route,
    pub(crate) request_dst: Route,
}

#[derive(Debug, Clone)]
pub(crate) struct RpcInboundClosing {
    pub(crate) handle: RpcInboundCallHandle,
    output: ServerStreamSink,
    rpc_closing: crate::rpc::RpcInboundClosing,
}

impl RpcInboundClosing {
    pub(crate) async fn send_response(
        &self,
        response: ResponseFrame,
    ) -> Result<(), ServerStreamSendError> {
        self.output.send_response(response).await
    }

    pub(crate) async fn send_empty_response_result(
        &self,
        result: Result<(), ProtocolError>,
    ) -> Result<(), ServerStreamSendError> {
        self.output.send_empty_response_result(result).await
    }

    pub(crate) async fn with_send_gate<F, Fut, T>(&self, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        self.output.with_send_gate(f).await
    }
}

impl OutboundCallResources {
    fn local_origin(&self) -> Option<(&Link, &Route, &Route)> {
        match self {
            Self::LocalOriginRouted {
                owner_link,
                request_src,
                request_dst,
            } => Some((owner_link, request_src, request_dst)),
            Self::PeerRoutingSubscription { .. } | Self::ServerOriginRouted => None,
        }
    }

    fn into_local_origin(self, call_id: CallId) -> Option<LocalOriginOutboundCall> {
        let Self::LocalOriginRouted {
            owner_link,
            request_src,
            request_dst,
        } = self
        else {
            return None;
        };
        Some(LocalOriginOutboundCall {
            call_id,
            owner_link,
            request_src,
            request_dst,
        })
    }
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

    fn read_resources(&self) -> RwLockReadGuard<'_, RpcEndpointResources> {
        self.resources.read().expect("RPC resource lock poisoned")
    }

    fn write_resources(&self) -> RwLockWriteGuard<'_, RpcEndpointResources> {
        self.resources.write().expect("RPC resource lock poisoned")
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
        let mut resources = self.write_resources();
        resources.inbound.clear();
        resources.outbound.clear();
    }

    pub(in crate::server) fn register_endpoint_unary(
        &self,
        start: EndpointUnaryStart,
    ) -> Result<RpcInboundUnary, RegisterCallError> {
        let output = ServerStreamSink::new(
            start.tx,
            start.reply_src,
            start.reply_dst,
            start.call_id.clone(),
        );
        let call = self.write().register_inbound_unary(RpcInboundStart {
            call_id: start.call_id.clone(),
            method: start.method,
            dedup_key: None,
        })?;
        self.write_resources().inbound.insert(
            start.call_id,
            InboundCallResources {
                owner_link: start.owner_link,
                output,
            },
        );
        Ok(call)
    }

    pub(in crate::server) fn register_endpoint_server_stream(
        &self,
        start: EndpointServerStreamStart,
    ) -> Result<EndpointServerStream, RegisterCallError> {
        let output = ServerStreamSink::new(
            start.tx,
            start.reply_src,
            start.reply_dst,
            start.call_id.clone(),
        );
        let call = self
            .write()
            .register_inbound_server_stream(RpcInboundStart {
                call_id: start.call_id.clone(),
                method: start.method,
                dedup_key: start.dedup_key,
            })?;
        self.write_resources().inbound.insert(
            start.call_id,
            InboundCallResources {
                owner_link: start.owner_link,
                output: output.clone(),
            },
        );
        Ok(EndpointServerStream {
            handle: call.handle,
            cancellation: call.cancellation,
            output,
        })
    }

    pub(in crate::server) fn register_local_origin_outbound(
        &self,
        start: LocalOriginOutboundStart,
    ) -> Result<(), RegisterCallError> {
        let rpc_start = RpcOutboundStart {
            call_id: start.call_id.clone(),
            method: start.method,
            state: start.state,
        };
        match start.method.kind {
            MethodKind::Unary => {
                self.write().register_outbound(rpc_start)?;
            }
            MethodKind::ServerStreaming => {
                self.write().register_outbound_stream(rpc_start)?;
            }
        }
        self.write_resources().outbound.insert(
            start.call_id,
            OutboundCallResources::LocalOriginRouted {
                owner_link: start.owner_link,
                request_src: start.request_src,
                request_dst: start.request_dst,
            },
        );
        Ok(())
    }

    pub(in crate::server) fn register_peer_routing_outbound(
        &self,
        start: PeerRoutingOutboundStart,
    ) -> Result<(), RegisterCallError> {
        self.write().register_outbound_stream(RpcOutboundStart {
            call_id: start.call_id.clone(),
            method: start.method,
            state: OutboundCallState::AwaitingResponse,
        })?;
        self.write_resources().outbound.insert(
            start.call_id,
            OutboundCallResources::PeerRoutingSubscription { link: start.link },
        );
        Ok(())
    }

    pub(in crate::server) fn register_server_origin_outbound(
        &self,
        start: ServerOriginOutboundStart,
    ) -> Result<(), RegisterCallError> {
        self.write().register_outbound_stream(RpcOutboundStart {
            call_id: start.call_id.clone(),
            method: start.method,
            state: OutboundCallState::AwaitingResponse,
        })?;
        self.write_resources()
            .outbound
            .insert(start.call_id, OutboundCallResources::ServerOriginRouted);
        Ok(())
    }

    pub(in crate::server) fn active_endpoint_stream_sinks_for_method(
        &self,
        method: MethodSpec,
    ) -> Vec<ActiveEndpointStreamSink> {
        let calls = {
            let state = self.read();
            state
                .inbound_call_ids_if(|call| {
                    call.method == method && call.state == InboundCallState::Active
                })
                .into_iter()
                .filter_map(|call_id| state.inbound_for_call(&call_id).cloned())
                .collect::<Vec<_>>()
        };
        let resources = self.read_resources();
        calls
            .into_iter()
            .filter_map(|call| {
                let resource = resources.inbound.get(&call.call_id)?;
                Some(ActiveEndpointStreamSink {
                    handle: RpcInboundCallHandle {
                        call_id: call.call_id,
                        method: call.method,
                        generation: call.generation,
                    },
                    owner_link: resource.owner_link.clone(),
                    output: resource.output.clone(),
                })
            })
            .collect()
    }

    pub(in crate::server) fn outbound_for_call_matches(
        &self,
        call_id: &CallId,
        predicate: impl FnOnce(&OutboundCall, Option<&OutboundCallResources>) -> bool,
    ) -> bool {
        let state = self.read();
        let resources = self.read_resources();
        state
            .outbound_for_call(call_id)
            .is_some_and(|call| predicate(call, resources.outbound.get(call_id)))
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

    pub(crate) fn inbound_call_target_for_call(
        &self,
        call_id: &CallId,
    ) -> Option<RpcInboundCallTarget> {
        self.read().inbound_call_target_for_call(call_id)
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
        self.read_resources().inbound.get(call_id).cloned()
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
        let call = self.write().remove_inbound_for_handle(handle)?;
        self.write_resources().inbound.remove(&call.call_id);
        Some(call)
    }

    pub(in crate::server) fn inbound_call_is_active_for_handle(
        &self,
        handle: &RpcInboundCallHandle,
    ) -> bool {
        self.read().inbound_call_is_active_for_handle(handle)
    }

    #[cfg(test)]
    pub(in crate::server) fn begin_inbound_closing_for_call_if(
        &self,
        call_id: &CallId,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        self.begin_inbound_closing_for_call_inner(call_id, predicate)
    }

    pub(in crate::server) fn begin_inbound_closing_for_call(
        &self,
        call_id: &CallId,
    ) -> RpcInboundCloseTarget {
        let Some(resource) = self.read_resources().inbound.get(call_id).cloned() else {
            let state = self.read();
            return if state
                .inbound_for_call(call_id)
                .is_some_and(|call| matches!(call.state, InboundCallState::Closing))
            {
                RpcInboundCloseTarget::AlreadyClosing
            } else {
                RpcInboundCloseTarget::Absent
            };
        };

        let mut state = self.write();
        if let Some(closing) = state.begin_inbound_closing_for_call_if(call_id, |_| true) {
            return RpcInboundCloseTarget::Closing(Box::new(RpcInboundClosing {
                handle: closing.handle.clone(),
                output: resource.output,
                rpc_closing: closing,
            }));
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
        let resources = self.read_resources().inbound.clone();
        let mut state = self.write();
        let call_ids = state.inbound_call_ids_if(|call| {
            resources
                .get(&call.call_id)
                .is_some_and(|resources| predicate(call, resources))
        });
        call_ids
            .into_iter()
            .filter_map(|call_id| {
                let resource = resources.get(&call_id)?.clone();
                let closing = state.begin_inbound_closing_for_call_if(&call_id, |_| true)?;
                Some(RpcInboundClosing {
                    handle: closing.handle.clone(),
                    output: resource.output,
                    rpc_closing: closing,
                })
            })
            .collect()
    }

    pub(in crate::server) fn begin_inbound_closing_for_handle_if(
        &self,
        handle: &RpcInboundCallHandle,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        let resource = self
            .read_resources()
            .inbound
            .get(&handle.call_id)
            .cloned()?;
        let closing = self
            .write()
            .begin_inbound_closing_for_handle_if(handle, |call| predicate(call, &resource))?;
        Some(RpcInboundClosing {
            handle: closing.handle.clone(),
            output: resource.output,
            rpc_closing: closing,
        })
    }

    #[cfg(test)]
    fn begin_inbound_closing_for_call_inner(
        &self,
        call_id: &CallId,
        predicate: impl FnOnce(&InboundCall, &InboundCallResources) -> bool,
    ) -> Option<RpcInboundClosing> {
        let resource = self.read_resources().inbound.get(call_id).cloned()?;
        let closing = self
            .write()
            .begin_inbound_closing_for_call_if(call_id, |call| predicate(call, &resource))?;
        Some(RpcInboundClosing {
            handle: closing.handle.clone(),
            output: resource.output,
            rpc_closing: closing,
        })
    }

    pub(in crate::server) fn finish_inbound_closing(
        &self,
        closing: &RpcInboundClosing,
    ) -> Option<InboundCall> {
        let call = self.write().finish_inbound_closing(&closing.rpc_closing)?;
        self.write_resources().inbound.remove(&call.call_id);
        Some(call)
    }

    pub(in crate::server) fn finish_outbound_peer_routing_subscription(
        &self,
        link: &Link,
        call_id: &CallId,
    ) -> bool {
        self.remove_outbound_for_call_if(call_id, |call, resources| {
            call.method == method::ROUTING_SUBSCRIBE_EVENTS
                && matches!(
                    resources,
                    Some(OutboundCallResources::PeerRoutingSubscription { link: call_link })
                        if call_link == link
                )
        })
        .is_some()
    }

    pub(in crate::server) fn finish_inbound_peer_routing_subscription(
        &self,
        link: &Link,
        call_id: &CallId,
    ) -> bool {
        self.remove_inbound_for_call_if(call_id, |call| {
            call.method == method::ROUTING_SUBSCRIBE_EVENTS
                && call.dedup_key == Some(routing_subscription_dedup_key(link))
        })
        .is_some()
    }

    pub(in crate::server) fn remove_inbound_for_call_if(
        &self,
        call_id: &CallId,
        predicate: impl FnMut(&InboundCall) -> bool,
    ) -> Option<InboundCall> {
        let call = self
            .write()
            .remove_inbound_for_call_if(call_id, predicate)?;
        self.write_resources().inbound.remove(&call.call_id);
        Some(call)
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_return_hop(
        &self,
        call_id: &CallId,
        owner_link: &Link,
        response_route: &Route,
    ) -> Option<OutboundCall> {
        self.remove_outbound_for_call_if(call_id, |call, resources| {
            call.method.access == method::MethodAccess::Routed
                && resources
                    .and_then(OutboundCallResources::local_origin)
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
        self.remove_outbound_for_call_if(call_id, |call, resources| {
            call.method.access == method::MethodAccess::Routed
                && resources
                    .and_then(OutboundCallResources::local_origin)
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
        self.remove_outbound_for_call_if(call_id, |_, resources| {
            resources
                .and_then(OutboundCallResources::local_origin)
                .is_some_and(|(_, request_src, request_dst)| {
                    local_origin_request_route_matches(request_src, request_dst, failed_route)
                })
        })
    }

    pub(in crate::server) fn remove_server_origin_outbound(
        &self,
        call_id: &CallId,
        method: MethodSpec,
    ) -> Option<OutboundCall> {
        self.remove_outbound_for_call_if(call_id, |call, resources| {
            call.method == method
                && matches!(resources, Some(OutboundCallResources::ServerOriginRouted))
        })
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_owner_link(
        &self,
        owner_link: &Link,
    ) -> Vec<LocalOriginOutboundCall> {
        self.remove_outbound_calls_if(|_, resources| {
            resources
                .and_then(OutboundCallResources::local_origin)
                .is_some_and(|(call_owner_link, _, _)| call_owner_link == owner_link)
        })
        .into_iter()
        .filter_map(|call| {
            self.write_resources()
                .outbound
                .remove(&call.call_id)
                .and_then(|resources| resources.into_local_origin(call.call_id))
        })
        .collect()
    }

    pub(in crate::server) fn remove_local_origin_outbound_for_route_prefix(
        &self,
        route_prefix: &Route,
    ) -> Vec<LocalOriginOutboundCall> {
        self.remove_outbound_calls_if(|_, resources| {
            resources
                .and_then(OutboundCallResources::local_origin)
                .is_some_and(|(_, _, request_dst)| request_dst.starts_with_route(route_prefix))
        })
        .into_iter()
        .filter_map(|call| {
            self.write_resources()
                .outbound
                .remove(&call.call_id)
                .and_then(|resources| resources.into_local_origin(call.call_id))
        })
        .collect()
    }

    pub(in crate::server) fn remove_inbound_for_owner_link_except_method(
        &self,
        owner_link: &Link,
        excluded_method: MethodSpec,
    ) -> Vec<InboundCall> {
        let resources = self.read_resources().inbound.clone();
        let calls = self.write().remove_inbound_calls_if(|call| {
            call.method != excluded_method
                && resources
                    .get(&call.call_id)
                    .is_some_and(|resources| resources.owner_link == *owner_link)
        });
        let mut output_resources = self.write_resources();
        for call in &calls {
            output_resources.inbound.remove(&call.call_id);
        }
        calls
    }

    fn remove_outbound_for_call_if(
        &self,
        call_id: &CallId,
        mut predicate: impl FnMut(&OutboundCall, Option<&OutboundCallResources>) -> bool,
    ) -> Option<OutboundCall> {
        let resource = self.read_resources().outbound.get(call_id).cloned();
        let call = self
            .write()
            .remove_outbound_for_call_if(call_id, |call| predicate(call, resource.as_ref()))?;
        self.write_resources().outbound.remove(&call.call_id);
        Some(call)
    }

    fn remove_outbound_calls_if(
        &self,
        mut predicate: impl FnMut(&OutboundCall, Option<&OutboundCallResources>) -> bool,
    ) -> Vec<OutboundCall> {
        let resources = self.read_resources().outbound.clone();
        self.write()
            .remove_outbound_calls_if(|call| predicate(call, resources.get(&call.call_id)))
    }
}

/// Dedup key for a routing-event subscription bound to a particular link.
/// Prevents a single connection from opening multiple concurrent routing
/// subscriptions. Applies uniformly to peer and local connections.
pub(crate) fn routing_subscription_dedup_key(link: &Link) -> DedupKey {
    DedupKey::new("routing-subscription", link.as_str())
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

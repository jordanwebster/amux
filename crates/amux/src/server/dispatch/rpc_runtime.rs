use prost::Message as _;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::protocol::Route;
use crate::protocol::message::{
    CallId, Frame, FrameBody, Message, ProtocolError, RequestFrame, ResponseFrame,
};
use crate::protocol::method::{self, MethodAccess, MethodLookupError, MethodSpec};
use crate::protocol::wire::{
    AgentLifecycleRequest, AgentLifecycleResponse, AgentRecord, decode_agent_event,
    decode_agent_lifecycle_request_payload, encode_agent_lifecycle_response_frame,
};
use crate::rpc::{
    DedupKey, InboundCallState, OutboundCallState, RegisterCallError, RpcInboundCallTarget,
    RpcInboundUnary,
};
use crate::server::connection::ConnectionContext;
use crate::server::{
    EndpointServerStream, EndpointServerStreamStart, EndpointUnaryStart, LocalOriginOutboundStart,
    OutboundCallResources, RpcDispatcher, RpcInboundCloseTarget, RpcInboundClosing,
    ServerStreamSendError, send_terminal_and_finish_session_subscription,
    session_subscription_closing_from_rpc_closing,
};
use crate::services::{AgentService, AgentServiceCtx, SubscribeSessionCall};

fn reply_routes(src: Route, msg_type: &str) -> Option<(Route, Route)> {
    match Route::reply(src) {
        Some(routes) => Some(routes),
        None => {
            tracing::warn!(msg_type, "dropping application frame with empty src route");
            None
        }
    }
}

fn frame_message(src: Route, dst: Route, call_id: CallId, body: FrameBody) -> Message {
    Message::Frame(Frame {
        src,
        dst,
        call_id,
        body,
    })
}

pub(super) enum LocalRequestTracking {
    Continue,
    Reject {
        failed_route: Route,
        error: ProtocolError,
    },
}

pub(super) async fn track_forwarded_local_request_if_any(
    src: &Route,
    dst: &Route,
    call_id: &CallId,
    body: &FrameBody,
    ctx: &ConnectionContext,
) -> LocalRequestTracking {
    if !ctx.is_local || dst.is_empty() || src.peek() != Some(&ctx.link) {
        return LocalRequestTracking::Continue;
    }
    if ctx.state.read().await.is_cloud_server() {
        return LocalRequestTracking::Continue;
    }
    let FrameBody::Request(request) = body else {
        return LocalRequestTracking::Continue;
    };
    let Some(spec) =
        method::find(&request.method).filter(|spec| spec.access == MethodAccess::Routed)
    else {
        return LocalRequestTracking::Continue;
    };
    let Some(counterparty_route) = route_from_src_and_dst(src, dst) else {
        return LocalRequestTracking::Continue;
    };

    let Some(rpc) = ctx.user_state.read().await.rpc_for_outbound_route(dst) else {
        return LocalRequestTracking::Reject {
            failed_route: counterparty_route,
            error: ProtocolError::Unreachable {
                message: format!("route not found: {dst}"),
            },
        };
    };

    match rpc.register_local_origin_outbound(LocalOriginOutboundStart {
        call_id: call_id.clone(),
        method: spec,
        state: OutboundCallState::AwaitingResponse,
        owner_link: ctx.link.clone(),
        request_src: src.clone(),
        request_dst: dst.clone(),
    }) {
        Ok(_) => LocalRequestTracking::Continue,
        Err(error) => {
            tracing::warn!(
                error = ?error,
                call_id = ?call_id.as_bytes(),
                method = request.method,
                "rejecting duplicate local-origin forwarded request"
            );
            LocalRequestTracking::Reject {
                failed_route: counterparty_route,
                error: local_origin_registration_error(error),
            }
        }
    }
}

fn local_origin_registration_error(error: RegisterCallError) -> ProtocolError {
    match error {
        RegisterCallError::DuplicateCallId { call_id } => ProtocolError::AlreadyExists {
            message: format!(
                "duplicate active forwarded local call {:?}",
                call_id.as_bytes()
            ),
        },
        RegisterCallError::DuplicateDedupKey { call_id, .. } => ProtocolError::AlreadyExists {
            message: format!(
                "duplicate active forwarded local call {:?}",
                call_id.as_bytes()
            ),
        },
    }
}

fn route_from_src_and_dst(src: &Route, dst: &Route) -> Option<Route> {
    Route::from_links(
        src.iter()
            .chain(dst.iter())
            .map(|link| link.as_str().to_string()),
    )
    .ok()
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, crate::protocol::wire::DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        crate::protocol::wire::DecodeError::Invalid(format!(
            "{name} must be 16 bytes, got {}",
            bytes.len()
        ))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

async fn endpoint_rpc(
    ctx: &ConnectionContext,
    counterparty: &Route,
    call_id: &CallId,
) -> Option<RpcDispatcher> {
    let us = ctx.user_state.read().await;
    us.rpc_for_inbound_call(call_id)
        .or_else(|| us.route_rpc_for_counterparty(counterparty))
        .or_else(|| (ctx.is_local || counterparty.peek() == Some(&ctx.link)).then(|| ctx.rpc()))
}

enum EndpointCallCloseTarget {
    Absent,
    AlreadyClosing,
    SessionSubscription(crate::server::session_subscription_lifecycle::SessionSubscriptionClosing),
    Generic(RpcInboundClosing),
}

struct EndpointCall<'a> {
    tx: &'a mpsc::Sender<Message>,
    ctx: &'a ConnectionContext,
    rpc: RpcDispatcher,
    counterparty: Route,
    call_id: CallId,
    reply_src: Route,
    reply_dst: Route,
}

impl<'a> EndpointCall<'a> {
    fn new(
        tx: &'a mpsc::Sender<Message>,
        ctx: &'a ConnectionContext,
        rpc: RpcDispatcher,
        counterparty: Route,
        call_id: CallId,
        msg_type: &str,
    ) -> Option<Self> {
        let (reply_src, reply_dst) = reply_routes(counterparty.clone(), msg_type)?;
        Some(Self {
            tx,
            ctx,
            rpc,
            counterparty,
            call_id,
            reply_src,
            reply_dst,
        })
    }

    async fn send_error(&self, error: ProtocolError) -> crate::server::connection::Result<()> {
        send_endpoint_error_response(
            self.tx,
            self.reply_src.clone(),
            self.reply_dst.clone(),
            self.call_id.clone(),
            error,
        )
        .await
    }

    async fn terminate_existing_or_send(
        &self,
        error: ProtocolError,
    ) -> crate::server::connection::Result<()> {
        terminate_endpoint_call_if_currently_present_or_send(self, error).await
    }

    async fn register_unary(
        &self,
        method: MethodSpec,
    ) -> Result<RpcInboundUnary, RegisterCallError> {
        self.rpc.register_endpoint_unary(EndpointUnaryStart {
            tx: self.tx.clone(),
            owner_link: self.ctx.link.clone(),
            reply_src: self.reply_src.clone(),
            reply_dst: self.reply_dst.clone(),
            call_id: self.call_id.clone(),
            method,
        })
    }

    async fn register_server_stream(
        &self,
        method: MethodSpec,
        dedup_key: Option<DedupKey>,
    ) -> Result<EndpointServerStream, RegisterCallError> {
        self.rpc
            .register_endpoint_server_stream(EndpointServerStreamStart {
                tx: self.tx.clone(),
                owner_link: self.ctx.link.clone(),
                reply_src: self.reply_src.clone(),
                reply_dst: self.reply_dst.clone(),
                call_id: self.call_id.clone(),
                method,
                dedup_key,
            })
    }

    async fn finish_unary_response(
        &self,
        call: RpcInboundUnary,
        response: ResponseFrame,
    ) -> crate::server::connection::Result<()> {
        let closing = self
            .rpc()
            .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true);
        let Some(closing) = closing else {
            return Ok(());
        };

        let result = closing.send_response(response).await;
        self.rpc.finish_inbound_closing(&closing);
        result.map_err(endpoint_response_send_error)
    }

    fn rpc(&self) -> RpcDispatcher {
        self.rpc.clone()
    }
}

async fn terminate_endpoint_call_if_currently_present_or_send(
    endpoint: &EndpointCall<'_>,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let close_target = match endpoint
        .rpc()
        .begin_inbound_closing_for_call(&endpoint.call_id)
    {
        RpcInboundCloseTarget::Closing(closing)
            if closing.handle.method == method::AGENT_SUBSCRIBE_SESSION =>
        {
            match session_subscription_closing_from_rpc_closing(endpoint.rpc(), closing) {
                Some(closing) => EndpointCallCloseTarget::SessionSubscription(closing),
                None => EndpointCallCloseTarget::AlreadyClosing,
            }
        }
        RpcInboundCloseTarget::Closing(closing) => EndpointCallCloseTarget::Generic(closing),
        RpcInboundCloseTarget::AlreadyClosing => EndpointCallCloseTarget::AlreadyClosing,
        RpcInboundCloseTarget::Absent => EndpointCallCloseTarget::Absent,
    };

    match close_target {
        EndpointCallCloseTarget::Absent => endpoint.send_error(error).await,
        EndpointCallCloseTarget::AlreadyClosing => Ok(()),
        EndpointCallCloseTarget::SessionSubscription(closing) => {
            send_terminal_and_finish_session_subscription(
                &endpoint.ctx.user_state,
                closing,
                Err(error),
            )
            .await
            .map_err(endpoint_response_send_error)
        }
        EndpointCallCloseTarget::Generic(closing) => {
            let result = closing.send_empty_response_result(Err(error)).await;
            endpoint.rpc.finish_inbound_closing(&closing);
            result.map_err(endpoint_response_send_error)
        }
    }
}

fn endpoint_response_send_error(error: ServerStreamSendError) -> crate::server::ConnectionError {
    crate::server::connection::ConnectionError::Config(format!(
        "failed to send endpoint error response: {error}"
    ))
}

pub(super) async fn handle_endpoint_frame(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    body: FrameBody,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let rpc = endpoint_rpc(ctx, &counterparty, &call_id).await;
    if let Some(rpc) = rpc.clone()
        && let Some(target) = rpc.inbound_call_target_for_call(&call_id)
    {
        return match target {
            RpcInboundCallTarget::ActiveNoInput { method } => {
                handle_active_endpoint_no_input_followup(
                    tx,
                    counterparty,
                    call_id,
                    method,
                    body,
                    ctx,
                    rpc,
                )
                .await
            }
            RpcInboundCallTarget::NotAccepting {
                method: call_method,
                state,
            } => {
                if matches!(state, InboundCallState::Starting)
                    && call_method == method::AGENT_SUBSCRIBE_SESSION
                    && matches!(body, FrameBody::Cancel)
                {
                    return handle_active_endpoint_no_input_followup(
                        tx,
                        counterparty,
                        call_id,
                        call_method,
                        body,
                        ctx,
                        rpc,
                    )
                    .await;
                }
                drop_endpoint_frame_for_inactive_call(&counterparty, &call_id, state, &body);
                Ok(())
            }
        };
    }

    if let Some(rpc) = rpc.clone()
        && handle_server_origin_agent_subscription_frame(rpc, &counterparty, &call_id, &body, ctx)
            .await?
    {
        return Ok(());
    }

    match body {
        FrameBody::Request(request) => {
            let Some(rpc) = rpc else {
                send_endpoint_error_response_for_counterparty(
                    tx,
                    counterparty,
                    call_id,
                    "EndpointRequest",
                    ProtocolError::Unreachable {
                        message: "unknown endpoint frame source".to_string(),
                    },
                )
                .await?;
                return Ok(());
            };
            handle_endpoint_request(tx, counterparty, call_id, request, ctx, rpc).await
        }
        FrameBody::StreamItem(payload) => {
            drop_stale_endpoint_stream_item(&counterparty, &call_id, payload);
            Ok(())
        }
        FrameBody::Cancel => {
            drop_stale_endpoint_cancel(&counterparty, &call_id);
            Ok(())
        }
        FrameBody::Response(_) => {
            tracing::debug!(
                counterparty = %counterparty,
                call_id = ?call_id.as_bytes(),
                "dropping stale endpoint response for inactive call"
            );
            Ok(())
        }
        FrameBody::RoutingError { .. } => Ok(()),
    }
}

async fn handle_endpoint_request(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    request: RequestFrame,
    ctx: &ConnectionContext,
    rpc: RpcDispatcher,
) -> crate::server::connection::Result<()> {
    let Some(endpoint) = EndpointCall::new(tx, ctx, rpc, counterparty, call_id, "EndpointRequest")
    else {
        return Ok(());
    };

    let spec = match method::find_for_scope(&request.method, MethodAccess::Routed) {
        Ok(spec) => spec,
        Err(MethodLookupError::WrongScope {
            spec,
            requested_scope,
        }) => {
            return endpoint
                .send_error(ProtocolError::PermissionDenied {
                    message: format!(
                        "method {} is {} scoped and not valid in {} scope",
                        request.method,
                        spec.access.as_str(),
                        requested_scope.as_str()
                    ),
                })
                .await;
        }
        Err(MethodLookupError::Unknown) => {
            return endpoint
                .send_error(ProtocolError::Unimplemented {
                    message: format!("unknown endpoint method {}", request.method),
                })
                .await;
        }
    };

    match spec.kind {
        method::MethodKind::Unary => handle_endpoint_unary_request(&endpoint, spec, request).await,
        method::MethodKind::ServerStreaming => {
            handle_endpoint_server_stream_request(&endpoint, spec, request).await
        }
    }
}

async fn handle_endpoint_server_stream_request(
    endpoint: &EndpointCall<'_>,
    spec: MethodSpec,
    request: RequestFrame,
) -> crate::server::connection::Result<()> {
    match spec.name {
        method::AGENT_SUBSCRIBE_EVENTS_NAME => {
            let request = match crate::protocol::wire::SubscribeAgentEventsRequest::decode(
                request.payload.as_slice(),
            ) {
                Ok(request) => request,
                Err(error) => {
                    return endpoint
                        .send_error(ProtocolError::InvalidArgument {
                            message: format!("invalid SubscribeAgentEvents request: {error}"),
                        })
                        .await;
                }
            };
            let host_id = match uuid_from_bytes("host_id", request.host_id) {
                Ok(host_id) => host_id,
                Err(error) => {
                    return endpoint
                        .send_error(ProtocolError::InvalidArgument {
                            message: error.to_string(),
                        })
                        .await;
                }
            };
            handle_subscribe_agent_events_request(endpoint, spec, host_id).await
        }
        method::AGENT_SUBSCRIBE_SESSION_NAME => {
            let request = match crate::protocol::wire::decode_subscribe_session_request(&request) {
                Ok(request) => request,
                Err(error) => {
                    return endpoint
                        .send_error(ProtocolError::InvalidArgument {
                            message: error.to_string(),
                        })
                        .await;
                }
            };
            let call = endpoint.register_server_stream(spec, None).await;
            let call = match call {
                Ok(call) => call,
                Err(error) => {
                    return endpoint.send_error(duplicate_call_error(error)).await;
                }
            };
            let call =
                SubscribeSessionCall::from_rpc(call, endpoint.counterparty.clone(), endpoint.rpc())
                    .expect("SubscribeSession dispatch registered non-SubscribeSession RPC call");
            let agent_ctx = agent_service_ctx(endpoint.ctx).await;
            tokio::spawn(async move {
                if let Err(error) = AgentService::subscribe_session(call, request, &agent_ctx).await
                {
                    tracing::warn!(error = %error, "SubscribeSession service task failed");
                }
            });
            Ok(())
        }
        method => send_unsupported_endpoint_method(endpoint, method).await,
    }
}

async fn handle_server_origin_agent_subscription_frame(
    rpc: RpcDispatcher,
    counterparty: &Route,
    call_id: &CallId,
    body: &FrameBody,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<bool> {
    let active = rpc.outbound_for_call_matches(call_id, |call, resources| {
        call.method == method::AGENT_SUBSCRIBE_EVENTS
            && matches!(resources, Some(OutboundCallResources::ServerOriginRouted))
            && matches!(
                call.state,
                OutboundCallState::AwaitingResponse | OutboundCallState::ActiveStream
            )
    });
    if !active {
        return Ok(false);
    };
    let Some(subscribed_host_id) = ctx
        .user_state
        .read()
        .await
        .agent_subscription_host_for_route_and_call(call_id, counterparty)
    else {
        tracing::warn!(
            route = %counterparty,
            call_id = ?call_id.as_bytes(),
            "dropping agent subscription frame without matching routing state"
        );
        return Ok(false);
    };

    match body {
        FrameBody::StreamItem(payload) => {
            rpc.set_outbound_state_for_call(call_id, OutboundCallState::ActiveStream);
            let event = match decode_agent_event(payload) {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!(
                        route = %counterparty,
                        error = %error,
                        "dropping malformed agent subscription event"
                    );
                    return Ok(true);
                }
            };
            super::peer::handle_agent_event(event, subscribed_host_id, ctx).await?;
            Ok(true)
        }
        FrameBody::Response(response) => {
            if let ResponseFrame::Error(error) = response {
                tracing::warn!(
                    route = %counterparty,
                    error = %error,
                    "agent subscription failed"
                );
            } else {
                tracing::debug!(route = %counterparty, "agent subscription completed");
            }
            rpc.remove_server_origin_outbound(call_id, method::AGENT_SUBSCRIBE_EVENTS);
            ctx.user_state
                .write()
                .await
                .clear_agent_subscription_for_route(call_id, counterparty);
            Ok(true)
        }
        FrameBody::Cancel => {
            rpc.remove_server_origin_outbound(call_id, method::AGENT_SUBSCRIBE_EVENTS);
            ctx.user_state
                .write()
                .await
                .clear_agent_subscription_for_route(call_id, counterparty);
            Ok(true)
        }
        FrameBody::Request(_) | FrameBody::RoutingError { .. } => Ok(false),
    }
}

async fn handle_endpoint_unary_request(
    endpoint: &EndpointCall<'_>,
    spec: MethodSpec,
    request: RequestFrame,
) -> crate::server::connection::Result<()> {
    match spec.name {
        method::AGENT_CREATE_NAME | method::AGENT_RENAME_NAME | method::AGENT_DELETE_NAME => {
            match decode_agent_lifecycle_request_payload(&request.method, &request.payload) {
                Ok(request) => handle_agent_lifecycle_request(endpoint, request).await,
                Err(error) => {
                    tracing::warn!(
                        method = request.method,
                        error = %error,
                        "failed to decode protobuf agent lifecycle payload"
                    );
                    endpoint
                        .send_error(ProtocolError::InvalidArgument {
                            message: error.to_string(),
                        })
                        .await
                }
            }
        }
        method::AGENT_SEND_INPUT_NAME => {
            let request = match crate::protocol::wire::decode_send_input_request(&request) {
                Ok(request) => request,
                Err(error) => {
                    return endpoint
                        .send_error(ProtocolError::InvalidArgument {
                            message: error.to_string(),
                        })
                        .await;
                }
            };
            handle_send_input_request(endpoint, spec, request).await
        }
        method => send_unsupported_endpoint_method(endpoint, method).await,
    }
}

async fn send_unsupported_endpoint_method(
    endpoint: &EndpointCall<'_>,
    method: &str,
) -> crate::server::connection::Result<()> {
    tracing::warn!(method, "unsupported endpoint protobuf request");
    endpoint
        .send_error(ProtocolError::Unimplemented {
            message: format!("endpoint method {method} is not implemented"),
        })
        .await
}

async fn handle_active_endpoint_no_input_followup(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    method: MethodSpec,
    body: FrameBody,
    ctx: &ConnectionContext,
    rpc: RpcDispatcher,
) -> crate::server::connection::Result<()> {
    let Some(endpoint) =
        EndpointCall::new(tx, ctx, rpc, counterparty, call_id, "ActiveEndpointUnary")
    else {
        return Ok(());
    };
    let error = match body {
        FrameBody::Cancel => ProtocolError::Cancelled {
            message: format!("endpoint call {} was cancelled", method.name),
        },
        body => ProtocolError::InvalidArgument {
            message: format!(
                "endpoint {} frame is not valid for active non-client-streaming call {}",
                frame_body_kind(&body),
                method.name
            ),
        },
    };
    endpoint.terminate_existing_or_send(error).await
}

fn duplicate_call_error(error: RegisterCallError) -> ProtocolError {
    match error {
        RegisterCallError::DuplicateCallId { call_id } => ProtocolError::AlreadyExists {
            message: format!("duplicate active call {:?}", call_id.as_bytes()),
        },
        RegisterCallError::DuplicateDedupKey { key, call_id, .. } => ProtocolError::AlreadyExists {
            message: format!(
                "duplicate active call for dedup key {key:?}; existing call id {:?}",
                call_id.as_bytes()
            ),
        },
    }
}

fn frame_body_kind(body: &FrameBody) -> &'static str {
    match body {
        FrameBody::Request(_) => "request",
        FrameBody::Response(_) => "response",
        FrameBody::StreamItem(_) => "stream_item",
        FrameBody::Cancel => "cancel",
        FrameBody::RoutingError { .. } => "routing_error",
    }
}

fn drop_stale_endpoint_stream_item(counterparty: &Route, call_id: &CallId, _payload: Vec<u8>) {
    tracing::debug!(
        counterparty = %counterparty,
        call_id = ?call_id.as_bytes(),
        "dropping stale endpoint stream item for inactive call"
    );
}

fn drop_stale_endpoint_cancel(counterparty: &Route, call_id: &CallId) {
    tracing::debug!(
        counterparty = %counterparty,
        call_id = ?call_id.as_bytes(),
        "dropping stale endpoint cancel for inactive call"
    );
}

fn drop_endpoint_frame_for_inactive_call(
    counterparty: &Route,
    call_id: &CallId,
    state: InboundCallState,
    body: &FrameBody,
) {
    tracing::debug!(
        counterparty = %counterparty,
        call_id = ?call_id.as_bytes(),
        state = ?state,
        body = frame_body_kind(body),
        "dropping route-scoped frame for inactive inbound call"
    );
}

async fn handle_agent_lifecycle_request(
    endpoint: &EndpointCall<'_>,
    request: AgentLifecycleRequest,
) -> crate::server::connection::Result<()> {
    let method = agent_lifecycle_method_spec(&request);
    let call = endpoint.register_unary(method).await;
    let call = match call {
        Ok(call) => call,
        Err(error) => {
            return endpoint.send_error(duplicate_call_error(error)).await;
        }
    };

    let agent_ctx = agent_service_ctx(endpoint.ctx).await;
    let response = match request {
        AgentLifecycleRequest::Create(request) => AgentLifecycleResponse::Create(
            AgentService::create(&agent_ctx, request)
                .await
                .map(|agent| AgentRecord::from(&agent)),
        ),
        AgentLifecycleRequest::Rename(request) => AgentLifecycleResponse::Rename(
            AgentService::rename(&agent_ctx, request)
                .await
                .map(|agent| AgentRecord::from(&agent)),
        ),
        AgentLifecycleRequest::Delete { agent_id } => {
            AgentLifecycleResponse::Delete(AgentService::delete(&agent_ctx, agent_id).await)
        }
    };

    finish_agent_lifecycle_unary(endpoint, call, response).await
}

async fn handle_subscribe_agent_events_request(
    endpoint: &EndpointCall<'_>,
    method: MethodSpec,
    host_id: Uuid,
) -> crate::server::connection::Result<()> {
    let stream = endpoint.register_server_stream(method, None).await;
    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            return endpoint.send_error(duplicate_call_error(error)).await;
        }
    };

    let agent_ctx = agent_service_ctx(endpoint.ctx).await;
    let rpc = endpoint.rpc();
    let stream_handle = stream.handle.clone();
    match AgentService::subscribe_agent_events(&agent_ctx, host_id, &stream, || {
        rpc.activate_inbound_for_handle(&stream_handle)
    })
    .await
    {
        Ok(()) => Ok(()),
        Err(error) => finish_endpoint_server_stream_with_error(endpoint, stream, error).await,
    }
}

async fn handle_send_input_request(
    endpoint: &EndpointCall<'_>,
    method: MethodSpec,
    request: crate::protocol::wire::SendInputRequest,
) -> crate::server::connection::Result<()> {
    let call = endpoint.register_unary(method).await;
    let call = match call {
        Ok(call) => call,
        Err(error) => {
            return endpoint.send_error(duplicate_call_error(error)).await;
        }
    };

    let agent_ctx = agent_service_ctx(endpoint.ctx).await;
    let response = AgentService::send_input(&agent_ctx, request)
        .await
        .map(|()| {
            ResponseFrame::Payload(crate::protocol::wire::SendInputResponse {}.encode_to_vec())
        })
        .unwrap_or_else(ResponseFrame::Error);
    endpoint.finish_unary_response(call, response).await
}

async fn finish_endpoint_server_stream_with_error(
    endpoint: &EndpointCall<'_>,
    stream: EndpointServerStream,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let closing = endpoint
        .rpc()
        .begin_inbound_closing_for_handle_if(&stream.handle, |_, _| true);
    let Some(closing) = closing else {
        return Ok(());
    };
    let result = closing.send_response(ResponseFrame::Error(error)).await;
    endpoint.rpc().finish_inbound_closing(&closing);
    result.map_err(endpoint_response_send_error)
}

fn agent_lifecycle_method_spec(request: &AgentLifecycleRequest) -> MethodSpec {
    match request {
        AgentLifecycleRequest::Create(_) => method::AGENT_CREATE,
        AgentLifecycleRequest::Rename(_) => method::AGENT_RENAME,
        AgentLifecycleRequest::Delete { .. } => method::AGENT_DELETE,
    }
}

async fn finish_agent_lifecycle_unary(
    endpoint: &EndpointCall<'_>,
    call: RpcInboundUnary,
    response: AgentLifecycleResponse,
) -> crate::server::connection::Result<()> {
    let method = response.method_name();
    let response = encode_agent_lifecycle_response_frame(&response).unwrap_or_else(|error| {
        ResponseFrame::Error(ProtocolError::ServerError {
            message: format!("failed to encode {method} response: {error}"),
        })
    });
    endpoint.finish_unary_response(call, response).await
}

async fn agent_service_ctx(ctx: &ConnectionContext) -> AgentServiceCtx {
    let (host_id, is_cloud_server) = {
        let state = ctx.state.read().await;
        (state.host_id(), state.is_cloud_server())
    };
    AgentServiceCtx::new(
        ctx.user_state.clone(),
        ctx.event_tx.clone(),
        ctx.user_id,
        host_id,
        is_cloud_server,
    )
}

async fn send_endpoint_error_response(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    call_id: CallId,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let _ = tx
        .send(frame_message(
            reply_src,
            reply_dst,
            call_id,
            FrameBody::Response(ResponseFrame::Error(error)),
        ))
        .await;
    Ok(())
}

async fn send_endpoint_error_response_for_counterparty(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    msg_type: &str,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let Some((reply_src, reply_dst)) = reply_routes(counterparty, msg_type) else {
        return Ok(());
    };
    send_endpoint_error_response(tx, reply_src, reply_dst, call_id, error).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio::time::timeout;
    use uuid::Uuid;

    use super::*;
    use crate::protocol::Link;
    use crate::server::{LOCAL_USER_ID, test_helpers};

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn route_stack(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    fn call_id(n: u128) -> CallId {
        CallId::from(Uuid::from_u128(n))
    }

    async fn test_ctx(link: &str, is_local: bool) -> ConnectionContext {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let link = Link::new(link).unwrap();
        let rpc = {
            let mut us = user_state.write().await;
            let (_handle, _rx) = us.try_reserve_link(link.clone()).unwrap();
            if !is_local {
                us.mark_peer_link(link.clone());
            }
            us.rpc_for_link(&link).unwrap()
        };
        ConnectionContext {
            state,
            rpc,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link,
            is_local,
            heartbeat: None,
            routing_role: crate::protocol::handshake::RoutingRole::Host,
        }
    }

    async fn expect_endpoint_error(
        rx: &mut mpsc::Receiver<Message>,
        call_id: &CallId,
    ) -> ProtocolError {
        let msg = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for endpoint error response")
            .expect("expected endpoint error response");
        let Message::Frame(Frame {
            call_id: response_call_id,
            body: FrameBody::Response(ResponseFrame::Error(error)),
            ..
        }) = msg
        else {
            panic!("expected endpoint error response, got {msg:?}");
        };
        assert_eq!(&response_call_id, call_id);
        error
    }

    #[tokio::test]
    async fn closing_inbound_call_is_not_endpoint_stream_target() {
        let ctx = test_ctx("owner", true).await;
        let key_call_id = call_id(42);
        let (tx, _rx) = mpsc::channel(1);

        let stream = ctx
            .rpc()
            .register_endpoint_server_stream(EndpointServerStreamStart {
                tx,
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: route("client"),
                call_id: key_call_id.clone(),
                method: method::AGENT_SUBSCRIBE_EVENTS,
                dedup_key: None,
            })
            .unwrap();
        ctx.rpc().activate_inbound_for_handle(&stream.handle);
        ctx.rpc()
            .begin_inbound_closing_for_handle_if(&stream.handle, |_, _| true)
            .unwrap();

        assert!(matches!(
            ctx.rpc().inbound_call_target_for_call(&key_call_id),
            Some(RpcInboundCallTarget::NotAccepting {
                method: method::AGENT_SUBSCRIBE_EVENTS,
                state: InboundCallState::Closing
            })
        ));
    }

    #[tokio::test]
    async fn known_wrong_scope_endpoint_method_returns_permission_denied() {
        let ctx = test_ctx("owner", true).await;
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        handle_endpoint_frame(
            &tx,
            counterparty,
            key_call_id.clone(),
            FrameBody::Request(RequestFrame {
                method: method::AGENT_LIST_NAME.to_string(),
                payload: Vec::new(),
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            expect_endpoint_error(&mut rx, &key_call_id).await,
            ProtocolError::PermissionDenied { message }
                if message.contains("not valid in routed scope")
        ));
    }

    #[tokio::test]
    async fn direct_peer_endpoint_request_uses_connection_rpc_without_route_context() {
        let ctx = test_ctx("peer", false).await;
        let counterparty = route_stack(&["peer", "client"]);
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        handle_endpoint_frame(
            &tx,
            counterparty,
            key_call_id.clone(),
            FrameBody::Request(RequestFrame {
                method: method::AGENT_LIST_NAME.to_string(),
                payload: Vec::new(),
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            expect_endpoint_error(&mut rx, &key_call_id).await,
            ProtocolError::PermissionDenied { message }
                if message.contains("not valid in routed scope")
        ));
    }

    #[tokio::test]
    async fn active_no_input_followup_cancel_terminates_call() {
        let ctx = test_ctx("owner", true).await;
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        let stream = ctx
            .rpc()
            .register_endpoint_server_stream(EndpointServerStreamStart {
                tx: tx.clone(),
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                call_id: key_call_id.clone(),
                method: method::AGENT_SUBSCRIBE_EVENTS,
                dedup_key: None,
            })
            .unwrap();
        ctx.rpc().activate_inbound_for_handle(&stream.handle);

        handle_endpoint_frame(
            &tx,
            counterparty,
            key_call_id.clone(),
            FrameBody::Cancel,
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            expect_endpoint_error(&mut rx, &key_call_id).await,
            ProtocolError::Cancelled { .. }
        ));
        assert!(ctx.rpc().inbound_for_call(&key_call_id).is_none());
    }

    #[tokio::test]
    async fn starting_subscribe_session_cancel_terminates_call() {
        let ctx = test_ctx("owner", true).await;
        let counterparty = route("client");
        let key_call_id = call_id(43);
        let (tx, mut rx) = mpsc::channel(2);

        ctx.rpc()
            .register_endpoint_server_stream(EndpointServerStreamStart {
                tx: tx.clone(),
                owner_link: Link::new("owner").unwrap(),
                reply_src: route("server"),
                reply_dst: counterparty.clone(),
                call_id: key_call_id.clone(),
                method: method::AGENT_SUBSCRIBE_SESSION,
                dedup_key: None,
            })
            .unwrap();

        handle_endpoint_frame(
            &tx,
            counterparty,
            key_call_id.clone(),
            FrameBody::Cancel,
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            expect_endpoint_error(&mut rx, &key_call_id).await,
            ProtocolError::Cancelled { .. }
        ));
        assert!(ctx.rpc().inbound_for_call(&key_call_id).is_none());
    }

    #[tokio::test]
    async fn unknown_endpoint_method_returns_unimplemented() {
        let ctx = test_ctx("owner", true).await;
        let counterparty = route("client");
        let key_call_id = call_id(7);
        let (tx, mut rx) = mpsc::channel(2);

        handle_endpoint_frame(
            &tx,
            counterparty,
            key_call_id.clone(),
            FrameBody::Request(RequestFrame {
                method: "/amux.v1.Missing/Nope".to_string(),
                payload: Vec::new(),
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            expect_endpoint_error(&mut rx, &key_call_id).await,
            ProtocolError::Unimplemented { message }
                if message.contains("unknown endpoint method")
        ));
    }
}

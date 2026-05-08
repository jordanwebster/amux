use tokio::sync::mpsc;

use crate::protocol::Route;
use crate::protocol::message::{
    CallId, FrameBody, Message, ProtocolError, RequestFrame, ResponseFrame, RoutedFrame,
    RoutedFrameMessage,
};
use crate::protocol::method::{self, MethodLookupError, MethodScope, MethodSpec};
use crate::protocol::wire::{
    AgentLifecycleRequest, AgentLifecycleResponse, AgentRecord,
    decode_agent_lifecycle_request_payload, encode_agent_lifecycle_response_frame,
};
use crate::rpc::{
    DedupKey, InboundCallState, OutboundCallState, RegisterCallError, RpcInboundBidi,
    RpcInboundClosing, RpcInboundFrameTarget, RpcInboundUnary, RpcLocalOriginOutboundStart,
    RpcRoutedBidiStart, RpcRoutedUnaryStart, RpcStreamWriter,
};
use crate::server::connection::ConnectionContext;
use crate::server::{
    RpcDispatcher, RpcInboundCloseTarget, open_session_closing_from_rpc_closing,
    send_terminal_and_finish_open_session,
};
use crate::services::{AgentService, AgentServiceCtx, OpenSessionCall};

fn reply_routes(src: Route, msg_type: &str) -> Option<(Route, Route)> {
    match Route::reply(src) {
        Some(routes) => Some(routes),
        None => {
            tracing::warn!(msg_type, "dropping routable message with empty src route");
            None
        }
    }
}

fn routed_payload_message(src: Route, dst: Route, call_id: CallId, payload: Vec<u8>) -> Message {
    Message::Routed(RoutedFrame {
        src,
        dst,
        call_id,
        message: RoutedFrameMessage::Payload(payload),
    })
}

pub(super) enum LocalOriginRoutedRegistration {
    Continue,
    Reject {
        failed_route: Route,
        error: ProtocolError,
    },
}

pub(super) async fn register_local_origin_routed_request_if_any(
    src: &Route,
    dst: &Route,
    call_id: &CallId,
    payload: &[u8],
    ctx: &ConnectionContext,
) -> LocalOriginRoutedRegistration {
    if !ctx.is_local || dst.is_empty() || src.peek() != Some(&ctx.link) {
        return LocalOriginRoutedRegistration::Continue;
    }
    if ctx.state.read().await.is_cloud_server() {
        return LocalOriginRoutedRegistration::Continue;
    }
    let Ok(FrameBody::Request(request)) = crate::protocol::wire::decode_frame_body(payload) else {
        return LocalOriginRoutedRegistration::Continue;
    };
    let Some(spec) = method::find(&request.method).filter(|spec| spec.scope == MethodScope::Routed)
    else {
        return LocalOriginRoutedRegistration::Continue;
    };
    let Some(counterparty_route) = route_from_src_and_dst(src, dst) else {
        return LocalOriginRoutedRegistration::Continue;
    };

    let Some(rpc) = ctx.user_state.read().await.rpc_for_outbound_route(dst) else {
        return LocalOriginRoutedRegistration::Reject {
            failed_route: counterparty_route,
            error: ProtocolError::Unreachable {
                message: format!("route not found: {dst}"),
            },
        };
    };

    match rpc.register_local_origin_outbound(RpcLocalOriginOutboundStart {
        call_id: call_id.clone(),
        method: spec,
        state: OutboundCallState::AwaitingResponse,
        owner_link: ctx.link.clone(),
        request_src: src.clone(),
        request_dst: dst.clone(),
    }) {
        Ok(_) => LocalOriginRoutedRegistration::Continue,
        Err(error) => {
            tracing::warn!(
                error = ?error,
                call_id = ?call_id.as_bytes(),
                method = request.method,
                "rejecting duplicate local-origin routed request"
            );
            LocalOriginRoutedRegistration::Reject {
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
                "duplicate active local-origin routed call {:?}",
                call_id.as_bytes()
            ),
        },
        RegisterCallError::DuplicateDedupKey { call_id, .. } => ProtocolError::AlreadyExists {
            message: format!(
                "duplicate active local-origin routed call {:?}",
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

pub(super) async fn handle_malformed_routed_frame_body(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    error: crate::protocol::wire::DecodeError,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    tracing::warn!(error = %error, "failed to decode routed protobuf FrameBody");
    let Some(rpc) = routed_endpoint_rpc(ctx, &counterparty, &call_id).await else {
        send_routed_error_response_for_counterparty(
            tx,
            counterparty,
            call_id,
            "RoutedFrameBodyDecodeError",
            ProtocolError::Unreachable {
                message: "unknown routed frame source".to_string(),
            },
        )
        .await?;
        return Ok(());
    };
    let Some(endpoint) = RoutedEndpointCall::new(
        tx,
        ctx,
        rpc,
        counterparty,
        call_id,
        "RoutedFrameBodyDecodeError",
    ) else {
        return Ok(());
    };
    endpoint
        .terminate_existing_or_send(ProtocolError::InvalidArgument {
            message: error.to_string(),
        })
        .await
}

async fn routed_endpoint_rpc(
    ctx: &ConnectionContext,
    counterparty: &Route,
    call_id: &CallId,
) -> Option<RpcDispatcher> {
    let us = ctx.user_state.read().await;
    us.rpc_for_inbound_call(call_id)
        .or_else(|| us.route_rpc_for_counterparty(counterparty))
        .or_else(|| (ctx.is_local || counterparty.peek() == Some(&ctx.link)).then(|| ctx.rpc()))
}

enum RoutedCallCloseTarget {
    Absent,
    AlreadyClosing,
    OpenSession(crate::server::open_session_lifecycle::OpenSessionClosing),
    Generic(RpcInboundClosing),
}

struct RoutedEndpointCall<'a> {
    tx: &'a mpsc::Sender<Message>,
    ctx: &'a ConnectionContext,
    rpc: RpcDispatcher,
    counterparty: Route,
    call_id: CallId,
    reply_src: Route,
    reply_dst: Route,
}

impl<'a> RoutedEndpointCall<'a> {
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
        send_routed_error_response(
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
        terminate_routed_call_if_currently_present_or_send(self, error).await
    }

    async fn register_unary(
        &self,
        method: MethodSpec,
    ) -> Result<RpcInboundUnary, RegisterCallError> {
        self.rpc.register_routed_unary(RpcRoutedUnaryStart {
            tx: self.tx.clone(),
            owner_link: self.ctx.link.clone(),
            reply_src: self.reply_src.clone(),
            reply_dst: self.reply_dst.clone(),
            call_id: self.call_id.clone(),
            method,
        })
    }

    async fn register_bidi(
        &self,
        method: MethodSpec,
        dedup_key: Option<DedupKey>,
        stream_capacity: usize,
    ) -> Result<RpcInboundBidi, RegisterCallError> {
        self.rpc.register_routed_bidi(RpcRoutedBidiStart {
            tx: self.tx.clone(),
            owner_link: self.ctx.link.clone(),
            reply_src: self.reply_src.clone(),
            reply_dst: self.reply_dst.clone(),
            call_id: self.call_id.clone(),
            method,
            dedup_key,
            stream_capacity,
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
        result.map_err(routed_response_send_error)
    }

    fn rpc(&self) -> RpcDispatcher {
        self.rpc.clone()
    }
}

async fn terminate_routed_call_if_currently_present_or_send(
    endpoint: &RoutedEndpointCall<'_>,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let close_target = match endpoint
        .rpc()
        .begin_inbound_closing_for_call(&endpoint.call_id)
    {
        RpcInboundCloseTarget::Closing(closing)
            if closing.handle.method == method::AGENT_OPEN_SESSION =>
        {
            match open_session_closing_from_rpc_closing(endpoint.rpc(), closing) {
                Some(closing) => RoutedCallCloseTarget::OpenSession(closing),
                None => RoutedCallCloseTarget::AlreadyClosing,
            }
        }
        RpcInboundCloseTarget::Closing(closing) => RoutedCallCloseTarget::Generic(closing),
        RpcInboundCloseTarget::AlreadyClosing => RoutedCallCloseTarget::AlreadyClosing,
        RpcInboundCloseTarget::Absent => RoutedCallCloseTarget::Absent,
    };

    match close_target {
        RoutedCallCloseTarget::Absent => endpoint.send_error(error).await,
        RoutedCallCloseTarget::AlreadyClosing => Ok(()),
        RoutedCallCloseTarget::OpenSession(closing) => {
            send_terminal_and_finish_open_session(&endpoint.ctx.user_state, closing, Err(error))
                .await
                .map_err(routed_response_send_error)
        }
        RoutedCallCloseTarget::Generic(closing) => {
            let result = closing.send_empty_response_result(Err(error)).await;
            endpoint.rpc.finish_inbound_closing(&closing);
            result.map_err(routed_response_send_error)
        }
    }
}

fn routed_response_send_error(
    error: crate::rpc::RpcRoutedSendError,
) -> crate::server::ConnectionError {
    crate::server::connection::ConnectionError::Config(format!(
        "failed to send routed error response: {error}"
    ))
}

pub(super) async fn handle_routed_endpoint_frame(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    body: FrameBody,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let rpc = routed_endpoint_rpc(ctx, &counterparty, &call_id).await;
    if let Some(rpc) = rpc.clone()
        && let Some(target) = rpc.inbound_frame_target_for_call(&call_id)
    {
        return match target {
            RpcInboundFrameTarget::ActiveStream {
                method,
                stream_writer,
            } => {
                handle_active_routed_stream_frame(
                    tx,
                    counterparty,
                    call_id,
                    method,
                    stream_writer,
                    body,
                    ctx,
                    rpc,
                )
                .await
            }
            RpcInboundFrameTarget::ActiveNoInput { method } => {
                handle_active_routed_no_input_followup(
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
            RpcInboundFrameTarget::NotAccepting { state } => {
                drop_routed_frame_for_inactive_call(&counterparty, &call_id, state, &body);
                Ok(())
            }
        };
    }

    match body {
        FrameBody::Request(request) => {
            let Some(rpc) = rpc else {
                send_routed_error_response_for_counterparty(
                    tx,
                    counterparty,
                    call_id,
                    "RoutedRequest",
                    ProtocolError::Unreachable {
                        message: "unknown routed frame source".to_string(),
                    },
                )
                .await?;
                return Ok(());
            };
            handle_routed_request(tx, counterparty, call_id, request, ctx, rpc).await
        }
        FrameBody::StreamItem(payload) => {
            drop_stale_routed_stream_item(&counterparty, &call_id, payload);
            Ok(())
        }
        FrameBody::Cancel => {
            drop_stale_routed_cancel(&counterparty, &call_id);
            Ok(())
        }
        FrameBody::Response(_) => {
            tracing::debug!(
                counterparty = %counterparty,
                call_id = ?call_id.as_bytes(),
                "dropping stale routed response for inactive call"
            );
            Ok(())
        }
    }
}

async fn handle_routed_request(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    request: RequestFrame,
    ctx: &ConnectionContext,
    rpc: RpcDispatcher,
) -> crate::server::connection::Result<()> {
    let Some(endpoint) =
        RoutedEndpointCall::new(tx, ctx, rpc, counterparty, call_id, "RoutedRequest")
    else {
        return Ok(());
    };

    let spec = match method::find_for_scope(&request.method, MethodScope::Routed) {
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
                        spec.scope.as_str(),
                        requested_scope.as_str()
                    ),
                })
                .await;
        }
        Err(MethodLookupError::Unknown) => {
            return endpoint
                .send_error(ProtocolError::Unimplemented {
                    message: format!("unknown routed method {}", request.method),
                })
                .await;
        }
    };

    match spec.kind {
        method::MethodKind::Unary => handle_routed_unary_request(&endpoint, spec, request).await,
        method::MethodKind::ServerStreaming => {
            send_unsupported_routed_method(&endpoint, spec.name).await
        }
        method::MethodKind::BidiStreaming => {
            handle_routed_bidi_request(&endpoint, spec, request, ctx).await
        }
    }
}

async fn handle_routed_unary_request(
    endpoint: &RoutedEndpointCall<'_>,
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
        method => send_unsupported_routed_method(endpoint, method).await,
    }
}

async fn handle_routed_bidi_request(
    endpoint: &RoutedEndpointCall<'_>,
    spec: MethodSpec,
    request: RequestFrame,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    match spec.name {
        method::AGENT_OPEN_SESSION_NAME => {
            if let Err(error) = crate::protocol::wire::decode_open_session_request(&request) {
                return endpoint
                    .send_error(ProtocolError::InvalidArgument {
                        message: error.to_string(),
                    })
                    .await;
            }
            let call = endpoint.register_bidi(spec, None, 256).await;
            let call = match call {
                Ok(call) => call,
                Err(error) => {
                    return endpoint.send_error(duplicate_call_error(error)).await;
                }
            };
            let call =
                OpenSessionCall::from_rpc(call, endpoint.counterparty.clone(), endpoint.rpc())
                    .expect("OpenSession dispatch registered non-OpenSession RPC call");
            let agent_ctx = agent_service_ctx(ctx).await;
            tokio::spawn(async move {
                if let Err(error) = AgentService::open_session(call, &agent_ctx).await {
                    tracing::warn!(error = %error, "OpenSession service task failed");
                }
            });
            Ok(())
        }
        method => send_unsupported_routed_method(endpoint, method).await,
    }
}

async fn send_unsupported_routed_method(
    endpoint: &RoutedEndpointCall<'_>,
    method: &str,
) -> crate::server::connection::Result<()> {
    tracing::warn!(method, "unsupported routed protobuf request");
    endpoint
        .send_error(ProtocolError::Unimplemented {
            message: format!("routed method {method} is not implemented"),
        })
        .await
}

async fn handle_active_routed_no_input_followup(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    method: MethodSpec,
    body: FrameBody,
    ctx: &ConnectionContext,
    rpc: RpcDispatcher,
) -> crate::server::connection::Result<()> {
    let Some(endpoint) =
        RoutedEndpointCall::new(tx, ctx, rpc, counterparty, call_id, "ActiveRoutedUnary")
    else {
        return Ok(());
    };
    let error = match body {
        FrameBody::Cancel => ProtocolError::Cancelled {
            message: format!("routed call {} was cancelled", method.name),
        },
        body => ProtocolError::InvalidArgument {
            message: format!(
                "routed {} frame is not valid for active non-client-streaming call {}",
                frame_body_kind(&body),
                method.name
            ),
        },
    };
    endpoint.terminate_existing_or_send(error).await
}

async fn handle_active_routed_stream_frame(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    method: MethodSpec,
    stream_writer: RpcStreamWriter,
    body: FrameBody,
    ctx: &ConnectionContext,
    rpc: RpcDispatcher,
) -> crate::server::connection::Result<()> {
    match body {
        FrameBody::StreamItem(payload) => {
            if stream_writer
                .send_frame_body(FrameBody::StreamItem(payload))
                .await
                .is_err()
            {
                tracing::debug!(
                    counterparty = %counterparty,
                    call_id = ?call_id.as_bytes(),
                    method = method.name,
                    body = "stream_item",
                    "dropping routed call frame because stream receiver is closed"
                );
            }
            Ok(())
        }
        FrameBody::Cancel => {
            let Some(endpoint) = RoutedEndpointCall::new(
                tx,
                ctx,
                rpc,
                counterparty,
                call_id,
                "ActiveRoutedStreamCancel",
            ) else {
                return Ok(());
            };
            endpoint
                .terminate_existing_or_send(ProtocolError::Cancelled {
                    message: format!("routed call {} was cancelled", method.name),
                })
                .await
        }
        body => {
            let body_kind = frame_body_kind(&body);
            let Some(endpoint) = RoutedEndpointCall::new(
                tx,
                ctx,
                rpc,
                counterparty,
                call_id,
                "ActiveRoutedStreamFrame",
            ) else {
                return Ok(());
            };
            endpoint
                .terminate_existing_or_send(ProtocolError::InvalidArgument {
                    message: format!(
                        "routed {body_kind} frame is not valid for active client-streaming call {}",
                        method.name
                    ),
                })
                .await
        }
    }
}

fn duplicate_call_error(error: RegisterCallError) -> ProtocolError {
    match error {
        RegisterCallError::DuplicateCallId { call_id } => ProtocolError::AlreadyExists {
            message: format!("duplicate active call {:?}", call_id.as_bytes()),
        },
        RegisterCallError::DuplicateDedupKey {
            key:
                DedupKey::OpenSession {
                    counterparty_route,
                    agent_id,
                },
            call_id,
            ..
        } => ProtocolError::AlreadyExists {
            message: format!(
                "duplicate OpenSession for agent {agent_id} from {counterparty_route}; existing call id {:?}",
                call_id.as_bytes()
            ),
        },
        RegisterCallError::DuplicateDedupKey {
            key: DedupKey::PeerRoutingSubscription { link },
            call_id,
            ..
        } => ProtocolError::AlreadyExists {
            message: format!(
                "duplicate peer routing subscription from {link}; existing call id {:?}",
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
    }
}

fn drop_stale_routed_stream_item(counterparty: &Route, call_id: &CallId, _payload: Vec<u8>) {
    tracing::debug!(
        counterparty = %counterparty,
        call_id = ?call_id.as_bytes(),
        "dropping stale routed stream item for inactive call"
    );
}

fn drop_stale_routed_cancel(counterparty: &Route, call_id: &CallId) {
    tracing::debug!(
        counterparty = %counterparty,
        call_id = ?call_id.as_bytes(),
        "dropping stale routed cancel for inactive call"
    );
}

fn drop_routed_frame_for_inactive_call(
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
        "dropping routed frame for inactive inbound call"
    );
}

async fn handle_agent_lifecycle_request(
    endpoint: &RoutedEndpointCall<'_>,
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

fn agent_lifecycle_method_spec(request: &AgentLifecycleRequest) -> MethodSpec {
    match request {
        AgentLifecycleRequest::Create(_) => method::AGENT_CREATE,
        AgentLifecycleRequest::Rename(_) => method::AGENT_RENAME,
        AgentLifecycleRequest::Delete { .. } => method::AGENT_DELETE,
    }
}

async fn finish_agent_lifecycle_unary(
    endpoint: &RoutedEndpointCall<'_>,
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

async fn send_routed_error_response(
    tx: &mpsc::Sender<Message>,
    reply_src: Route,
    reply_dst: Route,
    call_id: CallId,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let payload =
        crate::protocol::wire::encode_frame_body(&FrameBody::Response(ResponseFrame::Error(error)))
            .map_err(|error| {
                crate::server::connection::ConnectionError::Config(format!(
                    "failed to encode routed error response: {error}"
                ))
            })?;
    let _ = tx
        .send(routed_payload_message(
            reply_src, reply_dst, call_id, payload,
        ))
        .await;
    Ok(())
}

async fn send_routed_error_response_for_counterparty(
    tx: &mpsc::Sender<Message>,
    counterparty: Route,
    call_id: CallId,
    msg_type: &str,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let Some((reply_src, reply_dst)) = reply_routes(counterparty, msg_type) else {
        return Ok(());
    };
    send_routed_error_response(tx, reply_src, reply_dst, call_id, error).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;
    use uuid::Uuid;

    use super::*;
    use crate::protocol::{Link, open_session};
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

    fn test_host(id: Uuid, name: &str) -> crate::protocol::Host {
        crate::protocol::Host {
            id,
            name: name.to_string(),
            version: "v1".to_string(),
            capabilities: Default::default(),
        }
    }

    async fn expect_routed_error(
        rx: &mut mpsc::Receiver<Message>,
        call_id: &CallId,
    ) -> ProtocolError {
        let msg = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for routed error response")
            .expect("expected routed error response");
        let Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed error response");
        };
        assert_eq!(&response_call_id, call_id);
        let FrameBody::Response(ResponseFrame::Error(error)) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected routed error frame body");
        };
        error
    }

    #[tokio::test]
    async fn closing_inbound_call_is_not_routed_stream_target() {
        let (_state, user_state) = test_helpers::test_state().await;
        let key_call_id = call_id(42);
        let (tx, _rx) = mpsc::channel(1);

        {
            let us = user_state.read().await;
            let call = us
                .test_rpc()
                .register_routed_bidi(RpcRoutedBidiStart {
                    tx,
                    owner_link: Link::new("owner").unwrap(),
                    reply_src: route("server"),
                    reply_dst: route("client"),
                    call_id: key_call_id.clone(),
                    method: method::AGENT_OPEN_SESSION,
                    dedup_key: None,
                    stream_capacity: 1,
                })
                .unwrap();
            us.test_rpc()
                .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
                .unwrap();
        }

        assert!(matches!(
            user_state
                .read()
                .await
                .test_rpc()
                .inbound_frame_target_for_call(&key_call_id),
            Some(RpcInboundFrameTarget::NotAccepting {
                state: InboundCallState::Closing
            })
        ));
    }

    #[tokio::test]
    async fn known_wrong_scope_routed_method_returns_permission_denied() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        handle_routed_endpoint_frame(
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
            expect_routed_error(&mut rx, &key_call_id).await,
            ProtocolError::PermissionDenied { message }
                if message.contains("not valid in routed scope")
        ));
    }

    #[tokio::test]
    async fn direct_peer_endpoint_request_uses_connection_rpc_without_route_context() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let link = Link::new("peer").unwrap();
        let rpc = {
            let mut us = user_state.write().await;
            let (_handle, _rx) = us.try_reserve_link(link.clone()).unwrap();
            us.mark_peer_link(link.clone());
            us.rpc_for_link(&link).unwrap()
        };
        let ctx = ConnectionContext {
            state,
            rpc,
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link,
            is_local: false,
            heartbeat: None,
        };
        let counterparty = route_stack(&["peer", "client"]);
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        handle_routed_endpoint_frame(
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
            expect_routed_error(&mut rx, &key_call_id).await,
            ProtocolError::PermissionDenied { message }
                if message.contains("not valid in routed scope")
        ));
    }

    #[tokio::test]
    async fn active_call_stays_on_original_rpc_when_more_specific_route_appears() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let link = Link::new("peer").unwrap();
        let rpc = {
            let mut us = user_state.write().await;
            let (_handle, _rx) = us.try_reserve_link(link.clone()).unwrap();
            us.mark_peer_link(link.clone());
            us.rpc_for_link(&link).unwrap()
        };
        let ctx = ConnectionContext {
            state,
            rpc: rpc.clone(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: link.clone(),
            is_local: false,
            heartbeat: None,
        };
        let counterparty = route_stack(&["peer", "client"]);
        let (reply_src, reply_dst) = Route::reply(counterparty.clone()).unwrap();
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        rpc.register_routed_unary(RpcRoutedUnaryStart {
            tx: tx.clone(),
            owner_link: link.clone(),
            reply_src,
            reply_dst,
            call_id: key_call_id.clone(),
            method: method::AGENT_CREATE,
        })
        .unwrap();
        {
            let mut us = user_state.write().await;
            us.apply_peer_host_up(
                &link,
                test_host(Uuid::from_u128(100), "client-host"),
                route("client"),
            );
        }

        handle_routed_endpoint_frame(
            &tx,
            counterparty,
            key_call_id.clone(),
            FrameBody::Cancel,
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            expect_routed_error(&mut rx, &key_call_id).await,
            ProtocolError::Cancelled { .. }
        ));
        assert!(rpc.inbound_for_call(&key_call_id).is_none());
    }

    #[tokio::test]
    async fn unknown_routed_method_returns_unimplemented() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        handle_routed_endpoint_frame(
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
            expect_routed_error(&mut rx, &key_call_id).await,
            ProtocolError::Unimplemented { message }
                if message.contains("unknown routed method")
        ));
    }

    #[tokio::test]
    async fn unknown_routed_response_stream_item_and_cancel_are_dropped() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let (tx, mut rx) = mpsc::channel(2);

        for body in [
            FrameBody::Response(ResponseFrame::Payload(Vec::new())),
            FrameBody::StreamItem(b"stale".to_vec()),
            FrameBody::Cancel,
        ] {
            handle_routed_endpoint_frame(&tx, counterparty.clone(), call_id(42), body, &ctx)
                .await
                .unwrap();
        }

        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn duplicate_request_for_closing_inbound_call_is_dropped() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        {
            let us = user_state.read().await;
            let call = us
                .test_rpc()
                .register_routed_bidi(RpcRoutedBidiStart {
                    tx: tx.clone(),
                    owner_link: Link::new("owner").unwrap(),
                    reply_src: Route::empty(),
                    reply_dst: counterparty.clone(),
                    call_id: key_call_id.clone(),
                    method: method::AGENT_OPEN_SESSION,
                    dedup_key: None,
                    stream_capacity: 1,
                })
                .unwrap();
            us.test_rpc()
                .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
                .unwrap();
        }

        let payload = open_session::encode_open_session_request().unwrap();
        let FrameBody::Request(request) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected encoded OpenSession request frame");
        };

        handle_routed_endpoint_frame(
            &tx,
            counterparty,
            key_call_id,
            FrameBody::Request(request),
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn malformed_routed_body_closes_generic_active_inbound_call() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        let rpc = {
            let mut us = user_state.write().await;
            us.ensure_route_rpc(counterparty.clone())
        };
        rpc.register_routed_unary(RpcRoutedUnaryStart {
            tx: tx.clone(),
            owner_link: Link::new("owner").unwrap(),
            reply_src: Route::empty(),
            reply_dst: counterparty.clone(),
            call_id: key_call_id.clone(),
            method: method::AGENT_CREATE,
        })
        .unwrap();

        let Err(error) = crate::protocol::wire::decode_frame_body(&[0xff]) else {
            panic!("expected malformed FrameBody decode error");
        };

        handle_malformed_routed_frame_body(
            &tx,
            counterparty.clone(),
            key_call_id.clone(),
            error,
            &ctx,
        )
        .await
        .unwrap();

        let msg = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for generic routed error response")
            .expect("expected generic routed error response");
        let Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed generic error response");
        };
        assert_eq!(response_call_id, key_call_id);
        let FrameBody::Response(ResponseFrame::Error(ProtocolError::InvalidArgument { message })) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected invalid argument response");
        };
        assert!(message.contains("invalid varint"));
        assert!(
            user_state
                .read()
                .await
                .route_rpc(&counterparty)
                .unwrap()
                .inbound_for_call(&key_call_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn stream_frame_for_active_unary_call_terminates_call() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        let rpc = {
            let mut us = user_state.write().await;
            us.ensure_route_rpc(counterparty.clone())
        };
        rpc.register_routed_unary(RpcRoutedUnaryStart {
            tx: tx.clone(),
            owner_link: Link::new("owner").unwrap(),
            reply_src: Route::empty(),
            reply_dst: counterparty.clone(),
            call_id: key_call_id.clone(),
            method: method::AGENT_CREATE,
        })
        .unwrap();

        handle_routed_endpoint_frame(
            &tx,
            counterparty.clone(),
            key_call_id.clone(),
            FrameBody::StreamItem(b"unexpected".to_vec()),
            &ctx,
        )
        .await
        .unwrap();

        let msg = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for unary invalid-frame response")
            .expect("expected unary invalid-frame response");
        let Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed unary invalid-frame response");
        };
        assert_eq!(response_call_id, key_call_id);
        let FrameBody::Response(ResponseFrame::Error(ProtocolError::InvalidArgument { message })) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected invalid argument response");
        };
        assert!(
            message.contains("stream_item frame is not valid for active non-client-streaming call")
        );
        assert!(
            user_state
                .read()
                .await
                .route_rpc(&counterparty)
                .unwrap()
                .inbound_for_call(&key_call_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancel_for_active_unary_call_finishes_call_as_cancelled() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        let rpc = {
            let mut us = user_state.write().await;
            us.ensure_route_rpc(counterparty.clone())
        };
        rpc.register_routed_unary(RpcRoutedUnaryStart {
            tx: tx.clone(),
            owner_link: Link::new("owner").unwrap(),
            reply_src: Route::empty(),
            reply_dst: counterparty.clone(),
            call_id: key_call_id.clone(),
            method: method::AGENT_CREATE,
        })
        .unwrap();

        handle_routed_endpoint_frame(
            &tx,
            counterparty.clone(),
            key_call_id.clone(),
            FrameBody::Cancel,
            &ctx,
        )
        .await
        .unwrap();

        let msg = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for unary cancel response")
            .expect("expected unary cancel response");
        let Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed unary cancel response");
        };
        assert_eq!(response_call_id, key_call_id);
        let FrameBody::Response(ResponseFrame::Error(ProtocolError::Cancelled { message })) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected cancelled response");
        };
        assert!(message.contains("was cancelled"));
        assert!(
            user_state
                .read()
                .await
                .route_rpc(&counterparty)
                .unwrap()
                .inbound_for_call(&key_call_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancel_for_active_stream_call_finishes_call_without_queueing() {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);

        let rpc = {
            let mut us = user_state.write().await;
            us.ensure_route_rpc(counterparty.clone())
        };
        let call = rpc
            .register_routed_bidi(RpcRoutedBidiStart {
                tx: tx.clone(),
                owner_link: Link::new("owner").unwrap(),
                reply_src: Route::empty(),
                reply_dst: counterparty.clone(),
                call_id: key_call_id.clone(),
                method: method::AGENT_OPEN_SESSION,
                dedup_key: None,
                stream_capacity: 1,
            })
            .unwrap();

        handle_routed_endpoint_frame(
            &tx,
            counterparty.clone(),
            key_call_id.clone(),
            FrameBody::Cancel,
            &ctx,
        )
        .await
        .unwrap();

        assert!(call.cancellation.is_cancelled());
        let msg = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for stream cancel response")
            .expect("expected stream cancel response");
        let Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed stream cancel response");
        };
        assert_eq!(response_call_id, key_call_id);
        let FrameBody::Response(ResponseFrame::Error(ProtocolError::Cancelled { message })) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected cancelled response");
        };
        assert!(message.contains("was cancelled"));
        assert!(
            user_state
                .read()
                .await
                .route_rpc(&counterparty)
                .unwrap()
                .inbound_for_call(&key_call_id)
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_lifecycle_encode_failure_finishes_unary_call_with_server_error() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let ctx = ConnectionContext {
            state,
            rpc: user_state.read().await.test_rpc(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("owner").unwrap(),
            is_local: true,
            heartbeat: None,
        };
        let counterparty = route("client");
        let key_call_id = call_id(42);
        let (tx, mut rx) = mpsc::channel(2);
        let rpc = {
            let mut us = user_state.write().await;
            us.ensure_route_rpc(counterparty.clone())
        };
        let call = rpc
            .register_routed_unary(RpcRoutedUnaryStart {
                tx: tx.clone(),
                owner_link: Link::new("owner").unwrap(),
                reply_src: Route::empty(),
                reply_dst: counterparty.clone(),
                call_id: key_call_id.clone(),
                method: method::AGENT_CREATE,
            })
            .unwrap();
        let response = AgentLifecycleResponse::Create(Ok(AgentRecord {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: Some("bad-path".to_string()),
            command: "test".to_string(),
            working_dir: std::path::PathBuf::from(OsString::from_vec(vec![0xff])),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at_unix_ms: 0,
        }));

        let endpoint = RoutedEndpointCall::new(
            &tx,
            &ctx,
            rpc,
            counterparty.clone(),
            key_call_id.clone(),
            "test",
        )
        .unwrap();
        finish_agent_lifecycle_unary(&endpoint, call, response)
            .await
            .unwrap();

        let msg = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for encode-failure response")
            .expect("expected encode-failure response");
        let Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        }) = msg
        else {
            panic!("expected routed encode-failure response");
        };
        assert_eq!(response_call_id, key_call_id);
        let FrameBody::Response(ResponseFrame::Error(ProtocolError::ServerError { message })) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected server error response");
        };
        assert!(message.contains("failed to encode"));
        assert!(
            user_state
                .read()
                .await
                .test_rpc()
                .inbound_for_call(&key_call_id)
                .is_none()
        );
    }
}

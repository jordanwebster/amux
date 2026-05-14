//! Application frame dispatch adapters.
//!
//! This module handles hop-local control messages and delegates application
//! frames toward routing, endpoint dispatch, and protobuf services.

mod frame;
mod local;
mod peer;
mod rpc_runtime;

use frame::{handle_application_frame, handle_routing_error, validate_frame_provenance};
use local::handle_local_request;
use peer::{handle_peer_event, handle_ping, handle_reauth};
use prost::Message as _;
use tokio::sync::mpsc;

use super::connection::ConnectionContext;
use crate::protocol::message::{
    CallId, Frame, FrameBody, Message, ProtocolError, RequestFrame, ResponseFrame, RoutingEvent,
};
use crate::protocol::method::{MethodAccess, MethodLookupError};
use crate::protocol::{Route, method, wire};
use crate::rpc::{OutboundCallState, RegisterCallError};
use crate::server::connection::{ConnectionError, Result};
use crate::server::{
    EndpointServerStream, EndpointServerStreamStart, OutboundCallResources, peer_routing_dedup_key,
};
use crate::services::{RoutingService, RoutingServiceCtx, SubscribeRoutingEventsStartError};

async fn send_peer_response(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: CallId,
    error: Option<ProtocolError>,
) -> Result<()> {
    let Some((reply_src, reply_dst)) = Route::reply(src) else {
        tracing::warn!("dropping peer response with empty return route");
        return Ok(());
    };
    let response = match error {
        Some(error) => ResponseFrame::Error(error),
        None => ResponseFrame::Payload(wire::Empty {}.encode_to_vec()),
    };
    tx.send(Message::Frame(Frame {
        src: reply_src,
        dst: reply_dst,
        call_id,
        body: FrameBody::Response(response),
    }))
    .await
    .map_err(|_| {
        ConnectionError::Transport(crate::transport::TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "outgoing channel closed while sending peer response",
        )))
    })
}

async fn send_error_response(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: CallId,
    error: ProtocolError,
    send_context: &'static str,
) -> Result<()> {
    let Some((reply_src, reply_dst)) = Route::reply(src) else {
        tracing::warn!(
            context = send_context,
            "dropping error response with empty return route"
        );
        return Ok(());
    };
    tx.send(Message::Frame(Frame {
        src: reply_src,
        dst: reply_dst,
        call_id,
        body: FrameBody::Response(ResponseFrame::Error(error)),
    }))
    .await
    .map_err(|_| {
        ConnectionError::Transport(crate::transport::TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            format!("outgoing channel closed while sending {send_context}"),
        )))
    })
}

fn peer_protocol_error(message: impl Into<String>) -> ConnectionError {
    ConnectionError::Protocol(message.into())
}

fn validate_adjacent_peer_source(src: &Route, ctx: &ConnectionContext, frame_type: &str) -> bool {
    if ctx.is_local {
        tracing::warn!(
            frame_type = frame_type,
            "rejecting peer frame from local connection"
        );
        return false;
    }
    if src.len() != 1 {
        tracing::warn!(
            frame_type = frame_type,
            peer = %ctx.link,
            src = %src,
            "rejecting peer frame that did not arrive from an adjacent peer"
        );
        return false;
    }
    true
}

async fn handle_peer_request(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: CallId,
    request: RequestFrame,
    ctx: &ConnectionContext,
) -> Result<()> {
    if !validate_frame_provenance(&src, ctx) {
        return Ok(());
    }

    if !validate_adjacent_peer_source(&src, ctx, "peer request") {
        return Ok(());
    }

    match method::find_for_scope(&request.method, MethodAccess::Peer) {
        Ok(_) => {}
        Err(MethodLookupError::WrongScope {
            spec,
            requested_scope,
        }) => {
            return send_peer_response(
                tx,
                src,
                call_id,
                Some(ProtocolError::PermissionDenied {
                    message: format!(
                        "method {} is {} scoped and not valid in {} scope",
                        request.method,
                        spec.access.as_str(),
                        requested_scope.as_str()
                    ),
                }),
            )
            .await;
        }
        Err(MethodLookupError::Unknown) => {
            return send_peer_response(
                tx,
                src,
                call_id,
                Some(ProtocolError::Unimplemented {
                    message: format!("unknown peer method {}", request.method),
                }),
            )
            .await;
        }
    }

    match request.method.as_str() {
        method::ROUTING_SUBSCRIBE_EVENTS_NAME => {
            if let Err(error) =
                wire::SubscribeRoutingEventsRequest::decode(request.payload.as_slice())
            {
                send_peer_response(
                    tx,
                    src.clone(),
                    call_id,
                    Some(ProtocolError::InvalidArgument {
                        message: format!("invalid SubscribeRoutingEvents request: {error}"),
                    }),
                )
                .await?;
                return Ok(());
            }

            let stream = register_peer_routing_stream(tx, src.clone(), call_id.clone(), ctx).await;
            let stream = match stream {
                Ok(stream) => stream,
                Err(PeerRoutingStreamStartError::Response(error)) => {
                    send_peer_response(tx, src, call_id, Some(error)).await?;
                    return Ok(());
                }
                Err(PeerRoutingStreamStartError::ResponseThenClose { error, reason }) => {
                    send_peer_response(tx, src, call_id, Some(error)).await?;
                    return Err(peer_protocol_error(reason));
                }
            };

            let service_ctx = routing_service_ctx(ctx);
            match RoutingService::subscribe_routing_events(&service_ctx, &stream).await {
                Ok(()) => {
                    activate_inbound_peer_stream(ctx, &stream).await;
                    Ok(())
                }
                Err(SubscribeRoutingEventsStartError::Response(error)) => {
                    remove_inbound_peer_stream(ctx, &stream).await;
                    send_peer_response(tx, src, call_id, Some(error)).await?;
                    Ok(())
                }
                Err(SubscribeRoutingEventsStartError::ResponseThenClose { error, reason }) => {
                    remove_inbound_peer_stream(ctx, &stream).await;
                    send_peer_response(tx, src, call_id, Some(error)).await?;
                    Err(peer_protocol_error(reason))
                }
                Err(SubscribeRoutingEventsStartError::ConnectionClosed { reason }) => Err({
                    remove_inbound_peer_stream(ctx, &stream).await;
                    ConnectionError::Transport(crate::transport::TransportError::Io(
                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, reason),
                    ))
                }),
            }
        }
        method => {
            send_peer_response(
                tx,
                src,
                call_id,
                Some(ProtocolError::Unimplemented {
                    message: format!("unsupported peer method {method}"),
                }),
            )
            .await
        }
    }
}

fn routing_service_ctx(ctx: &ConnectionContext) -> RoutingServiceCtx {
    RoutingServiceCtx::new(ctx.user_state.clone(), ctx.link.clone(), ctx.routing_role)
}

enum PeerRoutingStreamStartError {
    Response(ProtocolError),
    ResponseThenClose {
        error: ProtocolError,
        reason: String,
    },
}

async fn register_peer_routing_stream(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: CallId,
    ctx: &ConnectionContext,
) -> std::result::Result<EndpointServerStream, PeerRoutingStreamStartError> {
    if !ctx.user_state.read().await.is_peer_link(&ctx.link) {
        return Err(PeerRoutingStreamStartError::ResponseThenClose {
            error: ProtocolError::InvalidArgument {
                message: "routing event subscription is only valid for peer connections"
                    .to_string(),
            },
            reason: "received peer routing subscription on non-peer connection".to_string(),
        });
    }
    let Some((reply_src, reply_dst)) = Route::reply(src) else {
        return Err(PeerRoutingStreamStartError::ResponseThenClose {
            error: ProtocolError::InvalidArgument {
                message: "routing event subscription requires a non-empty source route".to_string(),
            },
            reason: "received peer routing subscription with empty source route".to_string(),
        });
    };

    ctx.rpc()
        .register_endpoint_server_stream(EndpointServerStreamStart {
            tx: tx.clone(),
            reply_src,
            reply_dst,
            call_id,
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            owner_link: ctx.link.clone(),
            dedup_key: Some(peer_routing_dedup_key(&ctx.link)),
        })
        .map_err(peer_routing_start_error)
}

fn peer_routing_start_error(error: RegisterCallError) -> PeerRoutingStreamStartError {
    match error {
        RegisterCallError::DuplicateCallId { .. } => {
            PeerRoutingStreamStartError::ResponseThenClose {
                error: ProtocolError::AlreadyExists {
                    message: "routing event subscription call id already exists".to_string(),
                },
                reason: "duplicate peer routing subscription reused active call id".to_string(),
            }
        }
        RegisterCallError::DuplicateDedupKey { .. } => {
            PeerRoutingStreamStartError::Response(ProtocolError::AlreadyExists {
                message: "routing event subscription already exists for peer".to_string(),
            })
        }
    }
}

async fn remove_inbound_peer_stream(ctx: &ConnectionContext, stream: &EndpointServerStream) {
    ctx.rpc().remove_inbound_for_handle(&stream.handle);
}

async fn activate_inbound_peer_stream(ctx: &ConnectionContext, stream: &EndpointServerStream) {
    if !ctx.rpc().activate_inbound_for_handle(&stream.handle) {
        tracing::warn!(
            peer = %ctx.link,
            "routing event stream was removed before initial snapshot activation"
        );
    }
}

async fn accept_peer_routing_event(
    ctx: &ConnectionContext,
    call_id: &CallId,
    event: &RoutingEvent,
) -> bool {
    let is_peer_link = ctx.user_state.read().await.is_peer_link(&ctx.link);
    let active = is_peer_link
        && ctx
            .rpc()
            .outbound_for_call_matches(call_id, |call, resources| {
                call.method == method::ROUTING_SUBSCRIBE_EVENTS
                    && matches!(
                        resources,
                        Some(OutboundCallResources::PeerRoutingSubscription { link })
                            if link == &ctx.link
                    )
                    && matches!(
                        call.state,
                        OutboundCallState::AwaitingResponse | OutboundCallState::ActiveStream
                    )
            });
    if active {
        ctx.rpc()
            .set_outbound_state_for_call(call_id, OutboundCallState::ActiveStream);
        return true;
    }
    tracing::warn!(
        peer = %ctx.link,
        event = event.type_label(),
        "rejecting peer routing event on inactive stream"
    );
    false
}

async fn has_active_peer_routing_subscription(ctx: &ConnectionContext, call_id: &CallId) -> bool {
    let is_peer_link = ctx.user_state.read().await.is_peer_link(&ctx.link);
    is_peer_link
        && ctx
            .rpc()
            .outbound_for_call_matches(call_id, |call, resources| {
                call.method == method::ROUTING_SUBSCRIBE_EVENTS
                    && matches!(
                        resources,
                        Some(OutboundCallResources::PeerRoutingSubscription { link })
                            if link == &ctx.link
                    )
                    && matches!(
                        call.state,
                        OutboundCallState::AwaitingResponse | OutboundCallState::ActiveStream
                    )
            })
}

async fn finish_outbound_peer_routing_subscription(
    ctx: &ConnectionContext,
    call_id: &CallId,
    error: Option<&ProtocolError>,
) -> std::result::Result<(), String> {
    if let Some(error) = error {
        tracing::warn!(peer = %ctx.link, error = %error, "peer routing subscription failed");
    } else {
        tracing::warn!(peer = %ctx.link, "peer routing subscription completed");
    }
    let matched = ctx
        .rpc()
        .finish_outbound_peer_routing_subscription(&ctx.link, call_id);
    if matched {
        Err("peer routing subscription ended before connection close".to_string())
    } else {
        tracing::warn!(
            peer = %ctx.link,
            "dropping terminal peer response for inactive routing stream"
        );
        Ok(())
    }
}

async fn cancel_inbound_peer_routing_subscription(
    ctx: &ConnectionContext,
    call_id: &CallId,
) -> bool {
    let _us = ctx.user_state.write().await;
    ctx.rpc()
        .finish_inbound_peer_routing_subscription(&ctx.link, call_id)
}

async fn handle_peer_response(
    call_id: CallId,
    error: Option<ProtocolError>,
    ctx: &ConnectionContext,
) -> Result<()> {
    if ctx.is_local {
        tracing::warn!("rejecting peer response from local connection");
        return Ok(());
    }
    match finish_outbound_peer_routing_subscription(ctx, &call_id, error.as_ref()).await {
        Ok(()) => Ok(()),
        Err(reason) => Err(peer_protocol_error(reason)),
    }
}

async fn handle_peer_cancel(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: CallId,
    ctx: &ConnectionContext,
) -> Result<()> {
    if ctx.is_local {
        tracing::warn!("rejecting peer cancel from local connection");
        return Ok(());
    }
    let matched = cancel_inbound_peer_routing_subscription(ctx, &call_id).await;
    if !matched {
        tracing::warn!(peer = %ctx.link, "dropping peer cancel for inactive routing stream");
        return Ok(());
    }
    send_peer_response(
        tx,
        src,
        call_id,
        Some(ProtocolError::Cancelled {
            message: "routing event subscription cancelled".to_string(),
        }),
    )
    .await?;
    Err(peer_protocol_error(
        "peer cancelled routing event subscription",
    ))
}

pub(super) async fn handle_message(
    tx: &mpsc::Sender<Message>,
    msg: Message,
    ctx: &ConnectionContext,
) -> super::connection::Result<()> {
    tracing::debug!(msg_type = msg.type_label(), "received message");

    match msg {
        Message::Frame(frame) => {
            if !validate_frame_provenance(&frame.src, ctx) {
                return Ok(());
            }
            match frame.body {
                FrameBody::RoutingError {
                    failed_route,
                    error,
                } => {
                    handle_routing_error(
                        tx,
                        frame.src,
                        frame.dst,
                        frame.call_id,
                        failed_route,
                        error,
                        ctx,
                    )
                    .await
                }
                FrameBody::Request(request) if frame.dst.is_empty() => {
                    match method::find(&request.method).map(|spec| spec.access) {
                        Some(MethodAccess::Local) => {
                            if !ctx.is_local {
                                tracing::warn!(
                                    method = request.method,
                                    "rejecting local request from non-local connection"
                                );
                                return Ok(());
                            }
                            handle_local_request(tx, frame.src, frame.call_id, request, ctx).await
                        }
                        Some(MethodAccess::Peer) => {
                            handle_peer_request(tx, frame.src, frame.call_id, request, ctx).await
                        }
                        Some(MethodAccess::Routed) | None => {
                            handle_application_frame(
                                tx,
                                frame.src,
                                frame.dst,
                                frame.call_id,
                                FrameBody::Request(request),
                                ctx,
                            )
                            .await
                        }
                    }
                }
                FrameBody::Request(request) => match method::find(&request.method) {
                    Some(spec)
                        if matches!(spec.access, MethodAccess::Local | MethodAccess::Peer) =>
                    {
                        tracing::warn!(
                            method = request.method,
                            access = spec.access.as_str(),
                            dst = %frame.dst,
                            "rejecting non-routed request with remaining destination hops"
                        );
                        send_error_response(
                            tx,
                            frame.src,
                            frame.call_id,
                            ProtocolError::PermissionDenied {
                                message: format!(
                                    "method {} is not valid for routed delivery",
                                    request.method
                                ),
                            },
                            "non-routed request rejection",
                        )
                        .await
                    }
                    Some(_) | None => {
                        handle_application_frame(
                            tx,
                            frame.src,
                            frame.dst,
                            frame.call_id,
                            FrameBody::Request(request),
                            ctx,
                        )
                        .await
                    }
                },
                body if frame.dst.is_empty()
                    && has_active_peer_routing_subscription(ctx, &frame.call_id).await =>
                {
                    if !validate_adjacent_peer_source(&frame.src, ctx, "peer routing frame") {
                        return Ok(());
                    }
                    match body {
                        FrameBody::Response(ResponseFrame::Payload(_)) => {
                            handle_peer_response(frame.call_id, None, ctx).await
                        }
                        FrameBody::Response(ResponseFrame::Error(error)) => {
                            handle_peer_response(frame.call_id, Some(error), ctx).await
                        }
                        FrameBody::StreamItem(payload) => {
                            let event = match wire::decode_routing_event(&payload) {
                                Ok(event) => event,
                                Err(error) => {
                                    tracing::warn!(error = %error, "dropping malformed peer routing event");
                                    return Ok(());
                                }
                            };
                            if !accept_peer_routing_event(ctx, &frame.call_id, &event).await {
                                return Ok(());
                            }
                            handle_peer_event(event, ctx).await
                        }
                        FrameBody::Cancel => {
                            handle_peer_cancel(tx, frame.src, frame.call_id, ctx).await
                        }
                        body => {
                            handle_application_frame(
                                tx,
                                frame.src,
                                frame.dst,
                                frame.call_id,
                                body,
                                ctx,
                            )
                            .await
                        }
                    }
                }
                body => {
                    handle_application_frame(tx, frame.src, frame.dst, frame.call_id, body, ctx)
                        .await
                }
            }
        }
        Message::Ping => handle_ping(tx).await,
        Message::Pong => Ok(()),
        Message::Reauth(request) => handle_reauth(tx, request, ctx).await,
        Message::ReauthResponse(_) => {
            tracing::warn!("dropping unexpected reauth response");
            Ok(())
        }
        Message::GoAway(_) => {
            tracing::warn!("dropping unexpected goaway");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::protocol::Link;
    use crate::protocol::message::CallId;
    use crate::server::{ConnectionHandle, LOCAL_USER_ID, PeerRoutingOutboundStart, test_helpers};

    async fn test_peer_ctx(link: Link) -> ConnectionContext {
        test_ctx(link, false).await
    }

    async fn test_local_ctx(link: Link) -> ConnectionContext {
        test_ctx(link, true).await
    }

    async fn test_ctx(link: Link, is_local: bool) -> ConnectionContext {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
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

    #[tokio::test]
    async fn peer_routing_event_is_rejected_for_subscription_on_other_link() {
        let subscribed_link = Link::new("peer-a").unwrap();
        let event_link = Link::new("peer-b").unwrap();
        let call_id = CallId::from(Uuid::new_v4());
        let ctx = test_peer_ctx(event_link.clone()).await;

        {
            let mut us = ctx.user_state.write().await;
            let (_handle, _rx) = us.try_reserve_link(subscribed_link.clone()).unwrap();
            us.mark_peer_link(subscribed_link.clone());
            us.mark_peer_link(event_link.clone());
            us.rpc_for_link(&subscribed_link)
                .unwrap()
                .register_peer_routing_outbound(PeerRoutingOutboundStart {
                    link: subscribed_link.clone(),
                    call_id: call_id.clone(),
                    method: method::ROUTING_SUBSCRIBE_EVENTS,
                })
                .unwrap();
        }

        assert!(!accept_peer_routing_event(&ctx, &call_id, &RoutingEvent::SnapshotComplete).await);
        assert!(
            ctx.user_state
                .read()
                .await
                .rpc_for_link(&subscribed_link)
                .unwrap()
                .outbound_for_call_matches(&call_id, |call, _| {
                    matches!(call.state, OutboundCallState::AwaitingResponse)
                })
        );
    }

    #[tokio::test]
    async fn spoofed_peer_request_source_is_dropped() {
        let link = Link::new("peer").unwrap();
        let ctx = test_peer_ctx(link.clone()).await;
        let spoofed = Link::new("spoofed").unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let call_id = CallId::from(Uuid::new_v4());

        handle_message(
            &tx,
            Message::Frame(Frame {
                src: Route::from_link(spoofed),
                dst: Route::empty(),
                call_id: call_id.clone(),
                body: FrameBody::Request(RequestFrame {
                    method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                    payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
                }),
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(rx.try_recv().is_err());
        assert_eq!(ctx.rpc().inbound_len(), 0);
        assert!(ctx.user_state.read().await.rpc_for_link(&link).is_some());
    }

    #[tokio::test]
    async fn peer_request_with_routed_source_is_dropped() {
        let link = Link::new("peer").unwrap();
        let ctx = test_peer_ctx(link.clone()).await;
        let (tx, mut rx) = mpsc::channel(4);
        let call_id = CallId::from(Uuid::new_v4());

        handle_message(
            &tx,
            Message::Frame(Frame {
                src: Route::from_links([link.as_str().to_string(), "previous-hop".to_string()])
                    .unwrap(),
                dst: Route::empty(),
                call_id: call_id.clone(),
                body: FrameBody::Request(RequestFrame {
                    method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                    payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
                }),
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(rx.try_recv().is_err());
        assert_eq!(ctx.rpc().inbound_len(), 0);
    }

    #[tokio::test]
    async fn non_routed_request_with_remaining_destination_is_rejected_without_forwarding() {
        let local = Link::new("local").unwrap();
        let peer = Link::new("peer").unwrap();
        let ctx = test_local_ctx(local.clone()).await;
        let (tx, mut rx) = mpsc::channel(4);
        let (peer_tx, mut peer_rx) = mpsc::channel(4);
        let call_id = CallId::from(Uuid::new_v4());

        ctx.user_state.write().await.connections.insert(
            peer.clone(),
            crate::server::state::ConnectionEntry::new(
                ConnectionHandle::new(peer_tx),
                crate::server::state::ConnectionKind::Peer,
            ),
        );

        handle_message(
            &tx,
            Message::Frame(Frame {
                src: Route::from_link(local),
                dst: Route::from_link(peer),
                call_id: call_id.clone(),
                body: FrameBody::Request(RequestFrame {
                    method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                    payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
                }),
            }),
            &ctx,
        )
        .await
        .unwrap();

        assert!(peer_rx.try_recv().is_err());
        let Some(Message::Frame(Frame {
            call_id: response_call_id,
            body: FrameBody::Response(ResponseFrame::Error(ProtocolError::PermissionDenied { .. })),
            ..
        })) = rx.recv().await
        else {
            panic!("expected permission denied response");
        };
        assert_eq!(response_call_id, call_id);
    }

    #[tokio::test]
    async fn observer_connection_rejects_routing_subscription_with_failed_precondition() {
        let link = Link::new("observer").unwrap();
        let mut ctx = test_peer_ctx(link).await;
        ctx.routing_role = crate::protocol::handshake::RoutingRole::Observer;
        let (tx, mut rx) = mpsc::channel(4);
        let call_id = CallId::from(Uuid::new_v4());

        handle_peer_request(
            &tx,
            Route::from_link(ctx.link.clone()),
            call_id.clone(),
            RequestFrame {
                method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let Some(Message::Frame(Frame {
            call_id: response_call_id,
            body: FrameBody::Response(ResponseFrame::Error(error)),
            ..
        })) = rx.recv().await
        else {
            panic!("expected routing subscription error response");
        };
        assert_eq!(response_call_id, call_id);
        assert_eq!(
            error,
            ProtocolError::FailedPrecondition {
                message: "peer did not advertise a routing role that serves routing events"
                    .to_string(),
            }
        );
    }

    #[tokio::test]
    async fn relay_connection_serves_routing_subscription() {
        let link = Link::new("relay").unwrap();
        let mut ctx = test_peer_ctx(link).await;
        ctx.routing_role = crate::protocol::handshake::RoutingRole::Relay;
        let (tx, mut rx) = mpsc::channel(4);
        let call_id = CallId::from(Uuid::new_v4());

        handle_peer_request(
            &tx,
            Route::from_link(ctx.link.clone()),
            call_id.clone(),
            RequestFrame {
                method: method::ROUTING_SUBSCRIBE_EVENTS_NAME.to_string(),
                payload: wire::SubscribeRoutingEventsRequest {}.encode_to_vec(),
            },
            &ctx,
        )
        .await
        .unwrap();

        let Some(Message::Frame(Frame {
            call_id: response_call_id,
            body: FrameBody::StreamItem(_),
            ..
        })) = rx.recv().await
        else {
            panic!("expected routing subscription snapshot");
        };
        assert_eq!(response_call_id, call_id);
    }
}

//! Frame-scope dispatch adapters.
//!
//! This module decodes local, peer, and routed frames and routes them toward
//! protobuf services. It should not be treated as the service layer itself:
//! actual service modules should be named after protobuf services.

mod local;
mod peer;
mod routable;
mod rpc_runtime;

use local::handle_local_request;
use peer::{handle_peer_event, handle_ping, handle_reauth};
use prost::Message as _;
use routable::{handle_routable, handle_routing_error};
use tokio::sync::mpsc;

use super::connection::ConnectionContext;
use crate::protocol::message::{
    FrameBody, Message, PeerFrame, ProtocolError, RequestFrame, ResponseFrame, RoutedCallId,
    RoutingEvent,
};
use crate::protocol::route::Route;
use crate::protocol::{method, wire};
use crate::rpc::{
    DedupKey, OutboundCallState, RegisterCallError, RpcInboundServerStream, RpcServerStreamStart,
};
use crate::server::connection::{ConnectionError, Result};
use crate::services::{RoutingService, RoutingServiceCtx, SubscribeRoutingEventsStartError};

async fn send_peer_response(
    tx: &mpsc::Sender<Message>,
    call_id: RoutedCallId,
    error: Option<ProtocolError>,
) -> Result<()> {
    let response = match error {
        Some(error) => ResponseFrame::Error(error),
        None => ResponseFrame::Payload(wire::Empty {}.encode_to_vec()),
    };
    tx.send(Message::Peer(PeerFrame {
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

fn peer_protocol_error(message: impl Into<String>) -> ConnectionError {
    ConnectionError::Protocol(message.into())
}

async fn handle_peer_request(
    tx: &mpsc::Sender<Message>,
    call_id: RoutedCallId,
    request: RequestFrame,
    ctx: &ConnectionContext,
) -> Result<()> {
    if ctx.is_local {
        tracing::warn!(
            method = request.method,
            "rejecting peer request from local connection"
        );
        return Ok(());
    }

    match request.method.as_str() {
        method::ROUTING_SUBSCRIBE_EVENTS_NAME => {
            if let Err(error) =
                wire::SubscribeRoutingEventsRequest::decode(request.payload.as_slice())
            {
                send_peer_response(
                    tx,
                    call_id,
                    Some(ProtocolError::InvalidArgument {
                        message: format!("invalid SubscribeRoutingEvents request: {error}"),
                    }),
                )
                .await?;
                return Ok(());
            }

            let stream = register_peer_routing_stream(tx, call_id.clone(), ctx).await;
            let stream = match stream {
                Ok(stream) => stream,
                Err(PeerRoutingStreamStartError::Response(error)) => {
                    send_peer_response(tx, call_id, Some(error)).await?;
                    return Ok(());
                }
                Err(PeerRoutingStreamStartError::ResponseThenClose { error, reason }) => {
                    send_peer_response(tx, call_id, Some(error)).await?;
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
                    send_peer_response(tx, call_id, Some(error)).await?;
                    Ok(())
                }
                Err(SubscribeRoutingEventsStartError::ResponseThenClose { error, reason }) => {
                    remove_inbound_peer_stream(ctx, &stream).await;
                    send_peer_response(tx, call_id, Some(error)).await?;
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
    RoutingServiceCtx::new(ctx.state.clone(), ctx.user_state.clone(), ctx.link.clone())
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
    call_id: RoutedCallId,
    ctx: &ConnectionContext,
) -> std::result::Result<RpcInboundServerStream, PeerRoutingStreamStartError> {
    let mut us = ctx.user_state.write().await;
    if !us.is_peer_link(&ctx.link) {
        return Err(PeerRoutingStreamStartError::ResponseThenClose {
            error: ProtocolError::InvalidArgument {
                message: "routing event subscription is only valid for peer connections"
                    .to_string(),
            },
            reason: "received peer routing subscription on non-peer connection".to_string(),
        });
    }

    let peer_route = Route::from_link(ctx.link.clone());
    us.rpc
        .register_server_stream(RpcServerStreamStart {
            tx: tx.clone(),
            counterparty_route: peer_route.clone(),
            call_id,
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            dedup_key: Some(DedupKey::PeerRoutingSubscription {
                counterparty_route: peer_route,
            }),
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
        RegisterCallError::DuplicateDedupKey {
            key: DedupKey::PeerRoutingSubscription { .. },
            ..
        } => PeerRoutingStreamStartError::Response(ProtocolError::AlreadyExists {
            message: "routing event subscription already exists for peer".to_string(),
        }),
        RegisterCallError::DuplicateDedupKey { key, .. } => {
            PeerRoutingStreamStartError::Response(ProtocolError::AlreadyExists {
                message: format!("duplicate peer routing subscription conflicts with {key:?}"),
            })
        }
    }
}

async fn remove_inbound_peer_stream(ctx: &ConnectionContext, stream: &RpcInboundServerStream) {
    ctx.user_state
        .write()
        .await
        .rpc
        .remove_inbound_for_handle(&stream.handle);
}

async fn activate_inbound_peer_stream(ctx: &ConnectionContext, stream: &RpcInboundServerStream) {
    if !ctx
        .user_state
        .write()
        .await
        .rpc
        .activate_inbound_for_handle(&stream.handle)
    {
        tracing::warn!(
            peer = %ctx.link,
            "routing event stream was removed before initial snapshot activation"
        );
    }
}

async fn accept_peer_routing_event(
    ctx: &ConnectionContext,
    call_id: &RoutedCallId,
    event: &RoutingEvent,
) -> bool {
    let mut us = ctx.user_state.write().await;
    let peer_route = Route::from_link(ctx.link.clone());
    let active = us.is_peer_link(&ctx.link)
        && us
            .rpc
            .outbound_for_route(&peer_route, call_id)
            .is_some_and(|call| {
                call.method == method::ROUTING_SUBSCRIBE_EVENTS
                    && matches!(
                        call.state,
                        OutboundCallState::AwaitingResponse | OutboundCallState::ActiveStream
                    )
            });
    if active {
        us.rpc
            .set_outbound_state_for_route(&peer_route, call_id, OutboundCallState::ActiveStream);
        return true;
    }
    tracing::warn!(
        peer = %ctx.link,
        event = event.type_label(),
        "rejecting peer routing event on inactive stream"
    );
    false
}

async fn finish_outbound_peer_routing_subscription(
    ctx: &ConnectionContext,
    call_id: &RoutedCallId,
    error: Option<&ProtocolError>,
) -> std::result::Result<(), String> {
    if let Some(error) = error {
        tracing::warn!(peer = %ctx.link, error = %error, "peer routing subscription failed");
    } else {
        tracing::warn!(peer = %ctx.link, "peer routing subscription completed");
    }
    let matched = ctx
        .user_state
        .write()
        .await
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
    call_id: &RoutedCallId,
) -> bool {
    ctx.user_state
        .write()
        .await
        .finish_inbound_peer_routing_subscription(&ctx.link, call_id)
}

async fn handle_peer_response(
    call_id: RoutedCallId,
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
    call_id: RoutedCallId,
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
    match &msg {
        Message::Routed(_) => tracing::trace!("received routed frame"),
        _ => tracing::debug!(msg_type = msg.type_label(), "received message"),
    }

    match msg {
        Message::Routed(frame) => match frame.message {
            crate::protocol::message::RoutedFrameMessage::Payload(payload) => {
                handle_routable(tx, frame.src, frame.dst, frame.call_id, payload, ctx).await
            }
            crate::protocol::message::RoutedFrameMessage::RoutingError {
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
        },
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
        Message::Peer(frame) => match frame.body {
            FrameBody::Request(request) => {
                handle_peer_request(tx, frame.call_id, request, ctx).await
            }
            FrameBody::Response(ResponseFrame::Payload(_)) => {
                handle_peer_response(frame.call_id, None, ctx).await
            }
            FrameBody::Response(ResponseFrame::Error(error)) => {
                handle_peer_response(frame.call_id, Some(error), ctx).await
            }
            FrameBody::StreamItem(payload) => {
                if ctx.is_local {
                    tracing::warn!("rejecting peer routing event from local connection");
                    return Ok(());
                }
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
            FrameBody::Cancel => handle_peer_cancel(tx, frame.call_id, ctx).await,
        },
        Message::PeerSnapshot { .. } => {
            tracing::warn!("dropping internal peer snapshot batch sent to dispatcher");
            Ok(())
        }
        Message::Local(frame) => {
            if !ctx.is_local {
                tracing::warn!("rejecting local frame from remote peer");
                return Ok(());
            }
            match frame.body {
                FrameBody::Request(request) => {
                    handle_local_request(tx, frame.call_id, request, ctx).await
                }
                FrameBody::Response(_) | FrameBody::StreamItem(_) | FrameBody::Cancel => {
                    tracing::warn!("dropping unexpected local frame body");
                    Ok(())
                }
            }
        }
    }
}

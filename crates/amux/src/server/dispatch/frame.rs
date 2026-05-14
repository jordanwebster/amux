use tokio::sync::mpsc;

use super::rpc_runtime;
use crate::protocol::Route;
use crate::protocol::message::{CallId, Frame, FrameBody, Message, ProtocolError, ResponseFrame};
#[cfg(test)]
use crate::protocol::method;
use crate::server::connection::ConnectionContext;
use crate::server::routing::{FrameForwardingResult, forward_frame_or_endpoint};

pub(super) fn validate_frame_provenance(src: &Route, ctx: &ConnectionContext) -> bool {
    match src.peek() {
        Some(link) if *link == ctx.link => true,
        Some(link) => {
            tracing::warn!(
                inbound_link = %ctx.link,
                src_first_hop = %link,
                "dropping application frame with unexpected src route"
            );
            false
        }
        None => {
            tracing::warn!(
                inbound_link = %ctx.link,
                "dropping application frame with empty src route"
            );
            false
        }
    }
}

pub(super) async fn handle_application_frame(
    tx: &mpsc::Sender<Message>,
    src: Route,
    dst: Route,
    call_id: CallId,
    body: FrameBody,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if !validate_frame_provenance(&src, ctx) {
        return Ok(());
    }

    match rpc_runtime::track_forwarded_local_request_if_any(&src, &dst, &call_id, &body, ctx).await
    {
        rpc_runtime::LocalRequestTracking::Continue => {}
        rpc_runtime::LocalRequestTracking::Reject {
            failed_route,
            error,
        } => {
            return reject_duplicate_forwarded_local_request(tx, src, call_id, failed_route, error)
                .await;
        }
    }

    let (src, call_id, body) =
        match forward_frame_or_endpoint(tx, &ctx.user_state, src, dst, call_id, body).await {
            FrameForwardingResult::Forwarded => return Ok(()),
            FrameForwardingResult::Endpoint { src, call_id, body } => (src, call_id, body),
        };

    if ctx.state.read().await.is_cloud_server() {
        return reject_cloud_relay_endpoint_frame(tx, src, call_id, body).await;
    }

    rpc_runtime::handle_endpoint_frame(tx, src, call_id, body, ctx).await
}

async fn reject_duplicate_forwarded_local_request(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: CallId,
    failed_route: Route,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let Some((reply_src, reply_dst)) = Route::reply(src) else {
        tracing::warn!("dropping duplicate local-origin forwarded request with empty src route");
        return Ok(());
    };
    let _ = tx
        .send(Message::routing_error_for_route(
            reply_src,
            reply_dst,
            call_id,
            failed_route,
            error,
        ))
        .await;
    Ok(())
}

async fn reject_cloud_relay_endpoint_frame(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: CallId,
    request_body: FrameBody,
) -> crate::server::connection::Result<()> {
    let Some((reply_src, reply_dst)) = Route::reply(src) else {
        tracing::warn!("dropping endpoint frame to cloud relay with empty src route");
        return Ok(());
    };
    let error = match &request_body {
        FrameBody::Request(request)
            if request.method == crate::protocol::method::AGENT_SUBSCRIBE_EVENTS_NAME =>
        {
            ProtocolError::FailedPrecondition {
                message: "host has no supported agent types".to_string(),
            }
        }
        _ => ProtocolError::ServerError {
            message: "cloud relays do not host remote service endpoints".to_string(),
        },
    };
    let _ = tx
        .send(Message::Frame(Frame {
            src: reply_src,
            dst: reply_dst,
            call_id,
            body: FrameBody::Response(ResponseFrame::Error(error)),
        }))
        .await;
    Ok(())
}

pub(super) async fn handle_routing_error(
    tx: &mpsc::Sender<Message>,
    mut src: Route,
    mut dst: Route,
    call_id: CallId,
    failed_route: Route,
    error: ProtocolError,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if !validate_frame_provenance(&src, ctx) {
        return Ok(());
    }

    if let Some(next_hop) = dst.pop() {
        let hop_name = next_hop.clone();
        let route_tx = {
            let us = ctx.user_state.read().await;
            let route_tx = us.connection_for_route(&Route::from_link(hop_name.clone()));
            if route_tx.is_some() && !us.is_peer_link(&hop_name) {
                us.remove_local_origin_outbound_for_return_hop_and_failed_route(
                    &call_id,
                    &hop_name,
                    &failed_route,
                );
            }
            route_tx
        };

        if let Some(route_tx) = route_tx {
            src.push(next_hop);
            let _ = route_tx
                .send(Message::routing_error_for_route(
                    src,
                    dst,
                    call_id,
                    failed_route,
                    error,
                ))
                .await;
        } else {
            tracing::debug!(
                return_hop = %hop_name,
                "dropping routing error because return route is gone"
            );
        }
    } else {
        tracing::debug!(%error, "routing error reached local endpoint");
        let cleanup_route = if !failed_route.is_empty()
            && !src.is_empty()
            && failed_route.starts_with_route(&src)
        {
            Some(failed_route)
        } else {
            tracing::warn!(
                src = %src,
                failed_route = %failed_route,
                "dropping routing error cleanup with inconsistent failed route"
            );
            None
        };
        let (cancelled, cleanup_jobs, removed_agent_subscription) =
            if let Some(failed_route) = cleanup_route {
                let mut us = ctx.user_state.write().await;
                let removed_agent_subscription =
                    us.remove_agent_subscription_for_route_and_call(&call_id, &failed_route);
                let (cancelled, cleanup_jobs) =
                    crate::server::cancel_session_subscription_for_route_and_call(
                        &mut us,
                        &failed_route,
                        &call_id,
                    );
                (cancelled, cleanup_jobs, removed_agent_subscription)
            } else {
                (0, Vec::new(), false)
            };
        crate::server::finish_session_subscription_cleanup_jobs(&ctx.user_state, cleanup_jobs)
            .await;
        if cancelled != 0 {
            tracing::debug!(
                count = cancelled,
                "cancelled session subscriptions after routing error"
            );
        }
        if removed_agent_subscription {
            tracing::debug!("removed agent subscription after routing error");
        }
        let _ = tx;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::protocol::Link;
    use crate::protocol::message::{FrameBody, RequestFrame, ResponseFrame};
    use crate::server::{ConnectionHandle, LOCAL_USER_ID, test_helpers};

    async fn test_ctx() -> ConnectionContext {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        let link = Link::new("test-link").unwrap();
        let rpc = {
            let mut us = user_state.write().await;
            let (_handle, _rx) = us.try_reserve_link(link.clone()).unwrap();
            us.rpc_for_link(&link).unwrap()
        };
        ConnectionContext {
            state,
            rpc,
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link,
            is_local: true,
            heartbeat: None,
            routing_role: crate::protocol::handshake::RoutingRole::Observer,
        }
    }

    fn expect_no_message(rx: &mut mpsc::Receiver<Message>) {
        assert!(
            matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "expected no response"
        );
    }

    #[tokio::test]
    async fn duplicate_forwarded_local_request_is_rejected_without_forwarding() {
        let (tx, mut rx) = mpsc::channel(4);
        let (peer_tx, mut peer_rx) = mpsc::channel(4);
        let call_id = CallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;
        let peer = Link::new("peer").unwrap();
        let src = Route::from_link(ctx.link.clone());
        let dst = Route::from_link(peer.clone());
        let full_route =
            Route::from_links([ctx.link.as_str().to_string(), peer.as_str().to_string()]).unwrap();
        let body = FrameBody::Request(RequestFrame {
            method: method::AGENT_RENAME_NAME.to_string(),
            payload: Vec::new(),
        });

        ctx.user_state.write().await.connections.insert(
            peer.clone(),
            crate::server::state::ConnectionEntry::new(
                ConnectionHandle::new(peer_tx),
                crate::server::state::ConnectionKind::LocalClient,
            ),
        );
        ctx.user_state
            .write()
            .await
            .ensure_route_rpc(Route::from_link(peer.clone()));

        handle_application_frame(
            &tx,
            src.clone(),
            dst.clone(),
            call_id.clone(),
            body.clone(),
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            peer_rx.recv().await,
            Some(Message::Frame(Frame {
                body: FrameBody::Request(_),
                ..
            }))
        ));
        assert_eq!(ctx.user_state.read().await.total_outbound_len(), 1);

        handle_application_frame(&tx, src, dst, call_id.clone(), body, &ctx)
            .await
            .unwrap();

        let Some(Message::Frame(Frame {
            call_id: response_call_id,
            dst: response_dst,
            body:
                FrameBody::RoutingError {
                    failed_route,
                    error,
                },
            ..
        })) = rx.recv().await
        else {
            panic!("expected duplicate local-origin routing error");
        };
        assert_eq!(response_call_id, call_id);
        assert!(response_dst.is_empty());
        assert_eq!(failed_route, full_route);
        assert!(matches!(error, ProtocolError::AlreadyExists { .. }));
        assert!(matches!(
            peer_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn cloud_relay_rejects_endpoint_frame_without_decoding() {
        let (tx, mut rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;
        ctx.state.write().await.is_cloud_server = true;

        handle_application_frame(
            &tx,
            Route::from_link(ctx.link.clone()),
            Route::empty(),
            call_id.clone(),
            FrameBody::StreamItem(b"opaque".to_vec()),
            &ctx,
        )
        .await
        .unwrap();

        let Some(Message::Frame(Frame {
            call_id: response_call_id,
            body: FrameBody::Response(ResponseFrame::Error(ProtocolError::ServerError { message })),
            ..
        })) = rx.recv().await
        else {
            panic!("expected cloud relay endpoint rejection");
        };
        assert_eq!(response_call_id, call_id);
        assert_eq!(message, "cloud relays do not host remote service endpoints");
        let us = ctx.user_state.read().await;
        assert_eq!(us.total_inbound_len(), 0);
    }

    #[tokio::test]
    async fn non_local_spoofed_source_is_dropped() {
        let (tx, mut rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let mut ctx = test_ctx().await;
        ctx.is_local = false;
        ctx.link = Link::new("real-peer").unwrap();

        handle_application_frame(
            &tx,
            Route::from_link(Link::new("spoofed-peer").unwrap()),
            Route::empty(),
            call_id,
            FrameBody::Request(RequestFrame {
                method: method::AGENT_CREATE_NAME.to_string(),
                payload: Vec::new(),
            }),
            &ctx,
        )
        .await
        .unwrap();

        expect_no_message(&mut rx);
    }

    #[tokio::test]
    async fn local_empty_source_is_dropped() {
        let (tx, mut rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;

        handle_application_frame(
            &tx,
            Route::empty(),
            Route::empty(),
            call_id,
            FrameBody::Request(RequestFrame {
                method: method::AGENT_CREATE_NAME.to_string(),
                payload: Vec::new(),
            }),
            &ctx,
        )
        .await
        .unwrap();

        expect_no_message(&mut rx);
    }

    #[tokio::test]
    async fn local_spoofed_source_is_dropped() {
        let (tx, mut rx) = mpsc::channel(1);
        let call_id = CallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;

        handle_application_frame(
            &tx,
            Route::from_link(Link::new("spoofed-local").unwrap()),
            Route::empty(),
            call_id,
            FrameBody::Request(RequestFrame {
                method: method::AGENT_CREATE_NAME.to_string(),
                payload: Vec::new(),
            }),
            &ctx,
        )
        .await
        .unwrap();

        expect_no_message(&mut rx);
    }
}

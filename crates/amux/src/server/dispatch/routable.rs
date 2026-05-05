use tokio::sync::mpsc;

use super::rpc_runtime;
use crate::protocol::Route;
use crate::protocol::message::{
    FrameBody, Message, ProtocolError, ResponseFrame, RoutedCallId, RoutedFrame, RoutedFrameMessage,
};
#[cfg(test)]
use crate::protocol::method;
use crate::server::connection::ConnectionContext;
use crate::server::routing::{ForwardedRoutedPayload, forward_routed_payload_or_endpoint};

fn validate_routable_provenance(src: &Route, ctx: &ConnectionContext) -> bool {
    if ctx.is_local {
        return true;
    }

    match src.peek() {
        Some(link) if *link == ctx.link => true,
        Some(link) => {
            tracing::warn!(
                inbound_link = %ctx.link,
                src_first_hop = %link,
                "dropping routable message with spoofed src route"
            );
            false
        }
        None => {
            tracing::warn!(
                inbound_link = %ctx.link,
                "dropping routable message from non-local connection with empty src route"
            );
            false
        }
    }
}

pub(super) async fn handle_routable(
    tx: &mpsc::Sender<Message>,
    src: Route,
    dst: Route,
    call_id: RoutedCallId,
    payload: Vec<u8>,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if !validate_routable_provenance(&src, ctx) {
        return Ok(());
    }

    match rpc_runtime::register_local_origin_routed_request_if_any(
        &src, &dst, &call_id, &payload, ctx,
    )
    .await
    {
        rpc_runtime::LocalOriginRoutedRegistration::Continue => {}
        rpc_runtime::LocalOriginRoutedRegistration::Reject {
            failed_route,
            error,
        } => {
            return reject_duplicate_local_origin_routed_request(
                tx,
                src,
                call_id,
                failed_route,
                error,
            )
            .await;
        }
    }

    let (src, call_id, payload) =
        match forward_routed_payload_or_endpoint(tx, &ctx.user_state, src, dst, call_id, payload)
            .await
        {
            ForwardedRoutedPayload::Forwarded => return Ok(()),
            ForwardedRoutedPayload::Endpoint {
                src,
                call_id,
                payload,
            } => (src, call_id, payload),
        };

    if ctx.state.read().await.is_cloud_server() {
        return reject_cloud_relay_endpoint_payload(tx, src, call_id).await;
    }

    let body = match crate::protocol::wire::decode_frame_body(&payload) {
        Ok(body) => body,
        Err(error) => {
            return rpc_runtime::handle_malformed_routed_frame_body(tx, src, call_id, error, ctx)
                .await;
        }
    };

    rpc_runtime::handle_routed_endpoint_frame(tx, src, call_id, body, ctx).await
}

async fn reject_duplicate_local_origin_routed_request(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: RoutedCallId,
    failed_route: Route,
    error: ProtocolError,
) -> crate::server::connection::Result<()> {
    let Some((reply_src, reply_dst)) = Route::reply(src) else {
        tracing::warn!("dropping duplicate local-origin routed request with empty src route");
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

async fn reject_cloud_relay_endpoint_payload(
    tx: &mpsc::Sender<Message>,
    src: Route,
    call_id: RoutedCallId,
) -> crate::server::connection::Result<()> {
    let Some((reply_src, reply_dst)) = Route::reply(src) else {
        tracing::warn!("dropping endpoint routed payload to cloud relay with empty src route");
        return Ok(());
    };
    let payload = crate::protocol::wire::encode_frame_body(&FrameBody::Response(
        ResponseFrame::Error(ProtocolError::ServerError {
            message: "cloud relays do not host routed service endpoints".to_string(),
        }),
    ))
    .map_err(|error| {
        crate::server::connection::ConnectionError::Config(format!(
            "failed to encode cloud relay endpoint rejection: {error}"
        ))
    })?;

    let _ = tx
        .send(Message::Routed(RoutedFrame {
            src: reply_src,
            dst: reply_dst,
            call_id,
            message: RoutedFrameMessage::Payload(payload),
        }))
        .await;
    Ok(())
}

pub(super) async fn handle_routing_error(
    tx: &mpsc::Sender<Message>,
    mut src: Route,
    mut dst: Route,
    call_id: RoutedCallId,
    failed_route: Route,
    error: ProtocolError,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    if !validate_routable_provenance(&src, ctx) {
        return Ok(());
    }

    if let Some(next_hop) = dst.pop() {
        let hop_name = next_hop.clone();
        let route_tx = {
            let mut us = ctx.user_state.write().await;
            let route_tx = us.topology.route(&hop_name);
            if route_tx.is_some() && !us.topology.peer_links.contains(&hop_name) {
                us.rpc
                    .remove_outbound_for_route_if(&failed_route, &call_id, |call| {
                        call.resources
                            .as_ref()
                            .and_then(|resources| resources.local_origin())
                            .is_some_and(|(owner_link, _, _)| *owner_link == hop_name)
                    });
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
        let (cancelled, cleanup_jobs) = if let Some(failed_route) = cleanup_route {
            let mut us = ctx.user_state.write().await;
            crate::server::cancel_open_session_for_route_and_call(&mut us, &failed_route, &call_id)
        } else {
            (0, Vec::new())
        };
        crate::server::finish_open_session_cleanup_jobs(&ctx.user_state, cleanup_jobs).await;
        if cancelled != 0 {
            tracing::debug!(
                count = cancelled,
                "cancelled open sessions after routing error"
            );
        }
        let _ = tx;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;
    use tokio::time::timeout;
    use uuid::Uuid;

    use super::*;
    use crate::agent::{AgentSession, TEST_ECHO_V1, TestAgentSession};
    use crate::protocol::Link;
    use crate::protocol::message::{
        FrameBody, RequestFrame, ResponseFrame, RoutedFrame, RoutedFrameMessage,
    };
    use crate::protocol::open_session::{self, OpenSessionServerFrame};
    use crate::server::{ConnectionHandle, LOCAL_USER_ID, test_helpers};

    async fn test_ctx() -> ConnectionContext {
        let (state, user_state) = test_helpers::test_state().await;
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("test-link").unwrap(),
            is_local: true,
            heartbeat: None,
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    async fn expect_open_session_invalid_argument(rx: &mut mpsc::Receiver<Message>) -> String {
        timeout(Duration::from_secs(1), async {
            loop {
                let Some(Message::Routed(RoutedFrame {
                    message: RoutedFrameMessage::Payload(payload),
                    ..
                })) = rx.recv().await
                else {
                    panic!("expected routed OpenSession response");
                };
                match open_session::decode_open_session_server_frame(&payload).unwrap() {
                    OpenSessionServerFrame::Response(Err(ProtocolError::InvalidArgument {
                        message,
                    })) => return message,
                    OpenSessionServerFrame::Event(_) => {}
                    other => {
                        panic!("expected OpenSession invalid argument response, got {other:?}")
                    }
                }
            }
        })
        .await
        .expect("timed out waiting for routed OpenSession response")
    }

    async fn wait_for_no_open_session_state(ctx: &ConnectionContext) {
        timeout(Duration::from_secs(1), async {
            loop {
                {
                    let us = ctx.user_state.read().await;
                    if us.rpc.inbound_len() == 0 {
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for OpenSession cleanup");
    }

    fn encode_frame_body(body: FrameBody) -> Vec<u8> {
        crate::protocol::wire::encode_frame_body(&body).unwrap()
    }

    async fn insert_test_echo_agent(ctx: &ConnectionContext) -> Uuid {
        let agent_id = Uuid::new_v4();
        ctx.user_state.write().await.agents.insert(
            agent_id,
            AgentSession::TestAgent(TestAgentSession::echo_for_tests(agent_id, None)),
        );
        agent_id
    }

    fn expect_no_message(rx: &mut mpsc::Receiver<Message>) {
        assert!(
            matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "expected no routed response"
        );
    }

    #[tokio::test]
    async fn duplicate_local_origin_routed_request_is_rejected_without_forwarding() {
        let (tx, mut rx) = mpsc::channel(4);
        let (peer_tx, mut peer_rx) = mpsc::channel(4);
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;
        let peer = Link::new("peer").unwrap();
        let src = Route::from_link(ctx.link.clone());
        let dst = Route::from_link(peer.clone());
        let full_route =
            Route::from_links([ctx.link.as_str().to_string(), peer.as_str().to_string()]).unwrap();
        let payload = encode_frame_body(FrameBody::Request(RequestFrame {
            method: method::AGENT_RENAME_NAME.to_string(),
            payload: Vec::new(),
        }));

        ctx.user_state
            .write()
            .await
            .topology
            .routes
            .insert(peer.clone(), ConnectionHandle::new(peer_tx));

        handle_routable(
            &tx,
            src.clone(),
            dst.clone(),
            call_id.clone(),
            payload.clone(),
            &ctx,
        )
        .await
        .unwrap();

        assert!(matches!(
            peer_rx.recv().await,
            Some(Message::Routed(RoutedFrame {
                message: RoutedFrameMessage::Payload(_),
                ..
            }))
        ));
        assert_eq!(ctx.user_state.read().await.rpc.outbound_len(), 1);

        handle_routable(&tx, src, dst, call_id.clone(), payload, &ctx)
            .await
            .unwrap();

        let Some(Message::Routed(RoutedFrame {
            call_id: response_call_id,
            dst: response_dst,
            message:
                RoutedFrameMessage::RoutingError {
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
    async fn cloud_relay_rejects_endpoint_routed_payload_without_decoding() {
        let (tx, mut rx) = mpsc::channel(1);
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;
        ctx.state.write().await.is_cloud_server = true;

        handle_routable(
            &tx,
            Route::from_link(Link::new("client-link").unwrap()),
            Route::empty(),
            call_id.clone(),
            b"not a protobuf FrameBody".to_vec(),
            &ctx,
        )
        .await
        .unwrap();

        let Some(Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message: RoutedFrameMessage::Payload(payload),
            ..
        })) = rx.recv().await
        else {
            panic!("expected routed cloud relay rejection");
        };
        assert_eq!(response_call_id, call_id);
        let FrameBody::Response(ResponseFrame::Error(ProtocolError::ServerError { message })) =
            crate::protocol::wire::decode_frame_body(&payload).unwrap()
        else {
            panic!("expected cloud relay server error");
        };
        assert_eq!(message, "cloud relays do not host routed service endpoints");
        let us = ctx.user_state.read().await;
        assert_eq!(us.rpc.inbound_len(), 0);
    }

    #[tokio::test]
    async fn open_session_input_before_request_is_dropped_as_stale() {
        let (tx, mut rx) = mpsc::channel(1);
        let call_id = RoutedCallId::from(Uuid::new_v4());

        handle_routable(
            &tx,
            Route::from_link(Link::new("client-link").unwrap()),
            Route::empty(),
            call_id,
            open_session::encode_open_session_input(Vec::new(), b"input".to_vec()).unwrap(),
            &test_ctx().await,
        )
        .await
        .unwrap();

        expect_no_message(&mut rx);
    }

    #[tokio::test]
    async fn open_session_response_frame_after_request_terminates_call() {
        let (tx, mut rx) = mpsc::channel(4);
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;
        let agent_id = insert_test_echo_agent(&ctx).await;
        let src = Route::from_link(Link::new("client-link").unwrap());

        handle_routable(
            &tx,
            src.clone(),
            Route::empty(),
            call_id.clone(),
            open_session::encode_open_session_request(agent_id, TEST_ECHO_V1, None).unwrap(),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(ctx.user_state.read().await.rpc.inbound_len(), 1);

        handle_routable(
            &tx,
            src,
            Route::empty(),
            call_id,
            encode_frame_body(FrameBody::Response(ResponseFrame::Payload(Vec::new()))),
            &ctx,
        )
        .await
        .unwrap();

        assert!(
            expect_open_session_invalid_argument(&mut rx)
                .await
                .contains("response frame is not valid for active client-streaming call")
        );
        wait_for_no_open_session_state(&ctx).await;
    }

    #[tokio::test]
    async fn open_session_request_frame_after_request_terminates_call() {
        let (tx, mut rx) = mpsc::channel(4);
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;
        let agent_id = insert_test_echo_agent(&ctx).await;
        let src = Route::from_link(Link::new("client-link").unwrap());

        handle_routable(
            &tx,
            src.clone(),
            Route::empty(),
            call_id.clone(),
            open_session::encode_open_session_request(agent_id, TEST_ECHO_V1, None).unwrap(),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(ctx.user_state.read().await.rpc.inbound_len(), 1);

        handle_routable(
            &tx,
            src,
            Route::empty(),
            call_id,
            encode_frame_body(FrameBody::Request(RequestFrame {
                method: method::AGENT_CREATE_NAME.to_string(),
                payload: Vec::new(),
            })),
            &ctx,
        )
        .await
        .unwrap();

        assert!(
            expect_open_session_invalid_argument(&mut rx)
                .await
                .contains("request frame is not valid for active client-streaming call")
        );
        wait_for_no_open_session_state(&ctx).await;
    }

    #[tokio::test]
    async fn malformed_routed_payload_then_stale_open_session_frame_sends_one_terminal_response() {
        let (tx, mut rx) = mpsc::channel(2);
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;

        handle_routable(
            &tx,
            Route::from_link(Link::new("client-link").unwrap()),
            Route::empty(),
            call_id.clone(),
            vec![0xff],
            &ctx,
        )
        .await
        .unwrap();

        assert!(
            expect_open_session_invalid_argument(&mut rx)
                .await
                .contains("invalid varint")
        );

        handle_routable(
            &tx,
            Route::from_link(Link::new("client-link").unwrap()),
            Route::empty(),
            call_id,
            open_session::encode_open_session_input(Vec::new(), b"input".to_vec()).unwrap(),
            &ctx,
        )
        .await
        .unwrap();

        expect_no_message(&mut rx);
    }

    #[tokio::test]
    async fn malformed_routed_payload_for_active_open_session_closes_call_once() {
        let (tx, mut rx) = mpsc::channel(4);
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let ctx = test_ctx().await;
        let agent_id = insert_test_echo_agent(&ctx).await;
        let src = Route::from_link(Link::new("client-link").unwrap());

        handle_routable(
            &tx,
            src.clone(),
            Route::empty(),
            call_id.clone(),
            open_session::encode_open_session_request(agent_id, TEST_ECHO_V1, None).unwrap(),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(ctx.user_state.read().await.rpc.inbound_len(), 1);

        handle_routable(
            &tx,
            src.clone(),
            Route::empty(),
            call_id.clone(),
            vec![0xff],
            &ctx,
        )
        .await
        .unwrap();

        assert!(
            expect_open_session_invalid_argument(&mut rx)
                .await
                .contains("invalid varint")
        );
        wait_for_no_open_session_state(&ctx).await;

        handle_routable(
            &tx,
            src,
            Route::empty(),
            call_id,
            open_session::encode_open_session_input(Vec::new(), b"input".to_vec()).unwrap(),
            &ctx,
        )
        .await
        .unwrap();

        expect_no_message(&mut rx);
    }
}

use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};

use crate::protocol::message::{CallId, Frame, FrameBody, Message, ProtocolError};
use crate::protocol::{Link, Route};
use crate::server::ServerUserState;

pub(in crate::server) enum FrameForwardingResult {
    Forwarded,
    Endpoint {
        src: Route,
        call_id: CallId,
        body: FrameBody,
    },
}

fn frame_message(src: Route, dst: Route, call_id: CallId, body: FrameBody) -> Message {
    Message::Frame(Frame {
        src,
        dst,
        call_id,
        body,
    })
}

fn failed_route_from_parts(src: &Route, next_hop: &Link, remaining_dst: &Route) -> Route {
    let mut links: Vec<String> = src.iter().map(|link| link.as_str().to_string()).collect();
    links.reverse();
    links.push(next_hop.as_str().to_string());
    links.extend(remaining_dst.iter().map(|link| link.as_str().to_string()));
    Route::from_links(links).expect("failed route is composed from already-validated links")
}

fn route_with_next_hop(src: &Route, next_hop: &Link) -> Route {
    let mut route = src.clone();
    route.push(next_hop.clone());
    route
}

/// Forward an application frame if `dst` still has a next hop.
///
/// Relays stay stateless: this function rewrites route hops and forwards the
/// frame body, or returns/synthesizes a routing-layer error when the next hop
/// is unreachable. Endpoint service payloads are not decoded here.
pub(in crate::server) async fn forward_frame_or_endpoint(
    tx: &mpsc::Sender<Message>,
    user_state: &Arc<RwLock<ServerUserState>>,
    mut src: Route,
    mut dst: Route,
    call_id: CallId,
    body: FrameBody,
) -> FrameForwardingResult {
    let Some(next_hop) = dst.pop() else {
        return FrameForwardingResult::Endpoint { src, call_id, body };
    };

    let hop_name = next_hop.clone();
    let missing_failed_route = failed_route_from_parts(&src, &hop_name, &dst);
    let missing_reply = Route::reply(src.clone());
    let response_route = route_with_next_hop(&src, &hop_name);
    src.push(next_hop);

    let route_tx = {
        let us = user_state.read().await;
        let route_tx = us.connection_for_route(&Route::from_link(hop_name.clone()));
        if route_tx.is_some() && !us.is_peer_link(&hop_name) && is_terminal_response_body(&body) {
            us.remove_local_origin_outbound_for_return_hop(&call_id, &hop_name, &response_route);
        }
        route_tx
    };

    match route_tx {
        Some(route_tx) => {
            if route_tx
                .send(frame_message(src, dst, call_id.clone(), body))
                .await
                .is_err()
            {
                tracing::debug!(next_hop = %hop_name, "forwarding failed (channel closed)");
                remove_tracked_outbound_for_routing_error(
                    user_state,
                    &missing_failed_route,
                    &call_id,
                )
                .await;
                if let Some((reply_src, reply_dst)) = missing_reply {
                    let _ = tx
                        .send(Message::routing_error_for_route(
                            reply_src,
                            reply_dst,
                            call_id,
                            missing_failed_route,
                            ProtocolError::Unreachable {
                                message: format!("route channel closed: {hop_name}"),
                            },
                        ))
                        .await;
                }
            }
        }
        None => {
            tracing::debug!(next_hop = %hop_name, "no route, sending routing error");
            remove_tracked_outbound_for_routing_error(user_state, &missing_failed_route, &call_id)
                .await;
            if let Some((reply_src, reply_dst)) = missing_reply {
                let _ = tx
                    .send(Message::routing_error_for_route(
                        reply_src,
                        reply_dst,
                        call_id,
                        missing_failed_route,
                        ProtocolError::Unreachable {
                            message: format!("route not found: {hop_name}"),
                        },
                    ))
                    .await;
            }
        }
    }

    FrameForwardingResult::Forwarded
}

async fn remove_tracked_outbound_for_routing_error(
    user_state: &Arc<RwLock<ServerUserState>>,
    failed_route: &Route,
    call_id: &CallId,
) {
    user_state
        .read()
        .await
        .remove_tracked_outbound_for_call(call_id, failed_route);
}

fn is_terminal_response_body(body: &FrameBody) -> bool {
    matches!(body, FrameBody::Response(_))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

    use super::*;
    use crate::protocol::message::ResponseFrame;
    use crate::protocol::method;
    use crate::rpc::OutboundCallState;
    use crate::server::state::{ConnectionEntry, ConnectionKind};
    use crate::server::{
        ConnectionHandle, LocalOriginOutboundStart, PeerRoutingOutboundStart, ServerUserState,
    };

    fn insert_connection(us: &mut ServerUserState, link: Link, tx: mpsc::Sender<Message>) {
        us.connections.insert(
            link,
            ConnectionEntry::new(ConnectionHandle::new(tx), ConnectionKind::LocalClient),
        );
    }

    fn register_local_origin(
        us: &mut ServerUserState,
        call_id: CallId,
        owner_link: Link,
        request_dst: Route,
    ) {
        us.ensure_route_rpc(request_dst.clone())
            .register_local_origin_outbound(LocalOriginOutboundStart {
                call_id,
                method: method::AGENT_CREATE,
                state: OutboundCallState::AwaitingResponse,
                owner_link: owner_link.clone(),
                request_src: Route::from_link(owner_link),
                request_dst,
            })
            .unwrap();
    }

    #[tokio::test]
    async fn terminal_response_to_local_link_removes_tracked_local_origin_call() {
        let local = Link::new("local").unwrap();
        let peer = Link::new("peer").unwrap();
        let call_id = CallId::from(Uuid::new_v4());
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let (route_tx, mut route_rx) = mpsc::channel(8);

        {
            let mut us = user_state.write().await;
            insert_connection(&mut us, local.clone(), route_tx);
            register_local_origin(
                &mut us,
                call_id.clone(),
                local.clone(),
                Route::from_link(peer.clone()),
            );
        }

        let body = FrameBody::Response(ResponseFrame::Error(ProtocolError::Cancelled {
            message: "closed".to_string(),
        }));
        let (tx, _rx) = mpsc::channel(1);

        assert!(matches!(
            forward_frame_or_endpoint(
                &tx,
                &user_state,
                Route::from_link(peer),
                Route::from_link(local),
                call_id.clone(),
                body,
            )
            .await,
            FrameForwardingResult::Forwarded
        ));

        assert!(matches!(route_rx.try_recv(), Ok(Message::Frame(_))));
        assert_eq!(user_state.read().await.total_outbound_len(), 0);
    }

    #[tokio::test]
    async fn terminal_response_to_local_link_does_not_remove_local_origin_call_for_other_route() {
        let local = Link::new("local").unwrap();
        let intended_peer = Link::new("intended-peer").unwrap();
        let unrelated_peer = Link::new("unrelated-peer").unwrap();
        let call_id = CallId::from(Uuid::new_v4());
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let (route_tx, mut route_rx) = mpsc::channel(8);

        {
            let mut us = user_state.write().await;
            insert_connection(&mut us, local.clone(), route_tx);
            register_local_origin(
                &mut us,
                call_id.clone(),
                local.clone(),
                Route::from_link(intended_peer.clone()),
            );
        }

        let body = FrameBody::Response(ResponseFrame::Error(ProtocolError::Cancelled {
            message: "closed".to_string(),
        }));
        let (tx, _rx) = mpsc::channel(1);

        assert!(matches!(
            forward_frame_or_endpoint(
                &tx,
                &user_state,
                Route::from_link(unrelated_peer),
                Route::from_link(local),
                call_id.clone(),
                body,
            )
            .await,
            FrameForwardingResult::Forwarded
        ));

        assert!(matches!(route_rx.try_recv(), Ok(Message::Frame(_))));
        assert!(
            user_state
                .read()
                .await
                .route_rpc(&Route::from_link(intended_peer))
                .unwrap()
                .outbound_for_call(&call_id)
                .is_some()
        );
    }

    #[tokio::test]
    async fn origin_generated_routing_error_removes_tracked_local_origin_call() {
        let local = Link::new("local").unwrap();
        let peer = Link::new("peer").unwrap();
        let call_id = CallId::from(Uuid::new_v4());
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        {
            let mut us = user_state.write().await;
            register_local_origin(
                &mut us,
                call_id.clone(),
                local.clone(),
                Route::from_link(peer.clone()),
            );
        }
        let (tx, mut rx) = mpsc::channel(1);

        assert!(matches!(
            forward_frame_or_endpoint(
                &tx,
                &user_state,
                Route::from_link(local.clone()),
                Route::from_link(peer.clone()),
                call_id.clone(),
                FrameBody::StreamItem(b"payload".to_vec()),
            )
            .await,
            FrameForwardingResult::Forwarded
        ));

        let Message::Frame(Frame {
            call_id: response_call_id,
            body:
                FrameBody::RoutingError {
                    failed_route,
                    error: ProtocolError::Unreachable { .. },
                },
            ..
        }) = rx.recv().await.unwrap()
        else {
            panic!("expected routing error");
        };
        assert_eq!(response_call_id, call_id);
        assert_eq!(
            failed_route,
            Route::from_links([local.as_str().to_string(), peer.as_str().to_string()]).unwrap()
        );
        assert_eq!(user_state.read().await.total_outbound_len(), 0);
    }

    #[tokio::test]
    async fn origin_generated_routing_error_does_not_remove_non_local_origin_outbound_call() {
        let local = Link::new("local").unwrap();
        let missing_peer = Link::new("missing-peer").unwrap();
        let subscription_peer = Link::new("subscription-peer").unwrap();
        let call_id = CallId::from(Uuid::new_v4());
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        {
            let mut us = user_state.write().await;
            us.ensure_route_rpc(Route::empty())
                .register_peer_routing_outbound(PeerRoutingOutboundStart {
                    link: subscription_peer,
                    call_id: call_id.clone(),
                    method: method::ROUTING_SUBSCRIBE_EVENTS,
                })
                .unwrap();
        }
        let (tx, mut rx) = mpsc::channel(1);

        assert!(matches!(
            forward_frame_or_endpoint(
                &tx,
                &user_state,
                Route::from_link(local),
                Route::from_link(missing_peer),
                call_id.clone(),
                FrameBody::StreamItem(b"payload".to_vec()),
            )
            .await,
            FrameForwardingResult::Forwarded
        ));

        assert!(matches!(
            rx.recv().await,
            Some(Message::Frame(Frame {
                body: FrameBody::RoutingError { .. },
                ..
            }))
        ));
        assert!(
            user_state
                .read()
                .await
                .test_rpc()
                .outbound_for_call(&call_id)
                .is_some()
        );
    }

    #[tokio::test]
    async fn origin_generated_routing_error_does_not_remove_local_origin_call_for_other_route() {
        let local = Link::new("local").unwrap();
        let intended_peer = Link::new("intended-peer").unwrap();
        let unrelated_source = Link::new("unrelated-source").unwrap();
        let missing_peer = Link::new("missing-peer").unwrap();
        let call_id = CallId::from(Uuid::new_v4());
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        {
            let mut us = user_state.write().await;
            register_local_origin(
                &mut us,
                call_id.clone(),
                local.clone(),
                Route::from_link(intended_peer.clone()),
            );
        }
        let (tx, mut rx) = mpsc::channel(1);

        assert!(matches!(
            forward_frame_or_endpoint(
                &tx,
                &user_state,
                Route::from_link(unrelated_source),
                Route::from_link(missing_peer),
                call_id.clone(),
                FrameBody::StreamItem(b"payload".to_vec()),
            )
            .await,
            FrameForwardingResult::Forwarded
        ));

        assert!(matches!(
            rx.recv().await,
            Some(Message::Frame(Frame {
                body: FrameBody::RoutingError { .. },
                ..
            }))
        ));
        assert!(
            user_state
                .read()
                .await
                .route_rpc(&Route::from_link(intended_peer))
                .unwrap()
                .outbound_for_call(&call_id)
                .is_some()
        );
    }
}

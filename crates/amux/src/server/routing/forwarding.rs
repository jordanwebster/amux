use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};

use crate::protocol::message::{
    FrameBody, Message, ProtocolError, RoutedCallId, RoutedFrame, RoutedFrameMessage,
};
use crate::protocol::{Link, Route};
use crate::server::ServerUserState;

pub(in crate::server) enum ForwardedRoutedPayload {
    Forwarded,
    Endpoint {
        src: Route,
        call_id: RoutedCallId,
        payload: Vec<u8>,
    },
}

fn routed_payload_message(
    src: Route,
    dst: Route,
    call_id: RoutedCallId,
    payload: Vec<u8>,
) -> Message {
    Message::Routed(RoutedFrame {
        src,
        dst,
        call_id,
        message: RoutedFrameMessage::Payload(payload),
    })
}

fn failed_route_from_parts(src: &Route, next_hop: &Link, remaining_dst: &Route) -> Route {
    let mut links: Vec<String> = src.iter().map(|link| link.as_str().to_string()).collect();
    links.reverse();
    links.push(next_hop.as_str().to_string());
    links.extend(remaining_dst.iter().map(|link| link.as_str().to_string()));
    Route::from_links(links).expect("failed route is composed from already-validated links")
}

/// Forward a routed payload if `dst` still has a next hop.
///
/// Relays stay stateless: this function rewrites route hops and forwards the
/// opaque payload bytes, or returns/synthesizes a routing-layer error when the
/// next hop is unreachable. Endpoint service payloads are not decoded here.
pub(in crate::server) async fn forward_routed_payload_or_endpoint(
    tx: &mpsc::Sender<Message>,
    user_state: &Arc<RwLock<ServerUserState>>,
    mut src: Route,
    mut dst: Route,
    call_id: RoutedCallId,
    payload: Vec<u8>,
) -> ForwardedRoutedPayload {
    let Some(next_hop) = dst.pop() else {
        return ForwardedRoutedPayload::Endpoint {
            src,
            call_id,
            payload,
        };
    };

    let hop_name = next_hop.clone();
    let missing_failed_route = failed_route_from_parts(&src, &hop_name, &dst);
    let missing_reply = Route::reply(src.clone());
    src.push(next_hop);

    let route_tx = {
        let mut us = user_state.write().await;
        let route_tx = us.topology.route(&hop_name);
        if route_tx.is_some()
            && !us.topology.peer_links.contains(&hop_name)
            && is_terminal_response_payload(&payload)
        {
            us.rpc.remove_outbound_for_route_if(&src, &call_id, |call| {
                call.resources
                    .as_ref()
                    .and_then(|resources| resources.local_origin())
                    .is_some_and(|(owner_link, _, _)| *owner_link == hop_name)
            });
        }
        route_tx
    };

    match route_tx {
        Some(route_tx) => {
            if route_tx
                .send(routed_payload_message(src, dst, call_id.clone(), payload))
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

    ForwardedRoutedPayload::Forwarded
}

async fn remove_tracked_outbound_for_routing_error(
    user_state: &Arc<RwLock<ServerUserState>>,
    failed_route: &Route,
    call_id: &RoutedCallId,
) {
    let mut us = user_state.write().await;
    us.rpc
        .remove_outbound_for_route_if(failed_route, call_id, |call| call.resources.is_some());
}

fn is_terminal_response_payload(payload: &[u8]) -> bool {
    matches!(
        crate::protocol::wire::decode_frame_body(payload),
        Ok(FrameBody::Response(_))
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::{RwLock, mpsc};
    use uuid::Uuid;

    use super::*;
    use crate::protocol::message::ResponseFrame;
    use crate::protocol::method;
    use crate::rpc::{OutboundCallState, RpcLocalOriginOutboundStart};
    use crate::server::{ConnectionHandle, ServerUserState};

    #[tokio::test]
    async fn terminal_response_to_local_link_removes_tracked_local_origin_call() {
        let local = Link::new("local").unwrap();
        let peer = Link::new("peer").unwrap();
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let counterparty_route =
            Route::from_links([local.as_str().to_string(), peer.as_str().to_string()]).unwrap();
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        let (route_tx, mut route_rx) = mpsc::channel(8);

        {
            let mut us = user_state.write().await;
            us.topology
                .routes
                .insert(local.clone(), ConnectionHandle::new(route_tx));
            us.rpc
                .register_local_origin_outbound(RpcLocalOriginOutboundStart {
                    call_id: call_id.clone(),
                    counterparty_route: counterparty_route.clone(),
                    method: method::AGENT_CREATE,
                    state: OutboundCallState::AwaitingResponse,
                    owner_link: local.clone(),
                    request_src: Route::from_link(local.clone()),
                    request_dst: Route::from_link(peer.clone()),
                })
                .unwrap();
        }

        let payload = crate::protocol::wire::encode_frame_body(&FrameBody::Response(
            ResponseFrame::Error(ProtocolError::Cancelled {
                message: "closed".to_string(),
            }),
        ))
        .unwrap();
        let (tx, _rx) = mpsc::channel(1);

        assert!(matches!(
            forward_routed_payload_or_endpoint(
                &tx,
                &user_state,
                Route::from_link(peer),
                Route::from_link(local),
                call_id.clone(),
                payload,
            )
            .await,
            ForwardedRoutedPayload::Forwarded
        ));

        assert!(matches!(route_rx.try_recv(), Ok(Message::Routed(_))));
        assert_eq!(user_state.read().await.rpc.outbound_len(), 0);
    }

    #[tokio::test]
    async fn origin_generated_routing_error_removes_tracked_local_origin_call() {
        let local = Link::new("local").unwrap();
        let peer = Link::new("peer").unwrap();
        let call_id = RoutedCallId::from(Uuid::new_v4());
        let counterparty_route =
            Route::from_links([local.as_str().to_string(), peer.as_str().to_string()]).unwrap();
        let user_state = Arc::new(RwLock::new(ServerUserState::new()));
        {
            let mut us = user_state.write().await;
            us.rpc
                .register_local_origin_outbound(RpcLocalOriginOutboundStart {
                    call_id: call_id.clone(),
                    counterparty_route: counterparty_route.clone(),
                    method: method::AGENT_CREATE,
                    state: OutboundCallState::AwaitingResponse,
                    owner_link: local.clone(),
                    request_src: Route::from_link(local.clone()),
                    request_dst: Route::from_link(peer.clone()),
                })
                .unwrap();
        }
        let (tx, mut rx) = mpsc::channel(1);

        assert!(matches!(
            forward_routed_payload_or_endpoint(
                &tx,
                &user_state,
                Route::from_link(local.clone()),
                Route::from_link(peer.clone()),
                call_id.clone(),
                b"payload".to_vec(),
            )
            .await,
            ForwardedRoutedPayload::Forwarded
        ));

        let Message::Routed(RoutedFrame {
            call_id: response_call_id,
            message:
                RoutedFrameMessage::RoutingError {
                    failed_route,
                    error: ProtocolError::Unreachable { .. },
                },
            ..
        }) = rx.recv().await.unwrap()
        else {
            panic!("expected routing error");
        };
        assert_eq!(response_call_id, call_id);
        assert_eq!(failed_route, counterparty_route);
        assert_eq!(user_state.read().await.rpc.outbound_len(), 0);
    }
}

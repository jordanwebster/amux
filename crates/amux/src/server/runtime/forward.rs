use std::sync::Arc;

use tokio::sync::RwLock;

use crate::protocol::message::{Message, RoutableMessage};
use crate::protocol::route::Route;
use crate::server::ServerUserState;

pub(in crate::server) async fn send_routable_via_full_dst(
    user_state: &Arc<RwLock<ServerUserState>>,
    full_dst: &Route,
    message: &RoutableMessage,
) -> bool {
    let Some((src, dst)) = Route::send(full_dst.clone()) else {
        return false;
    };
    let Some(next_hop) = src.peek() else {
        return false;
    };

    let route_handle = {
        let us = user_state.read().await;
        us.routes.get(next_hop).cloned()
    };

    let Some(route_handle) = route_handle else {
        return false;
    };

    let request_id = route_handle.next_request_id();
    route_handle
        .send(Message::routable(src, dst, request_id, message))
        .await
        .is_ok()
}

pub(in crate::server) async fn try_send_routable_via_full_dst(
    user_state: &Arc<RwLock<ServerUserState>>,
    full_dst: &Route,
    message: &RoutableMessage,
) -> bool {
    let Some((src, dst)) = Route::send(full_dst.clone()) else {
        return false;
    };
    let Some(next_hop) = src.peek() else {
        return false;
    };

    let route_handle = {
        let us = user_state.read().await;
        us.routes.get(next_hop).cloned()
    };

    let Some(route_handle) = route_handle else {
        return false;
    };

    let request_id = route_handle.next_request_id();
    route_handle.try_send(Message::routable(src, dst, request_id, message))
}

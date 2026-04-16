//! Agent lifecycle, peer management, and route-level operations.

mod agents;
mod naming;
mod peers;

use super::{ConnectionHandle, ServerUserState};
use crate::protocol::message::Message;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

pub(super) use agents::{
    SubscribeError, create_agent, delete_local_agent, handle_subscribe, resume_agents,
    shutdown_server, suspend_server, withdraw_agent,
};
pub(super) use naming::{apply_local_name_candidate, rename_local_agent};
pub(super) use peers::{
    announce_agent_message, broadcast_to_peers, handle_peer_disconnect, send_initial_announcements,
};

pub(super) async fn connection_tx(
    user_state: &Arc<RwLock<ServerUserState>>,
    link_name: &str,
) -> Option<mpsc::Sender<Message>> {
    let us = user_state.read().await;
    us.routes.get(link_name).map(ConnectionHandle::sender)
}

#[cfg(test)]
mod tests;

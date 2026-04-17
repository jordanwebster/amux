use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use super::*;
use crate::agent::claude::hooks::{ClaudeHook, ParsedClaudeHook};
use crate::agent::{Agent, AgentSession, LocalAgentNameSource, SessionEvent};
use crate::protocol::RoutableMessage;
use crate::protocol::link::Link;
use crate::protocol::message::{
    AgentType, Command, CreateAgentRequest, DirectMessage, HookProvider, Message, TerminalSize,
};
pub(super) use crate::server::test_helpers::{test_ctx, test_state};
use crate::server::{ConnectionHandle, LOCAL_USER_ID, ServerUserState};

pub(super) fn dummy_pty_command() -> String {
    #[cfg(unix)]
    {
        "/bin/cat".to_string()
    }
    #[cfg(windows)]
    {
        "cmd.exe".to_string()
    }
}

pub(super) fn dummy_working_dir() -> PathBuf {
    std::env::temp_dir()
}

pub(super) fn claude_agent_type() -> String {
    "claude".to_string()
}

pub(super) fn claude_structured_protocol() -> Option<String> {
    Some("claude_pty_v1".to_string())
}

pub(super) fn claude_hook_command(agent_id: Uuid, hook: ClaudeHook, external: bool) -> Command {
    let parsed = ParsedClaudeHook::from_typed(hook);
    Command::HandleHook {
        agent_id,
        provider: HookProvider::Claude,
        payload: serde_json::to_vec(&parsed.raw).unwrap(),
        external,
    }
}

pub(super) fn create_test_session(req: &CreateAgentRequest) -> AgentSession {
    let cmd = match &req.agent_type {
        AgentType::TestAgent { command } => command.clone(),
        _ => panic!("expected TestAgent"),
    };
    let mut inner = crate::agent::TestAgentSession::new(req, cmd);
    inner.start().unwrap();
    AgentSession::TestAgent(inner)
}

pub(super) fn mock_tx() -> (mpsc::Sender<Message>, Arc<tokio::sync::Mutex<Vec<Message>>>) {
    let (tx, mut rx) = mpsc::channel::<Message>(16);
    let written = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let written_clone = written.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            written_clone.lock().await.push(msg);
        }
    });
    (tx, written)
}

pub(super) async fn add_peer_link(
    user_state: &Arc<RwLock<ServerUserState>>,
    link: &str,
) -> mpsc::Receiver<Message> {
    let (tx, rx) = mpsc::channel::<Message>(16);
    let mut us = user_state.write().await;
    us.routes.insert(
        Link::new(link).unwrap(),
        ConnectionHandle::new(tx, Arc::new(AtomicU64::new(1))),
    );
    us.peer_links.insert(Link::new(link).unwrap());
    rx
}

pub(super) async fn insert_remote_agent(user_state: &Arc<RwLock<ServerUserState>>, info: Agent) {
    let mut us = user_state.write().await;
    us.hosts
        .entry(info.host_id)
        .or_insert_with(|| crate::protocol::message::Host {
            id: info.host_id,
            name: format!("host-{}", info.host_id),
            route: info.route.clone(),
            version: "0.1.0".to_string(),
        });
    us.registry.register_remote(info).unwrap();
}

pub(super) async fn insert_local_claude(
    user_state: &Arc<RwLock<ServerUserState>>,
    agent_id: Uuid,
    name: Option<&str>,
    source: LocalAgentNameSource,
) {
    let req = CreateAgentRequest {
        agent_id,
        name: name.map(str::to_owned),
        agent_type: AgentType::Claude,
        working_dir: PathBuf::from("/tmp"),
        terminal_size: None,
        args: vec![],
    };
    let mut session = AgentSession::try_new(&req).expect("known agent type");
    session.set_local_name(name.map(str::to_owned), source);
    let info = session.to_agent(Uuid::new_v4());

    let mut us = user_state.write().await;
    us.agents.insert(agent_id, session);
    us.registry.register_local(info).unwrap();
}

pub(super) fn decode_written_routable(msg: &Message) -> RoutableMessage {
    let Message::Routable { payload, .. } = msg else {
        panic!("expected Routable, got {:?}", msg);
    };
    RoutableMessage::decode(payload).unwrap()
}

pub(super) fn drain_direct_messages(rx: &mut mpsc::Receiver<Message>) -> Vec<DirectMessage> {
    let mut messages = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        let Message::Direct { message } = msg else {
            panic!("expected Direct message, got {:?}", msg);
        };
        messages.push(message);
    }
    messages
}

pub(super) async fn insert_test_session(user_state: &Arc<RwLock<ServerUserState>>, agent_id: Uuid) {
    let mut us = user_state.write().await;
    let req = CreateAgentRequest {
        agent_id,
        name: Some("hook-test".to_string()),
        agent_type: AgentType::TestAgent {
            command: dummy_pty_command(),
        },
        working_dir: dummy_working_dir(),
        terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
        args: vec![],
    };
    let session = create_test_session(&req);
    us.agents.insert(agent_id, session);
}

pub(super) async fn setup_named_route(
    user_state: &Arc<RwLock<ServerUserState>>,
    link: &str,
) -> mpsc::Receiver<Message> {
    let (route_tx, route_rx) = mpsc::channel::<Message>(64);
    let mut us = user_state.write().await;
    us.routes.insert(
        Link::new(link).unwrap(),
        ConnectionHandle::new(route_tx, Arc::new(AtomicU64::new(1))),
    );
    route_rx
}

pub(super) async fn setup_route(
    user_state: &Arc<RwLock<ServerUserState>>,
) -> mpsc::Receiver<Message> {
    setup_named_route(user_state, "test-link").await
}

pub(super) async fn recv_routable(rx: &mut mpsc::Receiver<Message>) -> RoutableMessage {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for stream message")
        .expect("route channel closed");
    let Message::Routable { payload, .. } = msg else {
        panic!("expected Routable, got {:?}", msg);
    };
    RoutableMessage::decode(&payload).unwrap()
}

#[tokio::test]
async fn command_from_remote_peer_is_rejected() {
    let (state, user_state) = test_state().await;
    let (event_tx, _event_rx) = mpsc::channel::<SessionEvent>(16);
    let ctx = ConnectionContext {
        state,
        user_state,
        user_id: LOCAL_USER_ID,
        event_tx,
        link: Link::new("remote-peer").unwrap(),
        is_local: false,
        heartbeat: Some(crate::server::connection::HeartbeatSetup {
            role: crate::server::connection::HeartbeatRole::Acceptor,
            idle_timeout: std::time::Duration::from_secs(180),
        }),
        next_request_id: Arc::new(AtomicU64::new(1)),
        client_name: Some("amux-cli".to_string()),
        client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    let (tx, written) = mock_tx();

    handle_message(
        &tx,
        Message::Command {
            command: Command::Shutdown,
        },
        &ctx,
    )
    .await
    .unwrap();
    handle_message(
        &tx,
        Message::Command {
            command: Command::ListAgents,
        },
        &ctx,
    )
    .await
    .unwrap();

    tokio::task::yield_now().await;

    let msgs = written.lock().await;
    assert!(
        msgs.is_empty(),
        "remote peer should receive no response to commands"
    );
}

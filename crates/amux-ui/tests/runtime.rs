use std::time::Duration;

use amux::{AgentType, Config, CreateAgentRequest, Server, TerminalSize};
use amux_ui::{Cmd, CmdResult, Notification, Runtime, SessionPhase};
use bytes::Bytes;
use futures_util::StreamExt;
use tempfile::tempdir;
use uuid::Uuid;

// Ignored: the runtime's per-host agent-event subscriptions require knowing
// the local server's host_id, which is not currently exposed to local
// clients in the handshake. The push-based inventory in this crate is
// structurally complete, but local-agent inventory is unreachable until
// that piece lands as part of a forthcoming amux-ui redesign.
#[tokio::test]
#[ignore]
async fn runtime_drives_embedded_agent_session() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        enable_cloud_mode: Some(false),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };
    let client = Server::builder()
        .config(config)
        .embedded()
        .open()
        .await
        .unwrap();
    let external_client = client.clone();
    let runtime = Runtime::start_with_client(client);
    let mut notifications = runtime.notifications();

    wait_for(&mut notifications, |notification| {
        matches!(notification, Notification::Connected)
    })
    .await;

    let external_agent_id = Uuid::new_v4();
    external_client
        .create_agent(CreateAgentRequest {
            agent_id: external_agent_id,
            name: Some("external-ui-test".to_string()),
            agent_type: AgentType::TestAgent {
                command: "cat".to_string(),
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
        })
        .await
        .unwrap();
    wait_for(&mut notifications, |notification| {
        matches!(notification, Notification::AgentAdded(agent) if agent.id == external_agent_id)
    })
    .await;

    external_client
        .rename_agent(external_agent_id, "external-ui-renamed".to_string())
        .await
        .unwrap();
    wait_for(&mut notifications, |notification| {
        matches!(
            notification,
            Notification::AgentUpdated(agent)
                if agent.id == external_agent_id
                    && agent.name.as_deref() == Some("external-ui-renamed")
        )
    })
    .await;

    external_client
        .delete_agent(external_agent_id)
        .await
        .unwrap();
    wait_for(&mut notifications, |notification| {
        matches!(notification, Notification::AgentRemoved { id, .. } if *id == external_agent_id)
    })
    .await;

    let agent_id = Uuid::new_v4();
    let create_id = runtime.dispatch(Cmd::CreateAgent(CreateAgentRequest {
        agent_id,
        name: Some("ui-test".to_string()),
        agent_type: AgentType::TestAgent {
            command: "cat".to_string(),
        },
        working_dir: std::env::temp_dir(),
        terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
        args: Vec::new(),
    }));

    wait_for(&mut notifications, |notification| {
        matches!(notification, Notification::AgentAdded(agent) if agent.id == agent_id)
    })
    .await;
    wait_for(&mut notifications, |notification| {
        matches!(
            notification,
            Notification::CommandCompleted {
                id,
                result: CmdResult::CreateAgent(_)
            } if *id == create_id
        )
    })
    .await;

    let attach_id = runtime.dispatch(Cmd::AttachSession {
        id: agent_id,
        io_protocol: amux::agent::claude::io::RAW_V1.to_string(),
        args: None,
    });
    wait_for(
        &mut notifications,
        |notification| matches!(notification, Notification::SessionOpened(id) if *id == agent_id),
    )
    .await;
    wait_for(&mut notifications, |notification| {
        matches!(
            notification,
            Notification::CommandCompleted {
                id,
                result: CmdResult::AttachSession
            } if *id == attach_id
        )
    })
    .await;

    let send_id = runtime.dispatch(Cmd::SendInput {
        id: agent_id,
        io_protocol: amux::agent::claude::io::RAW_V1.to_string(),
        payload: Bytes::from_static(b"hello from ui\n"),
    });
    wait_for(&mut notifications, |notification| {
        matches!(
            notification,
            Notification::CommandCompleted {
                id,
                result: CmdResult::SendInput
            } if *id == send_id
        )
    })
    .await;
    wait_for(&mut notifications, |notification| {
        matches!(
            notification,
            Notification::SessionOutput {
                id,
                phase: SessionPhase::Live,
                ..
            } if *id == agent_id
        )
    })
    .await;

    let delete_id = runtime.dispatch(Cmd::DeleteAgent(agent_id));
    wait_for(&mut notifications, |notification| {
        matches!(notification, Notification::AgentRemoved { id, .. } if *id == agent_id)
    })
    .await;
    wait_for(&mut notifications, |notification| {
        matches!(
            notification,
            Notification::CommandCompleted {
                id,
                result: CmdResult::DeleteAgent
            } if *id == delete_id
        )
    })
    .await;

    runtime.shutdown().await;
}

async fn wait_for(
    notifications: &mut amux_ui::NotificationStream,
    mut predicate: impl FnMut(&Notification) -> bool,
) -> Notification {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(notification) = notifications.next().await {
            if predicate(&notification) {
                return notification;
            }
        }
        panic!("notification stream closed before expected notification");
    })
    .await
    .expect("timed out waiting for notification")
}

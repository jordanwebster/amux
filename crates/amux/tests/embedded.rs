use std::sync::Arc;
use std::time::Duration;

use amux::{
    AccessToken, AgentType, AuthError, Config, CreateAgentRequest, CredentialProvider, Server,
    ShutdownReason, UpdateReporter, UpdateStatus,
};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

struct TestCredentials;

#[async_trait::async_trait]
impl CredentialProvider for TestCredentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        Err(AuthError::Unauthenticated)
    }

    fn invalidate(&self, _token: &AccessToken) {}
}

struct CapturingUpdateReporter {
    tx: tokio::sync::mpsc::UnboundedSender<UpdateStatus>,
}

impl UpdateReporter for CapturingUpdateReporter {
    fn report(&self, status: UpdateStatus) {
        let _ = self.tx.send(status);
    }
}

#[tokio::test]
async fn embedded_server_opens_client_over_memory_transport() {
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

    let agents = client.list_agents().await.unwrap();
    assert!(agents.is_empty());
}

#[tokio::test]
async fn embedded_server_runs_update_checker_when_reporter_is_configured() {
    let dir = tempdir().unwrap();
    let (cloud_url, manifest_task) = spawn_manifest_server("999.0.0").await;
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        cloud_url,
        enable_cloud_mode: Some(false),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let client = Server::builder()
        .config(config)
        .update_reporter(Arc::new(CapturingUpdateReporter { tx }))
        .embedded()
        .open()
        .await
        .unwrap();

    let status = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for embedded update status")
        .expect("embedded update reporter channel closed");
    match status {
        UpdateStatus::Available(Some(info)) => {
            assert_eq!(info.update_version, "999.0.0");
        }
        other => panic!("unexpected update status: {other:?}"),
    }

    drop(client);
    manifest_task.await.unwrap();
}

#[tokio::test]
async fn embedded_shutdown_stops_agents_and_closes_server_tasks() {
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

    let agent_id = Uuid::new_v4();
    client
        .create_agent(CreateAgentRequest {
            agent_id,
            name: Some("shutdown-test".to_string()),
            agent_type: AgentType::TestAgent {
                command: "cat".to_string(),
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
        })
        .await
        .unwrap();

    client
        .shutdown(ShutdownReason::UserRequested)
        .await
        .unwrap();
    expect_client_closed(&client).await;
}

#[tokio::test]
async fn embedded_suspend_stops_agents_and_closes_server_tasks() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.yaml");
    let config = Config {
        state_path: state_path.clone(),
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

    let agent_id = Uuid::new_v4();
    client
        .create_agent(CreateAgentRequest {
            agent_id,
            name: Some("suspend-test".to_string()),
            agent_type: AgentType::TestAgent {
                command: "cat".to_string(),
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
        })
        .await
        .unwrap();

    let summary = client.suspend().await.unwrap();
    assert_eq!(summary.suspended_count, 1);
    expect_client_closed(&client).await;
    let suspended_path = state_path.with_file_name("suspended.yaml");
    assert!(
        std::fs::read_to_string(suspended_path)
            .unwrap_or_default()
            .contains("suspend-test")
    );
}

async fn expect_client_closed(client: &amux::Client) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if client.list_agents().await.is_err() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("embedded client remained usable after lifecycle shutdown");
}

async fn spawn_manifest_server(version: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await;
        let body = format!(r#"{{"version":"{version}"}}"#);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    (format!("http://{addr}"), task)
}

#[tokio::test]
async fn embedded_server_requires_credentials_when_cloud_enabled() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        enable_cloud_mode: Some(true),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };

    let result = Server::builder().config(config).embedded().open().await;
    let Err(error) = result else {
        panic!("embedded cloud-enabled server opened without credentials");
    };

    assert!(
        error
            .to_string()
            .contains("credentials provider is required")
    );
}

#[tokio::test]
async fn server_builder_rejects_credentials_for_cloud_relay() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };

    let result = Server::builder()
        .config(config)
        .as_cloud_relay()
        .credentials(Arc::new(TestCredentials))
        .embedded()
        .open()
        .await;
    let Err(error) = result else {
        panic!("server builder accepted credentials for a cloud relay");
    };

    assert!(
        error
            .to_string()
            .contains("credentials() and as_cloud_relay() are mutually exclusive")
    );
}

#[tokio::test]
async fn server_builder_rejects_credentials_before_cloud_relay() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };

    let result = Server::builder()
        .config(config)
        .credentials(Arc::new(TestCredentials))
        .as_cloud_relay()
        .embedded()
        .open()
        .await;
    let Err(error) = result else {
        panic!("server builder accepted credentials before as_cloud_relay");
    };

    assert!(
        error
            .to_string()
            .contains("credentials() and as_cloud_relay() are mutually exclusive")
    );
}

#[tokio::test]
async fn daemon_open_does_not_require_credentials_when_cloud_enabled() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("missing.sock"),
        enable_cloud_mode: Some(true),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };

    let result = Server::builder().config(config).daemon().open().await;
    let Err(error) = result else {
        panic!("daemon unexpectedly opened without a listener");
    };

    assert!(
        !error
            .to_string()
            .contains("credentials provider is required"),
        "daemon client attach should not validate server credential providers"
    );
}

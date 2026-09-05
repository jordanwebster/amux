use std::sync::Arc;
use std::time::Duration;

use amux::{
    AccessToken, AuthError, Config, CredentialProvider, Server, UpdateReporter, UpdateStatus,
};
#[cfg(debug_assertions)]
use amux::{AgentType, CreateAgentRequest};
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
async fn embedded_server_opens_client_service_client() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),

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

#[cfg(unix)]
#[tokio::test]
async fn daemon_open_uses_local_client_service() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),

        prevent_idle_sleep: Some(false),
        ..Config::default()
    };
    let server_config = config.clone();
    let server_task = tokio::spawn(async move {
        Server::builder().config(server_config).run().await.unwrap();
    });

    let client = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match Server::builder()
                .config(config.clone())
                .daemon()
                .open()
                .await
            {
                Ok(client) => break client,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("timed out waiting for daemon client");

    assert!(!client.owns_embedded_server());
    assert!(client.list_agents().await.unwrap().is_empty());
    client.shutdown().await.unwrap();
    server_task.await.unwrap();
}

#[tokio::test]
async fn embedded_server_does_not_poll_for_updates() {
    // An embedded/mobile client must not poll for binary updates: it can't
    // self-update (the app store owns its binary). The active manifest poll is a
    // desktop-daemon concern. A too-old mobile client is instead told to update
    // over the cloud connection (`UpdateStatus::Required`), which its
    // `update_reporter` still receives. Here we reach a working manifest server
    // and assert the embedded client never hits it.
    let dir = tempdir().unwrap();
    let (cloud_url, manifest_task) = spawn_manifest_server("999.0.0").await;
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        cloud_url,

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

    // The update poll fires immediately when spawned, so a short window with no
    // status reliably proves the embedded client never spawned one.
    let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
    assert!(
        result.is_err(),
        "embedded client unexpectedly reported update status: {result:?}"
    );

    drop(client);
    manifest_task.abort();
}

#[tokio::test]
#[cfg(debug_assertions)]
#[cfg_attr(
    windows,
    ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
)]
async fn embedded_shutdown_stops_agents_and_closes_server_tasks() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),

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
            host_id: None,
            name: Some("shutdown-test".to_string()),
            agent_type: AgentType::TestAgent {
                command: "cat".to_string(),
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        })
        .await
        .unwrap();

    client.shutdown().await.unwrap();
    expect_client_closed(&client).await;
}

#[tokio::test]
#[cfg(debug_assertions)]
#[cfg_attr(
    windows,
    ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
)]
async fn embedded_suspend_stops_agents_and_closes_server_tasks() {
    let dir = tempdir().unwrap();
    let state_path = dir.path().join("state.yaml");
    let config = Config {
        state_path: state_path.clone(),
        socket_path: dir.path().join("amux.sock"),

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
            host_id: None,
            name: Some("suspend-test".to_string()),
            agent_type: AgentType::TestAgent {
                command: "cat".to_string(),
            },
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
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
async fn config_split_embedded_without_credentials_stays_local() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        data_dir: dir.path().to_owned(),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };
    let client = Server::builder()
        .config(config)
        .embedded()
        .open()
        .await
        .unwrap();
    assert!(client.list_agents().await.unwrap().is_empty());
}

#[tokio::test]
async fn config_split_relay_still_requires_a_tcp_listener() {
    let error = Server::builder()
        .config(Config::default())
        .as_cloud_relay()
        .run()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cloud relay requires tcp_port"));
}

#[tokio::test]
async fn embedded_open_rejects_cloud_relay() {
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
        .embedded()
        .open()
        .await;
    let Err(error) = result else {
        panic!("embedded cloud relay opened");
    };

    assert!(
        error
            .to_string()
            .contains("embedded cloud relays are not supported")
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
async fn daemon_open_does_not_require_credentials() {
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("missing.sock"),

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

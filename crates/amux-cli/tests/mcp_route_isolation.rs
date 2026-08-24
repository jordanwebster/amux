#![cfg(unix)]

use std::fs::Permissions;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use amux::{AgentType, Client, Config, CreateAgentRequest, Server};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use uuid::Uuid;

struct Route {
    root: PathBuf,
    executable: PathBuf,
    config_path: PathBuf,
    config: Config,
    client: Client,
    server: JoinHandle<()>,
    agent_id: Uuid,
    host_id: Uuid,
}

impl Route {
    async fn start(parent: &Path, name: &str) -> Self {
        let root = parent.join(name);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("socket")).unwrap();

        let executable = root.join("bin/amux");
        std::fs::copy(env!("CARGO_BIN_EXE_amux"), &executable).unwrap();
        let config_path = root.join("amux.yaml");
        let config = Config {
            host_name: name.to_string(),
            socket_path: root.join("socket/amux.sock"),
            state_path: root.join("state/state.yaml"),
            data_dir: root.join("data"),
            enable_cloud_mode: Some(false),
            prevent_idle_sleep: Some(false),
            path: Some(config_path.clone()),
            ..Config::default()
        };
        std::fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();

        let server_config = config.clone();
        let server = tokio::spawn(async move {
            Server::builder().config(server_config).run().await.unwrap();
        });
        let client = wait_for_client(&config).await;
        let host_id = client
            .list_hosts()
            .await
            .unwrap()
            .into_iter()
            .find(|host| host.name == name)
            .expect("daemon must publish its local host")
            .id;
        let agent_id = Uuid::new_v4();
        client
            .create_agent(CreateAgentRequest {
                agent_id,
                host_id: None,
                name: Some(format!("{name}-agent")),
                agent_type: AgentType::TestAgent {
                    command: "cat".to_string(),
                },
                working_dir: root.clone(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
            .unwrap();

        Self {
            root,
            executable,
            config_path,
            config,
            client,
            server,
            agent_id,
            host_id,
        }
    }

    fn call_status(&self, path: &Path, marker: &str) -> Output {
        let mut child = Command::new(&self.executable)
            .args([
                "--config",
                self.config_path.to_str().unwrap(),
                "mcp",
                "agent",
                "--socket-path",
                self.config.socket_path.to_str().unwrap(),
            ])
            .env("AMUX_AGENT_ID", self.agent_id.to_string())
            .env("AMUX_HOST_ID", self.host_id.to_string())
            .env("PATH", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"tools/call",
                "params":{"name":"status","arguments":{"working_on":marker}}
            }),
        ];
        {
            let stdin = child.stdin.as_mut().unwrap();
            for request in requests {
                writeln!(stdin, "{request}").unwrap();
            }
        }
        drop(child.stdin.take());
        child.wait_with_output().unwrap()
    }

    async fn shutdown(self) {
        self.client.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.server)
            .await
            .expect("daemon shutdown timed out")
            .unwrap();
    }
}

async fn wait_for_client(config: &Config) -> Client {
    tokio::time::timeout(Duration::from_secs(5), async {
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
    .expect("timed out waiting for daemon")
}

fn assert_successful_status_call(output: &Output) {
    assert!(
        output.status.success(),
        "MCP process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = String::from_utf8(output.stdout.clone()).unwrap();
    let responses = rows
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "amux");
    assert_eq!(responses[1]["result"]["isError"], false);
}

fn run_rejected_route(executable: &Path, config: &Path, socket: &Path) -> Output {
    Command::new(executable)
        .args([
            "--config",
            config.to_str().unwrap(),
            "mcp",
            "agent",
            "--socket-path",
            socket.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_absolute_mcp_routes_are_isolated_and_fail_closed() {
    let temporary = TempDir::new().unwrap();
    let poison = temporary.path().join("poison");
    std::fs::create_dir(&poison).unwrap();
    let poison_marker = temporary.path().join("ambient-amux-ran");
    let poison_executable = poison.join("amux");
    std::fs::write(
        &poison_executable,
        format!("#!/bin/sh\ntouch '{}'\nexit 99\n", poison_marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&poison_executable, Permissions::from_mode(0o755)).unwrap();

    let first = Route::start(temporary.path(), "first-checkout").await;
    let second = Route::start(temporary.path(), "second-checkout").await;

    let first_marker = "first route owns this status";
    let second_marker = "second route owns this status";
    assert_successful_status_call(&first.call_status(&poison, first_marker));
    assert_successful_status_call(&second.call_status(&poison, second_marker));

    let first_agents = first.client.list_agents().await.unwrap();
    let second_agents = second.client.list_agents().await.unwrap();
    assert_eq!(first_agents.len(), 1);
    assert_eq!(second_agents.len(), 1);
    assert_eq!(first_agents[0].id, first.agent_id);
    assert_eq!(second_agents[0].id, second.agent_id);
    assert_eq!(
        first_agents[0]
            .working_on
            .as_ref()
            .map(|value| value.text.as_str()),
        Some(first_marker)
    );
    assert_eq!(
        second_agents[0]
            .working_on
            .as_ref()
            .map(|value| value.text.as_str()),
        Some(second_marker)
    );
    assert!(!first_agents.iter().any(|agent| agent.id == second.agent_id));
    assert!(!second_agents.iter().any(|agent| agent.id == first.agent_id));
    assert!(
        !poison_marker.exists(),
        "an ambient PATH entry redirected an absolute managed route"
    );

    let crossed = run_rejected_route(
        &first.executable,
        &first.config_path,
        &second.config.socket_path,
    );
    assert!(!crossed.status.success());
    assert!(
        String::from_utf8_lossy(&crossed.stderr).contains("does not match"),
        "crossed config/socket route was not rejected: {}",
        String::from_utf8_lossy(&crossed.stderr)
    );

    let missing_config = first.root.join("missing.yaml");
    let missing = run_rejected_route(
        &first.executable,
        &missing_config,
        &first.config.socket_path,
    );
    assert!(!missing.status.success());

    let missing_executable = first.root.join("bin/missing-amux");
    assert!(Command::new(&missing_executable).status().is_err());
    let unlaunchable = first.root.join("bin/unlaunchable-amux");
    std::fs::write(&unlaunchable, b"not executable").unwrap();
    std::fs::set_permissions(&unlaunchable, Permissions::from_mode(0o644)).unwrap();
    assert!(Command::new(&unlaunchable).status().is_err());

    first.shutdown().await;
    second.shutdown().await;
}

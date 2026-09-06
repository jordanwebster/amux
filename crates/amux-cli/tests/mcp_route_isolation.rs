#![cfg(unix)]

use std::fs::Permissions;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use amux::installation::{
    InstallationRoot, OperationId, ProfileId, ProfileLabel, ProfilePaths, Registry,
};
use amux::{
    AgentType, Client, Config, CreateAgentRequest, InstallationConfig, ProfileConfig, Server,
};
use serde_json::{Value, json};
use uuid::Uuid;

struct Route {
    root: PathBuf,
    executable: PathBuf,
    config_path: PathBuf,
    config: Config,
    client: Client,
    sibling_client: Client,
    sibling_config: PathBuf,
    sibling_id: ProfileId,
    installation: Arc<amux::installation::Installation>,
    front_door: amux::installation::FrontDoorListener,
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
        let root = root.canonicalize().unwrap();
        let id = ProfileId::new();
        let mut registry = Registry::open(InstallationRoot::OnDisk(root.clone())).unwrap();
        registry.create(id, ProfileLabel::default()).unwrap();
        drop(registry);
        let paths = ProfilePaths::for_id(&root, id).unwrap();
        let config_path = paths.config_path.unwrap();
        let installation_path = root.join("installation.yaml");
        let installation = InstallationConfig {
            path: Some(installation_path.clone()),
            root: root.clone(),
            front_door_socket: root.join("amux.sock"),
            host_name: name.to_string(),
            prevent_idle_sleep: Some(false),
            keymaps_dir: root.join("keymaps"),
            ..InstallationConfig::default()
        };
        std::fs::write(
            &installation_path,
            serde_yaml::to_string(&installation).unwrap(),
        )
        .unwrap();
        let config = Config {
            host_name: name.to_string(),
            socket_path: paths.socket_path,
            state_path: paths.state_path,
            data_dir: paths.data_dir,
            prevent_idle_sleep: Some(false),
            path: Some(config_path.clone()),
            ..Config::default()
        };
        let profile = ProfileConfig {
            installation_config: installation_path,
            socket_path: config.socket_path.clone(),
            state_path: config.state_path.clone(),
            data_dir: config.data_dir.clone(),
            cloud_url: config.cloud_url.clone(),
            tcp_port: None,
        };
        std::fs::write(&config_path, serde_yaml::to_string(&profile).unwrap()).unwrap();

        let front_path = installation.front_door_socket.clone();
        let owner = Arc::new(
            amux::installation::Installation::from_config(installation)
                .await
                .unwrap(),
        );
        let front_door = amux::installation::FrontDoor::new(owner.clone(), Some(front_path))
            .listen()
            .unwrap();
        let sibling = owner
            .create(OperationId::new(), Some("elsewhere".into()))
            .await
            .unwrap();
        let sibling_id = sibling.record.id;
        let sibling_config = ProfilePaths::for_id(&root, sibling_id)
            .unwrap()
            .config_path
            .unwrap();
        let sibling_client = owner.client(sibling_id).unwrap();
        std::fs::write(root.join("state/last-profile"), format!("{sibling_id}\n")).unwrap();
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
        for client in [&client, &sibling_client] {
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
        }

        Self {
            root,
            executable,
            config_path,
            config,
            client,
            sibling_client,
            sibling_config,
            sibling_id,
            installation: owner,
            front_door,
            agent_id,
            host_id,
        }
    }

    fn call_status(&self, path: &Path, marker: &str, explicit_config: bool) -> Output {
        let mut args = vec![
            "mcp",
            "agent",
            "--socket-path",
            self.config.socket_path.to_str().unwrap(),
        ];
        if explicit_config {
            args.extend(["--config", self.config_path.to_str().unwrap()]);
        }
        let mut child = Command::new(&self.executable)
            .args(args)
            .env(
                "AMUX_CONFIG",
                if explicit_config {
                    &self.sibling_config
                } else {
                    &self.config_path
                },
            )
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

    async fn assert_hook_reentry(&self, path: &Path) {
        let receiver = claude::hooks::HookReceiver::bind_sync(&self.root.join("hooks")).unwrap();
        let other = claude::hooks::HookReceiver::bind_sync(&self.root.join("other-hooks")).unwrap();
        let mut payloads = receiver.payloads();
        let mut other_payloads = other.payloads();
        for ambient in [&self.sibling_config, &self.root.join("missing.yaml")] {
            let payload = json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": self.agent_id,
                "transcript_path": self.root.join("transcript.jsonl"),
                "cwd": self.root,
                "prompt": "launching profile hook"
            });
            let mut child = Command::new(&self.executable)
                .args(["hooks", "claude"])
                .env("AMUX_CONFIG", ambient)
                .env("CLAUDE_HOOK_SOCKET", &receiver.path)
                .env_remove("CLAUDE_CODE_MESSAGING_SOCKET")
                .env_remove("CLAUDE_CODE_MESSAGING_TOKEN")
                .env("PATH", path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            writeln!(child.stdin.take().unwrap(), "{payload}").unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty());
            let received = tokio::time::timeout(Duration::from_secs(5), payloads.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(received.common().session_id, self.agent_id);
            assert_eq!(received.common().raw, payload);
            println!(
                "Hook received on {} with ambient config {}: {}",
                receiver.path.display(),
                ambient.display(),
                received.common().raw
            );
        }
        assert!(other_payloads.try_recv().is_err());
        assert_eq!(
            std::fs::read_to_string(self.root.join("state/last-profile")).unwrap(),
            format!("{}\n", self.sibling_id)
        );
    }

    async fn shutdown(self) {
        self.front_door.stop().await;
        Arc::try_unwrap(self.installation)
            .ok()
            .expect("front door released its owner")
            .shutdown(amux::ShutdownReason::UserRequested)
            .await;
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
    println!("MCP stdout:\n{rows}");
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
async fn mcp_route_isolation_uses_absolute_routes_and_fails_closed() {
    let temporary = tempfile::Builder::new()
        .prefix("mcp")
        .tempdir_in("/tmp")
        .unwrap();
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
    for explicit_config in [false, true] {
        assert_successful_status_call(&first.call_status(&poison, first_marker, explicit_config));
        assert_successful_status_call(&second.call_status(&poison, second_marker, explicit_config));
    }

    first.assert_hook_reentry(&poison).await;
    second.assert_hook_reentry(&poison).await;
    for route in [&first, &second] {
        let untouched = route.sibling_client.list_agents().await.unwrap();
        assert_eq!(untouched.len(), 1);
        assert_eq!(untouched[0].id, route.agent_id);
        assert!(untouched[0].working_on.is_none());
        println!(
            "Sibling profile {} inventory: {}",
            route.sibling_id,
            serde_json::to_string(&untouched).unwrap()
        );
    }

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

    println!(
        "Crossed config/socket stderr: {}",
        String::from_utf8_lossy(&crossed.stderr)
    );
    for route in [&first, &second] {
        println!(
            "Launching profile inventory: {}",
            serde_json::to_string(&route.client.list_agents().await.unwrap()).unwrap()
        );
    }
    let unconfigured = Command::new(&first.executable)
        .args(["mcp", "agent"])
        .env_remove("AMUX_CONFIG")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!unconfigured.status.success());
    assert!(String::from_utf8_lossy(&unconfigured.stderr).contains("MCP requires AMUX_CONFIG"));
    println!(
        "Missing launch config stderr: {}",
        String::from_utf8_lossy(&unconfigured.stderr)
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

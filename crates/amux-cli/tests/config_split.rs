#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use amux::installation::{InstallationRoot, ProfileId, ProfileLabel, ProfilePaths, Registry};
use amux::{InstallationConfig, ProfileConfig};

struct Fixture {
    _temp: tempfile::TempDir,
    installation: InstallationConfig,
    profile: PathBuf,
    id: ProfileId,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("cli")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        let installation = InstallationConfig {
            root: root.clone(),
            front_door_socket: root.join("amux.sock"),
            host_name: "cli-installation".into(),
            prevent_idle_sleep: Some(false),
            keymaps_dir: root.join("keymaps"),
            path: Some(root.join("installation.yaml")),
            ..InstallationConfig::default()
        };
        write(installation.path.as_ref().unwrap(), &installation);
        let id = ProfileId(uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            b"cli-config-split",
        ));
        let mut registry = Registry::open(InstallationRoot::OnDisk(root.clone())).unwrap();
        registry
            .create(
                id,
                ProfileLabel {
                    override_name: Some("personal".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let paths = ProfilePaths::for_id(&root, id).unwrap();
        let profile = paths.config_path.unwrap();
        write(
            &profile,
            &ProfileConfig {
                installation_config: installation.path.clone().unwrap(),
                socket_path: paths.socket_path,
                data_dir: paths.data_dir,
                state_path: paths.state_path,
                cloud_url: "https://amux.sh".into(),
                tcp_port: None,
            },
        );
        Self {
            _temp: temp,
            installation,
            profile,
            id,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_amux"));
        cmd.env("AMUX_CONFIG", &self.profile)
            .env("AMUX_LOG", self.installation.root.join("daemon.log"))
            .args(args);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        println!(
            "$ amux {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{output:?}");
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.command(&["server", "stop"]).output();
    }
}

fn write(path: &Path, value: &impl serde::Serialize) {
    std::fs::write(path, serde_yaml::to_string(value).unwrap()).unwrap();
}

#[cfg(debug_assertions)]
mod saved_report_replay {
    use super::*;

    fn copy_report(parent: &Path) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../amux-tui/tests/reports/chat_agent_activity");
        let report = parent.join("saved-report");
        std::fs::create_dir_all(&report).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), report.join(entry.file_name())).unwrap();
        }
        report
    }

    async fn replay(mut command: Command, requested: &Path, report: &Path) {
        command
            .args(["debug", "report", "replay"])
            .arg(requested)
            .arg("--frame");
        println!("$ {command:?}");
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::process::Command::from(command)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("saved replay must not wait for an installation socket")
        .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        println!("{text}{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", output.status);
        assert!(text.starts_with("Reproduces\nDiffering cells: none\nBounding rectangle: none\n"));
        let (_, frame) = text.split_once("Frame at event ").unwrap();
        let (_, frame) = frame.split_once(":\n").unwrap();
        assert_eq!(
            frame,
            std::fs::read_to_string(report.join("frame.txt")).unwrap()
        );
    }

    #[tokio::test]
    async fn saved_report_replay_without_installation_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let report = copy_report(&root);
        let config_home = root.join("no-config");
        let runtime = root.join("no-runtime");
        for requested in [&report, Path::new("saved-report")] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_amux"));
            command
                .current_dir(&root)
                .env_remove("AMUX_CONFIG")
                .env("XDG_CONFIG_HOME", &config_home)
                .env("XDG_DATA_HOME", root.join("no-data"))
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("TMPDIR", &runtime)
                .env("AMUX_LOG", root.join("replay.log"));
            replay(command, requested, &report).await;
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_amux"));
        command
            .env("AMUX_CONFIG", root.join("missing-profile.yaml"))
            .env("AMUX_LOG", root.join("replay.log"))
            .args(["--profile", "unavailable-account"]);
        replay(command, &report, &report).await;
        assert!(!config_home.exists());
        assert!(!runtime.exists());
        assert!(!root.join("no-data").exists());
        println!("Absolute and relative saved reports replay without installation configuration.");
    }

    #[tokio::test]
    async fn saved_report_replay_leaves_a_stopped_installation_stopped() {
        let fixture = Fixture::new();
        let report = copy_report(&fixture.installation.root);
        fixture.run(&["server", "start"]);
        fixture.run(&["server", "stop"]);
        assert!(!fixture.installation.front_door_socket.exists());
        replay(fixture.command(&[]), &report, &report).await;
        assert!(!fixture.installation.front_door_socket.exists());
        let profile = amux::load_profile_config(&fixture.profile).unwrap().profile;
        assert!(!profile.socket_path.exists());
        let output = fixture.command(&["profiles"]).output().unwrap();
        assert!(!output.status.success(), "replay must not start the daemon");
        println!(
            "Installation and profile sockets remain absent; profiles confirms no server running."
        );
    }

    #[tokio::test]
    async fn saved_report_replay_does_not_contact_the_installation_socket() {
        let fixture = Fixture::new();
        let report = copy_report(&fixture.installation.root);
        let listener =
            std::os::unix::net::UnixListener::bind(&fixture.installation.front_door_socket)
                .unwrap();
        listener.set_nonblocking(true).unwrap();
        replay(fixture.command(&[]), &report, &report).await;
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(listener);
        std::fs::remove_file(&fixture.installation.front_door_socket).unwrap();
        println!("Listening installation socket receives no connection during replay.");
    }

    #[tokio::test]
    async fn saved_report_replay_by_name_uses_the_selected_profile() {
        let fixture = Fixture::new();
        fixture.run(&["server", "start"]);
        let created = fixture.run(&["profile", "create", "work"]);
        let created = String::from_utf8(created.stdout).unwrap();
        let id = ProfileId(created.split_whitespace().next().unwrap().parse().unwrap());
        let paths = ProfilePaths::for_id(&fixture.installation.root, id).unwrap();
        let report = copy_report(&paths.data_dir.join("reports"));
        replay(
            fixture.command(&["--profile", "work"]),
            Path::new("saved-report"),
            &report,
        )
        .await;
        let output = fixture
            .command(&[
                "--profile",
                "personal",
                "debug",
                "report",
                "replay",
                "saved-report",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let personal = ProfilePaths::for_id(&fixture.installation.root, fixture.id).unwrap();
        let expected = format!(
            "report {} has no captured frame",
            personal.reports_dir.join("saved-report").display()
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&expected),
            "{output:?}"
        );
        println!(
            "Bare report names resolve under the selected profile; Personal cannot find Work's report."
        );
    }
}

#[test]
fn config_split_cli_boots_probes_stops_and_restarts_an_installation() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .command(&["profiles"])
            .output()
            .unwrap()
            .status
            .success()
    );
    for _ in 0..2 {
        fixture.run(&["server", "start"]);
        let output = fixture.run(&["profiles"]);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("{}  personal", fixture.id))
        );
        fixture.run(&["list"]);
        fixture.run(&["server", "start"]);
        fixture.run(&["server", "stop"]);
        assert!(!fixture.installation.front_door_socket.exists());
        Registry::open(InstallationRoot::OnDisk(fixture.installation.root.clone())).unwrap();
    }
}

#[test]
fn config_split_cli_rejects_path_disagreement_before_starting() {
    let fixture = Fixture::new();
    let mut profile = amux::load_profile_config(&fixture.profile).unwrap().profile;
    profile.socket_path = fixture.installation.root.join("wrong.sock");
    write(&fixture.profile, &profile);
    let output = fixture.command(&["server", "start"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("socket_path"));
    assert!(!fixture.installation.front_door_socket.exists());
}

#[test]
fn config_split_worktree_generator_uses_stable_uuid_and_profile_alias() {
    let fixture = Fixture::new();
    let root = fixture.installation.root.join("wt");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/worktree-profile.py");
    for _ in 0..2 {
        let output = Command::new("python3")
            .arg(&script)
            .arg(&root)
            .arg("example-tree")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let installation = InstallationConfig {
        root: root.clone(),
        front_door_socket: root.join("amux.sock"),
        ..fixture.installation.clone()
    };
    write(&root.join("installation.yaml"), &installation);
    let config = amux::load_profile_config(&root.join("profile.yaml")).unwrap();
    assert_eq!(
        config.profile_id.0,
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"amux-worktree:example-tree")
    );
    assert_eq!(config.profile_id.0.get_version_num(), 5);
}

#[test]
fn profile_selector_cli_lifecycle_keeps_each_commands_selection() {
    let fixture = Fixture::new();
    fixture.run(&["server", "start"]);
    let created = fixture.run(&["profile", "create", "work"]);
    let text = String::from_utf8(created.stdout).unwrap();
    let work = text.split_whitespace().next().unwrap();
    uuid::Uuid::parse_str(work).unwrap();
    fixture.run(&["list", "--profile", "work"]);
    let remembered = fixture.installation.root.join("state/last-profile");
    assert_eq!(std::fs::read_to_string(&remembered).unwrap().trim(), work);
    fixture.run(&["profile", "rename", "office", "--profile", work]);
    fixture.run(&["--profile", "office", "profile", "pause"]);
    let listing = fixture.run(&["profiles"]);
    assert!(String::from_utf8_lossy(&listing.stdout).contains("paused"));
    fixture.run(&["profile", "resume", "--profile", work]);
    fixture.run(&["profile", "rename", "--clear", "--profile", work]);
    fixture.run(&["list"]);
    assert_eq!(
        std::fs::read_to_string(&remembered).unwrap().trim(),
        fixture.id.to_string()
    );
    let unknown = fixture
        .command(&["list", "--profile", "missing"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("Unknown profile"));
    assert_eq!(
        std::fs::read_to_string(&remembered).unwrap().trim(),
        fixture.id.to_string()
    );
    let refused = fixture
        .command(&["profile", "delete", "--profile", work])
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "piped deletion must require --yes"
    );
    fixture.run(&["profile", "delete", "--profile", work, "--yes"]);
    assert!(
        !fixture
            .installation
            .root
            .join("profiles")
            .join(format!("{work}.sock"))
            .exists()
    );
    fixture.run(&["list"]);
}

#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_selector_cli_login_and_logout_preserve_the_device() {
    use amux::test_fixtures::{IdentityServer, TestAccount};
    let identity = IdentityServer::start(
        vec![TestAccount {
            sub: "alice".into(),
            name: Some("Alice Example".into()),
            email: Some("alice@example.test".into()),
        }],
        None,
    )
    .await;
    let fixture = Fixture::new();
    let mut config = amux::load_profile_config(&fixture.profile).unwrap().profile;
    config.cloud_url = identity.url();
    write(&fixture.profile, &config);
    fixture.run(&["server", "start"]);
    fixture.run(&["profile", "rename", "--clear"]);
    let key_path = config.data_dir.join("device.key");
    let key = std::fs::read(&key_path).unwrap();
    let login = fixture.run(&["login"]);
    let text = String::from_utf8_lossy(&login.stdout);
    assert!(text.contains("Alice Example") && text.contains("alice@example.test"));
    assert!(
        text.contains(&fixture.id.to_string()),
        "sole pristine profile is adopted"
    );
    let credential = fixture.profile.with_file_name("credentials.yaml");
    assert!(credential.exists());
    fixture.run(&["logout"]);
    assert_eq!(std::fs::read(&key_path).unwrap(), key);
    let info = fixture.run(&["profiles"]);
    assert!(String::from_utf8_lossy(&info.stdout).contains("logged_out"));
    let login = fixture.run(&[
        "login",
        "--profile",
        &fixture.id.to_string(),
        "--name",
        "Personal",
    ]);
    assert!(String::from_utf8_lossy(&login.stdout).contains("Personal"));
    assert_eq!(std::fs::read(&key_path).unwrap(), key);
    fixture.run(&["list"]);
}

#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_selector_cli_non_pristine_login_requires_confirmation() {
    use amux::test_fixtures::{IdentityServer, TestAccount};
    let identity = IdentityServer::start(
        vec![TestAccount {
            sub: "alice".into(),
            name: Some("Alice Example".into()),
            email: Some("alice@example.test".into()),
        }],
        None,
    )
    .await;
    let fixture = Fixture::new();
    let mut config = amux::load_profile_config(&fixture.profile).unwrap().profile;
    config.cloud_url = identity.url();
    write(&fixture.profile, &config);
    fixture.run(&["server", "start"]);
    std::fs::write(
        config.data_dir.join("cache/artifacts/retained"),
        b"retained artifact",
    )
    .unwrap();
    let output = fixture
        .command(&["login", "--profile", &fixture.id.to_string()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("personal")
            && error.contains("Confirmation requires an interactive terminal"),
        "{error}"
    );
    assert!(!fixture.profile.with_file_name("credentials.yaml").exists());
    assert!(String::from_utf8_lossy(&fixture.run(&["profiles"]).stdout).contains("unbound"));
}

#[test]
fn profile_selector_cli_fresh_init_and_default_last_used() {
    use std::io::Write;
    use std::process::Stdio;
    let temporary = tempfile::Builder::new()
        .prefix("ci")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let command = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_amux"));
        command
            .env_remove("AMUX_CONFIG")
            .env("XDG_CONFIG_HOME", root.join("c"))
            .env("XDG_DATA_HOME", root.join("d"))
            .env("XDG_STATE_HOME", root.join("s"))
            .env("TMPDIR", root.join("r"))
            .env("XDG_RUNTIME_DIR", root.join("r"))
            .env("AMUX_LOG", root.join("amux.log"))
            .args(args);
        command
    };
    // Ensure a failing assertion still stops the isolated daemon.
    struct Stop(Command);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.output();
        }
    }
    let _stop = Stop(command(&["server", "stop"]));
    let mut init = command(&["init"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    init.stdin.take().unwrap().write_all(b"2\n").unwrap();
    let output = init.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Created unbound profile"));
    assert!(!text.contains("Waiting for authentication"));
    let run = |args: &[&str]| {
        let output = command(args).output().unwrap();
        println!(
            "$ amux {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["list"]);
    let second = run(&["profile", "create", "Work"]);
    let id = second.split_whitespace().next().unwrap();
    run(&["list", "--profile", "Work"]);
    run(&["profile", "rename", "Office"]);
    let remembered = root.join("d/amux/state/last-profile");
    assert_eq!(std::fs::read_to_string(&remembered).unwrap().trim(), id);
    assert!(run(&["profiles"]).contains("Office"));
    run(&["profile", "delete", "--yes"]);
    run(&["list"]); // A stale remembered UUID falls back to the remaining profile.
    run(&["profile", "delete", "--yes"]);
    let empty = command(&["list"]).output().unwrap();
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("No profiles"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn front_door_cli_pairing_and_trust_stay_with_the_selected_profile() {
    use std::io::Write;
    use std::process::Stdio;

    use amux::installation::FrontDoorClient;

    let local = Fixture::new();
    let remote = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut config = amux::load_profile_config(&remote.profile).unwrap().profile;
    config.tcp_port = Some(address.port());
    write(&remote.profile, &config);
    drop(listener);
    local.run(&["server", "start"]);
    remote.run(&["server", "start"]);
    local.run(&["profile", "create", "work"]);
    let front = FrontDoorClient::connect(&local.installation.front_door_socket)
        .await
        .unwrap();
    let personal = front.admin(local.id);
    local.run(&[
        "pair",
        "--profile",
        "personal",
        "--demo",
        "--pin",
        "123456",
        "--for",
        "5m",
    ]);
    remote.run(&["pair", "--demo", "--pin", "654321", "--for", "5m"]);
    assert!(personal.pairing_is_active().await.unwrap());

    let mut pair = local
        .command(&[
            "pair",
            "--profile",
            "work",
            "--connect",
            &address.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    pair.stdin.take().unwrap().write_all(b"654321\n").unwrap();
    let output = pair.wait_with_output().unwrap();
    println!(
        "$ amux pair --profile work --connect {address}\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("via direct TCP"));
    assert!(personal.list_peers().await.unwrap().is_empty());
    assert!(
        personal.pairing_is_active().await.unwrap(),
        "pairing another profile does not close this window"
    );
    let peers = local.run(&["peer", "list", "--profile", "work"]);
    assert!(String::from_utf8_lossy(&peers.stdout).contains("cli-installation"));
    let info = local.run(&["peer", "info", "cli-installation", "--profile", "work"]);
    assert!(String::from_utf8_lossy(&info.stdout).contains(&format!("direct-tcp:{address}")));
    local.run(&["unpair", "cli-installation", "--profile", "work", "--force"]);
    let peers = local.run(&["peer", "list", "--profile", "work"]);
    assert!(String::from_utf8_lossy(&peers.stdout).contains("No trusted peers"));
    local.run(&["pair", "--profile", "personal", "--cancel"]);
    assert!(!personal.pairing_is_active().await.unwrap());
}

#[test]
fn front_door_cli_keymaps_use_installation_preferences_without_profiles() {
    let fixture = Fixture::new();
    let mut installation = fixture.installation.clone();
    installation.keymaps_dir = installation.root.join("shared/custom-keymaps");
    write(installation.path.as_ref().unwrap(), &installation);
    let directory = fixture.run(&["keymap", "dir"]);
    assert_eq!(
        String::from_utf8(directory.stdout).unwrap().trim(),
        installation.keymaps_dir.to_str().unwrap()
    );
    assert!(
        !installation.front_door_socket.exists(),
        "keymaps need no daemon"
    );
    fixture.run(&["server", "start"]);
    fixture.run(&["profile", "create", "work"]);
    let input = installation.root.join("keymap.toml");
    std::fs::write(&input, claude::pty::keymap::BAKED_KEYMAPS[0].1).unwrap();
    fixture.run(&[
        "keymap",
        "add",
        input.to_str().unwrap(),
        "--profile",
        "work",
    ]);
    assert!(installation.keymaps_dir.join("claude-2.1.toml").exists());
    fixture.run(&["keymap", "show", "claude-2.1", "--profile", "personal"]);
    let config_home = installation.root.join("config");
    std::fs::create_dir_all(config_home.join("amux")).unwrap();
    write(&config_home.join("amux/config.yaml"), &installation);
    let default_command = |args: &[&str]| {
        let mut command = fixture.command(args);
        command
            .env_remove("AMUX_CONFIG")
            .env("XDG_CONFIG_HOME", &config_home);
        command
    };
    struct Stop(Command);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.output();
        }
    }
    let _stop = Stop(default_command(&["server", "stop"]));
    fixture.run(&["profile", "delete", "--profile", "work", "--yes"]);
    fixture.run(&["profile", "delete", "--yes"]);
    let output = default_command(&["keymap", "remove", "claude-2.1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!installation.keymaps_dir.join("claude-2.1.toml").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn front_door_cli_ssh_pair_receiver_uses_its_explicit_profile() {
    use std::process::Stdio;
    use std::time::Duration;

    use amux::installation::FrontDoorClient;
    let local = Fixture::new();
    let remote = Fixture::new();
    local.run(&["server", "start"]);
    remote.run(&["server", "start"]);
    let created = remote.run(&["profile", "create", "work"]);
    let created = String::from_utf8(created.stdout).unwrap();
    let work_id = ProfileId(created.split_whitespace().next().unwrap().parse().unwrap());
    let local_front = FrontDoorClient::connect(&local.installation.front_door_socket)
        .await
        .unwrap();
    let remote_front = FrontDoorClient::connect(&remote.installation.front_door_socket)
        .await
        .unwrap();
    let local_admin = local_front.admin(local.id);
    let remote_personal = remote_front.admin(remote.id);
    let remote_work = remote_front.admin(work_id);
    let local_data = amux::load_profile_config(&local.profile)
        .unwrap()
        .profile
        .data_dir;
    let mut receiver =
        tokio::process::Command::from(remote.command(&["pair-recv", "--profile", "work"]))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let io = tokio::io::join(
        receiver.stdout.take().unwrap(),
        receiver.stdin.take().unwrap(),
    );
    let peer = tokio::time::timeout(
        Duration::from_secs(10),
        amux::pair_via_ssh_initiator(io, local_data, "local", "remote.example", &local_admin),
    )
    .await
    .unwrap()
    .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(5), receiver.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(remote_personal.list_peers().await.unwrap().is_empty());
    assert_eq!(remote_work.list_peers().await.unwrap().len(), 1);
    assert_eq!(
        local_admin
            .get_peer(peer.identity.host_id)
            .await
            .unwrap()
            .name,
        "cli-installation"
    );
    println!(
        "SSH pair-recv --profile work completes the framed identity exchange and stores trust only in work."
    );
}

#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_renamed_profile() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use amux::installation::FrontDoorClient;
    use amux::{AgentType, Config, CreateAgentRequest, PeerReachability, Server};
    use serde_json::{Value, json};

    async fn profile_client(path: &Path) -> amux::Client {
        let resolved = amux::load_profile_config(path).unwrap();
        Server::builder()
            .config(Config {
                socket_path: resolved.profile.socket_path,
                ..Config::default()
            })
            .daemon()
            .open()
            .await
            .unwrap()
    }

    let local = Fixture::new();
    let remote = Fixture::new();
    remote.run(&["server", "start"]);
    let created = remote.run(&["profile", "create", "work"]);
    let created = String::from_utf8(created.stdout).unwrap();
    let work_id = ProfileId(created.split_whitespace().next().unwrap().parse().unwrap());
    let work_config = ProfilePaths::for_id(&remote.installation.root, work_id)
        .unwrap()
        .config_path
        .unwrap();
    let selection = local.installation.root.join("remote-config");
    std::fs::write(&selection, work_config.to_str().unwrap()).unwrap();
    let calls = local.installation.root.join("ssh-argv.jsonl");
    let fake_ssh = local.installation.root.join("fake-ssh");
    // Replace only SSH transport. The receiver, relay, trust administration and
    // reconnecting daemon all run the real binary with the real split configs.
    let script = format!(
        r#"#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
assert args[:6] == ["-T", "-o", "BatchMode=yes", "--", "remote.example", "amux"], args
assert args[6:] == ["pair-recv"] or (len(args) == 9 and args[6:8] == ["relay", "--profile"]), args
fd = os.open({calls}, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
os.write(fd, (json.dumps(args) + "\n").encode())
os.close(fd)
env = dict(os.environ)
env["AMUX_CONFIG"] = Path({selection}).read_text()
env["AMUX_LOG"] = {remote_log}
env.pop("AMUX_SSH", None)
os.execve({binary}, [{binary}] + args[6:], env)
"#,
        calls = json!(calls),
        selection = json!(selection),
        remote_log = json!(remote.installation.root.join("ssh.log")),
        binary = json!(env!("CARGO_BIN_EXE_amux")),
    );
    std::fs::write(&fake_ssh, script).unwrap();
    std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let run_local_ssh = |args: &[&str]| {
        let output = local
            .command(args)
            .env("AMUX_SSH", &fake_ssh)
            .output()
            .unwrap();
        println!(
            "$ amux {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{output:?}");
        output
    };
    run_local_ssh(&["server", "start"]);
    run_local_ssh(&["pair", "--via-ssh", "remote.example"]);

    let local_front = FrontDoorClient::connect(&local.installation.front_door_socket)
        .await
        .unwrap();
    let remote_front = FrontDoorClient::connect(&remote.installation.front_door_socket)
        .await
        .unwrap();
    let peers = local_front.admin(local.id).list_peers().await.unwrap();
    assert_eq!(peers.len(), 1);
    let paired_host = peers[0].host_id;
    assert_eq!(
        peers[0].reachabilities,
        vec![PeerReachability::Ssh {
            target: "remote.example".into(),
            profile: work_id,
        }]
    );
    assert!(
        remote_front
            .admin(remote.id)
            .list_peers()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        remote_front
            .admin(work_id)
            .list_peers()
            .await
            .unwrap()
            .len(),
        1
    );

    let agent_id = uuid::Uuid::new_v4();
    for (path, name) in [
        (&work_config, "ssh-work-agent"),
        (&remote.profile, "ssh-personal-agent"),
    ] {
        let client = profile_client(path).await;
        client
            .create_agent(CreateAgentRequest {
                agent_id,
                host_id: None,
                name: Some(name.into()),
                agent_type: AgentType::TestAgent {
                    command: "cat".into(),
                },
                working_dir: remote.installation.root.clone(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
            .unwrap();
    }
    let wait_for_work = || async {
        let client = profile_client(&local.profile).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let agents = client.list_agents().await.unwrap();
                assert!(
                    !agents
                        .iter()
                        .any(|agent| agent.name.as_deref() == Some("ssh-personal-agent"))
                );
                if agents.iter().any(|agent| {
                    agent.id == agent_id && agent.name.as_deref() == Some("ssh-work-agent")
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("SSH must reach the paired work profile's fleet");
    };
    wait_for_work().await;
    local.run(&["peer", "list"]);
    local.run(&["list"]);
    local.run(&["server", "stop"]);
    drop(local_front);

    remote.run(&[
        "profile",
        "rename",
        "office",
        "--profile",
        &work_id.to_string(),
    ]);
    remote.run(&["list", "--profile", "personal"]);
    std::fs::write(&selection, remote.profile.to_str().unwrap()).unwrap();
    let remote_last_used = remote.installation.root.join("state/last-profile");
    assert_eq!(
        std::fs::read_to_string(&remote_last_used).unwrap().trim(),
        remote.id.to_string()
    );
    let before_redial = std::fs::read_to_string(&calls).unwrap().lines().count();
    println!(
        "Remote profile {work_id} renamed work → office; remote AMUX_CONFIG and last-used now select personal."
    );
    run_local_ssh(&["server", "start"]);
    wait_for_work().await;
    let restarted_front = FrontDoorClient::connect(&local.installation.front_door_socket)
        .await
        .unwrap();
    assert_eq!(
        restarted_front
            .admin(local.id)
            .get_peer(paired_host)
            .await
            .unwrap()
            .reachabilities,
        peers[0].reachabilities
    );
    local.run(&["peer", "list"]);
    let fleet = local.run(&["list"]);
    assert!(String::from_utf8_lossy(&fleet.stdout).contains("ssh-work-agent"));
    assert!(!String::from_utf8_lossy(&fleet.stdout).contains("ssh-personal-agent"));
    let recorded = std::fs::read_to_string(&calls).unwrap();
    println!("SSH argv at the process boundary:\n{recorded}");
    let argv: Vec<Value> = recorded
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(argv.iter().any(|args| args[6] == "pair-recv"));
    assert!(
        argv.len() > before_redial,
        "restart must spawn a new SSH relay"
    );
    for args in &argv[before_redial..] {
        assert_eq!(
            args,
            &json!([
                "-T",
                "-o",
                "BatchMode=yes",
                "--",
                "remote.example",
                "amux",
                "relay",
                "--profile",
                work_id.to_string()
            ])
        );
    }
    assert!(
        remote_front
            .admin(remote.id)
            .list_peers()
            .await
            .unwrap()
            .is_empty()
    );
}

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

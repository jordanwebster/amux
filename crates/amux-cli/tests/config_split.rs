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

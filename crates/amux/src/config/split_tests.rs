use super::*;
#[cfg(all(unix, feature = "local-agents"))]
use crate::installation::{FrontDoor, OperationId};
#[cfg(feature = "local-agents")]
use crate::installation::{Installation, InstallationError};
use crate::installation::{InstallationRoot, ProfileId, ProfileLabel, ProfilePaths, Registry};
#[cfg(all(unix, feature = "local-agents"))]
use crate::server::ShutdownReason;

struct Fixture {
    _temp: tempfile::TempDir,
    installation: InstallationConfig,
    id: ProfileId,
    paths: ProfilePaths,
    profile: ProfileConfig,
}

impl Fixture {
    fn new() -> Self {
        let temp = crate::test_fixtures::short_installation_root();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let installation = InstallationConfig {
            root: root.clone(),
            front_door_socket: root.join("amux.sock"),
            host_name: "shared-device".into(),
            prevent_idle_sleep: Some(false),
            keymaps_dir: root.join("keymaps"),
            path: Some(root.join("installation.yaml")),
            ..InstallationConfig::default()
        };
        write(&installation.file_path(), &installation);
        let id = ProfileId::new();
        let mut registry = Registry::open(InstallationRoot::OnDisk(root.clone())).unwrap();
        registry.create(id, ProfileLabel::default()).unwrap();
        let paths = ProfilePaths::for_id(&root, id).unwrap();
        let profile = ProfileConfig {
            installation_config: installation.file_path(),
            socket_path: paths.socket_path.clone(),
            data_dir: paths.data_dir.clone(),
            state_path: paths.state_path.clone(),
            cloud_url: "https://account.example".into(),
            tcp_port: None,
        };
        write(paths.config_path.as_ref().unwrap(), &profile);
        Self {
            _temp: temp,
            installation,
            id,
            paths,
            profile,
        }
    }

    fn load(&self) -> Result<ResolvedConfig, ConfigError> {
        load_profile_config(self.paths.config_path.as_ref().unwrap())
    }
}

fn write(path: &Path, value: &impl Serialize) {
    std::fs::write(path, serde_yaml::to_string(value).unwrap()).unwrap();
}

#[test]
fn config_split_loads_shared_preferences_and_profile_paths() {
    let mut fixture = Fixture::new();
    let config = fixture.load().unwrap();
    assert_eq!(config.profile_id, fixture.id);
    assert_eq!(config.installation.host_name, "shared-device");
    assert_eq!(config.profile.cloud_url, "https://account.example");
    assert_eq!(
        config.profile.socket_path,
        fixture
            .installation
            .root
            .join(format!("profiles/{}.sock", fixture.id))
    );
    assert_eq!(
        config.artifact_cache_dir(),
        fixture.paths.data_dir.join("cache/artifacts")
    );
    assert_eq!(config.reports_dir(), fixture.paths.reports_dir);
    fixture.installation.reports_dir = Some(fixture.installation.root.join("shared-reports"));
    write(&fixture.installation.file_path(), &fixture.installation);
    assert_eq!(
        fixture.load().unwrap().reports_dir(),
        fixture.installation.reports_dir.unwrap()
    );
}

#[test]
fn config_split_resolves_relative_paths_beside_each_file() {
    let mut fixture = Fixture::new();
    fixture.installation.root = PathBuf::from(".");
    fixture.installation.front_door_socket = PathBuf::from("amux.sock");
    fixture.installation.keymaps_dir = PathBuf::from("keymaps");
    fixture.installation.ui.theme = ThemeSetting::File(PathBuf::from("themes/night.yaml"));
    write(&fixture.installation.file_path(), &fixture.installation);
    fixture.profile.data_dir = PathBuf::from("data");
    fixture.profile.state_path = PathBuf::from("state/state.yaml");
    fixture.profile.socket_path = PathBuf::from(format!("../{}.sock", fixture.id));
    write(
        fixture.paths.config_path.as_ref().unwrap(),
        &fixture.profile,
    );
    let config = fixture.load().unwrap();
    assert_eq!(config.profile.data_dir, fixture.paths.data_dir);
    assert_eq!(config.profile.state_path, fixture.paths.state_path);
    assert_eq!(config.profile.socket_path, fixture.paths.socket_path);
    assert_eq!(
        config.installation.ui.theme,
        ThemeSetting::File(config.installation.root.join("themes/night.yaml"))
    );
}

#[test]
fn config_split_rejects_unknown_fields_and_missing_installation() {
    let fixture = Fixture::new();
    let profile_path = fixture.paths.config_path.as_ref().unwrap();
    for field in [
        "enable_cloud_mode: true",
        "host_name: wrong-owner",
        "surprise: true",
    ] {
        let yaml = serde_yaml::to_string(&fixture.profile).unwrap();
        std::fs::write(profile_path, format!("{yaml}{field}\n")).unwrap();
        assert!(
            matches!(fixture.load(), Err(ConfigError::Invalid(error)) if error.contains("unknown field"))
        );
    }
    let mut relative = fixture.profile.clone();
    relative.installation_config = PathBuf::from("../../installation.yaml");
    write(profile_path, &relative);
    assert!(
        matches!(fixture.load(), Err(ConfigError::Invalid(error)) if error.contains("absolute path"))
    );
    write(profile_path, &fixture.profile);
    let yaml = serde_yaml::to_string(&fixture.installation).unwrap();
    std::fs::write(
        fixture.installation.file_path(),
        format!("{yaml}data_dir: /wrong-owner\n"),
    )
    .unwrap();
    assert!(
        matches!(fixture.load(), Err(ConfigError::Invalid(error)) if error.contains("unknown field"))
    );
    std::fs::remove_file(fixture.installation.file_path()).unwrap();
    assert!(
        matches!(fixture.load(), Err(ConfigError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
}

#[cfg(feature = "local-agents")]
#[tokio::test]
async fn config_split_path_disagreement_fails_before_any_runtime_starts() {
    for field in [
        "socket_path",
        "data_dir",
        "state_path",
        "installation_config",
    ] {
        let mut fixture = Fixture::new();
        let wrong = fixture.installation.root.join("wrong");
        match field {
            "socket_path" => fixture.profile.socket_path = wrong,
            "data_dir" => fixture.profile.data_dir = wrong,
            "state_path" => fixture.profile.state_path = wrong,
            "installation_config" => {
                write(&wrong, &fixture.installation);
                fixture.profile.installation_config = wrong;
            }
            _ => unreachable!(),
        }
        write(
            fixture.paths.config_path.as_ref().unwrap(),
            &fixture.profile,
        );
        let result = Installation::from_config(fixture.installation.clone()).await;
        assert!(matches!(result,
            Err(InstallationError::Config(ConfigError::Disagreement { field: actual, .. })) if actual == field));
        assert!(!fixture.paths.socket_path.exists());
        assert!(!fixture.paths.data_dir.join("device.key").exists());
        // Failed validation releases the installation lock.
        Registry::open(InstallationRoot::OnDisk(fixture.installation.root)).unwrap();
    }
}

#[cfg(all(unix, feature = "local-agents"))]
#[tokio::test]
async fn config_split_boots_from_temp_root_and_discovers_profile_over_grpc() {
    use std::sync::Arc;

    use crate::protocol::wire;
    use crate::transport::{self, GrpcIo};
    let fixture = Fixture::new();
    let config = InstallationConfig::from_file(&fixture.installation.file_path()).unwrap();
    let installation = Arc::new(Installation::from_config(config.clone()).await.unwrap());
    let front = FrontDoor::new(installation.clone(), Some(config.front_door_socket.clone()));
    let listener = front.listen().unwrap();
    let stream = tokio::net::UnixStream::connect(&config.front_door_socket)
        .await
        .unwrap();
    let channel = transport::channel_from_single_io(
        tonic::transport::Endpoint::from_static("http://localhost"),
        "config split",
        GrpcIo::new(stream),
    );
    let mut directory = wire::profile_service_client::ProfileServiceClient::new(channel);
    let profiles = directory
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles;
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, fixture.id.to_string());
    assert!(profiles[0].available);
    assert_eq!(
        profiles[0].socket_path,
        fixture.paths.socket_path.to_string_lossy()
    );
    println!(
        "ListProfiles over {}: {profiles:#?}",
        config.front_door_socket.display()
    );
    let stream = tokio::net::UnixStream::connect(&profiles[0].socket_path)
        .await
        .unwrap();
    let channel = transport::channel_from_single_io(
        tonic::transport::Endpoint::from_static("http://localhost"),
        "profile",
        GrpcIo::new(stream),
    );
    let agents = wire::client_service_client(channel)
        .list_agents(wire::ListAgentsRequest {})
        .await
        .unwrap()
        .into_inner();
    println!("ListAgents over discovered socket: {agents:#?}");
    let created = installation
        .create(OperationId::new(), Some("second".into()))
        .await
        .unwrap();
    assert!(created.available, "{created:?}");
    let second_path = config
        .root
        .join("profiles")
        .join(created.record.id.to_string())
        .join("config.yaml");
    let second = load_profile_config(&second_path).unwrap();
    println!(
        "Created profile configuration at {}:\n{}",
        second_path.display(),
        std::fs::read_to_string(&second_path).unwrap()
    );
    assert_eq!(second.profile_id, created.record.id);
    assert_eq!(second.installation.host_name, "shared-device");
    assert!(
        !std::fs::read_to_string(&second_path)
            .unwrap()
            .contains("enable_cloud_mode")
    );
    installation.stop(ShutdownReason::UserRequested).await;
    listener.stop().await;
    drop(front);
    drop(directory);
    drop(installation);
    assert!(!fixture.paths.socket_path.exists());
    assert!(!config.front_door_socket.exists());
    let reopened = Installation::from_config(config).await.unwrap();
    assert_eq!(reopened.profiles().len(), 2);
    assert!(reopened.profiles().iter().all(|profile| profile.available));
    reopened.shutdown(ShutdownReason::UserRequested).await;
}

#[cfg(all(unix, feature = "local-agents"))]
#[tokio::test]
async fn config_split_daemon_owner_flushes_shutdown_and_releases_all_sockets() {
    use crate::installation::{FrontDoorClient, rpc};
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let fixture = Fixture::new();
        for via_rpc in [true, false] {
            let installation = Installation::from_config(fixture.installation.clone())
                .await
                .unwrap();
            let (shutdown, stopped) = tokio::sync::oneshot::channel();
            let daemon = tokio::spawn(installation.serve(async {
                let _ = stopped.await;
            }));
            let mut front = loop {
                match FrontDoorClient::connect(&fixture.installation.front_door_socket).await {
                    Ok(front) => break front,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            let profiles = front
                .profiles
                .list_profiles(rpc::ListProfilesRequest {})
                .await
                .unwrap()
                .into_inner()
                .profiles;
            assert_eq!(profiles.len(), 1);
            assert_eq!(profiles[0].id, fixture.id.to_string());
            assert!(profiles[0].available);
            let client = crate::Server::builder()
                .config(Config {
                    socket_path: PathBuf::from(&profiles[0].socket_path),
                    ..Config::default()
                })
                .daemon()
                .open()
                .await
                .unwrap();
            client.list_agents().await.unwrap();
            if via_rpc {
                front
                    .installation
                    .shutdown(rpc::InstallationShutdownRequest {
                        operation_id: uuid::Uuid::new_v4().to_string(),
                    })
                    .await
                    .unwrap();
                drop(shutdown);
            } else {
                shutdown.send(()).unwrap();
            }
            daemon.await.unwrap().unwrap();
            assert!(client.list_agents().await.is_err());
            assert!(!fixture.paths.socket_path.exists());
            assert!(!fixture.installation.front_door_socket.exists());
            Registry::open(InstallationRoot::OnDisk(fixture.installation.root.clone())).unwrap();
        }
    })
    .await
    .unwrap();
}

#[test]
fn config_split_setup_writes_only_installation_preferences() {
    let fixture = Fixture::new();
    let before = std::fs::read(fixture.paths.config_path.clone().unwrap()).unwrap();
    let mut selected = Config {
        path: fixture.paths.config_path.clone(),
        ..Config::default()
    };
    crate::setup::set_prevent_idle_sleep(&mut selected, true).unwrap();
    assert_eq!(
        fixture.load().unwrap().installation.prevent_idle_sleep,
        Some(true)
    );
    crate::setup::clear_prevent_idle_sleep(&mut selected).unwrap();
    assert_eq!(
        fixture.load().unwrap().installation.prevent_idle_sleep,
        None
    );
    assert_eq!(
        std::fs::read(fixture.paths.config_path.unwrap()).unwrap(),
        before
    );
}

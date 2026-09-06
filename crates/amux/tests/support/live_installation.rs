//! Isolated installation configuration and administration for live process fixtures.

use std::path::{Path, PathBuf};

use amux::installation::{
    FrontDoorClient, InstallationRoot, OperationId, ProfileId, ProfileLabel, ProfilePaths,
    Registry, rpc,
};
use amux::{Config, InstallationConfig, ProfileConfig};
use anyhow::Result;

pub fn configure(root: &Path, name: &str) -> Result<(Config, PathBuf)> {
    let root = root.canonicalize()?;
    let id = ProfileId::new();
    let mut registry = Registry::open(InstallationRoot::OnDisk(root.clone()))?;
    registry.create(id, ProfileLabel::default())?;
    drop(registry);
    let paths = ProfilePaths::for_id(&root, id)?;
    let installation_path = root.join("installation.yaml");
    let installation = InstallationConfig {
        root: root.clone(),
        front_door_socket: root.join("amux.sock"),
        host_name: name.into(),
        prevent_idle_sleep: Some(false),
        keymaps_dir: root.join("keymaps"),
        ..Default::default()
    };
    std::fs::write(&installation_path, serde_yaml::to_string(&installation)?)?;
    let profile = ProfileConfig {
        installation_config: installation_path,
        socket_path: paths.socket_path.clone(),
        state_path: paths.state_path.clone(),
        data_dir: paths.data_dir.clone(),
        cloud_url: Config::default().cloud_url,
        tcp_port: None,
    };
    let path = paths.config_path.unwrap();
    std::fs::write(&path, serde_yaml::to_string(&profile)?)?;
    let config = Config {
        host_name: name.into(),
        socket_path: paths.socket_path,
        state_path: paths.state_path,
        data_dir: paths.data_dir,
        path: Some(path.clone()),
        prevent_idle_sleep: Some(false),
        ..Default::default()
    };
    Ok((config, path))
}

pub async fn shutdown(root: &Path) -> Result<()> {
    FrontDoorClient::connect(&root.join("amux.sock"))
        .await?
        .installation
        .shutdown(rpc::InstallationShutdownRequest {
            operation_id: OperationId::new().0.to_string(),
        })
        .await?;
    Ok(())
}

pub async fn suspend(root: &Path) -> Result<u64> {
    let response = FrontDoorClient::connect(&root.join("amux.sock"))
        .await?
        .installation
        .suspend_all(rpc::SuspendAllRequest {
            operation_id: OperationId::new().0.to_string(),
            reason: rpc::SuspendReason::User as i32,
        })
        .await?
        .into_inner();
    Ok(response.profiles.iter().map(|p| p.suspended_count).sum())
}

pub async fn resume(root: &Path) -> Result<(u64, u64)> {
    let response = FrontDoorClient::connect(&root.join("amux.sock"))
        .await?
        .installation
        .resume_all(rpc::ResumeAllRequest {
            operation_id: OperationId::new().0.to_string(),
        })
        .await?
        .into_inner();
    Ok((
        response.profiles.iter().map(|p| p.resumed_count).sum(),
        response.profiles.iter().map(|p| p.failed_count).sum(),
    ))
}

use amux::{ConnectPolicy, DaemonOptions};
use anyhow::{Context, Result};
use std::path::Path;

pub(super) fn daemon_options() -> Result<DaemonOptions> {
    let executable =
        std::env::current_exe().context("failed to determine current executable path")?;
    Ok(DaemonOptions::new(executable))
}

pub(super) fn cli_daemon_policy() -> Result<ConnectPolicy> {
    Ok(ConnectPolicy::SpawnDaemon(daemon_options()?))
}

/// Check the update marker file and print a banner if a valid update is available.
/// The marker is ignored (and deleted) if its current_version doesn't match our binary.
pub(super) fn print_update_banner(state_path: &Path) {
    if let Some(minimum_version) = amux::update::read_upgrade_required(state_path) {
        let current = env!("CARGO_PKG_VERSION");
        println!(
            "An update is REQUIRED for cloud connectivity (minimum v{minimum_version}, \
             you have v{current}). Run 'amux update' to update."
        );
        return;
    }

    let marker_path = amux::update::update_marker_path(state_path);
    match amux::update::read_update_marker(&marker_path) {
        Some(info) if info.current_version == env!("CARGO_PKG_VERSION") => {
            println!(
                "An update is available ({} vs {}). Run 'amux update' to update.",
                info.update_version, info.current_version
            );
            println!("Updating will suspend and resume all running agents.");
        }
        Some(_) => {
            let _ = std::fs::remove_file(&marker_path);
        }
        None => {}
    }
}

//! Cloud relay startup and profile diagnostics.

use std::sync::Arc;

use amux::{Config, DebugFormat, Server};
use anyhow::{Result, anyhow};

use crate::client_common::{
    open_daemon, remove_stale_socket, server_unavailable_error, spawn_relay,
};
use crate::update::MarkerFileReporter;

pub async fn start_relay(config: &Config, foreground: bool) -> Result<()> {
    match open_daemon(config).await {
        Ok(_) => {
            println!("Server already running.");
            return Ok(());
        }
        Err(error) if server_unavailable_error(config, &error) => {
            remove_stale_socket(config, &error)
        }
        Err(error) => return Err(error.into()),
    }
    config.validate()?;
    if foreground {
        run_relay_foreground(config.clone()).await
    } else {
        spawn_relay(config).await?;
        println!("Server started.");
        Ok(())
    }
}

pub(crate) async fn run_relay_foreground(config: Config) -> Result<()> {
    let reporter = Arc::new(MarkerFileReporter::from_state_path(&config.state_path));
    Server::builder()
        .config(config)
        .update_reporter(reporter.clone())
        .subscription_reporter(reporter)
        .as_cloud_relay()
        .run()
        .await?;
    Ok(())
}

pub async fn debug(config: &Config, verbose: bool, format: DebugFormat) -> Result<String> {
    let admin = crate::front_door::profile_admin(config, None)
        .await
        .map_err(|error| anyhow!("cannot inspect profile: {error}"))?;
    Ok(admin.debug_dump_verbose(verbose, format).await?)
}

pub(crate) async fn server_is_running(config: &Config) -> bool {
    let Ok(installation) = crate::front_door::configuration(config.path.as_deref()) else {
        return false;
    };
    matches!(
        crate::front_door::existing(&installation).await,
        Ok(Some(_))
    )
}

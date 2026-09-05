//! Desktop daemon startup and discovery use the installation socket.

use std::io::{ErrorKind, Write};
use std::path::Path;
use std::time::Duration;

use amux::InstallationConfig;
use amux::installation::{FrontDoorClient, Installation, rpc};
use anyhow::{Context, Result, anyhow};

/// AMUX_CONFIG identifies a profile; the default path identifies the installation.
pub fn configuration(profile_path: Option<&Path>) -> Result<InstallationConfig> {
    match profile_path {
        Some(path) => Ok(amux::load_profile_config(path)?.installation),
        None => InstallationConfig::from_file(&InstallationConfig::default_path())
            .context("cannot load installation; run `amux init` first"),
    }
}

pub async fn existing(config: &InstallationConfig) -> Result<Option<FrontDoorClient>> {
    match FrontDoorClient::connect(&config.front_door_socket).await {
        Ok(mut front) => {
            front
                .installation
                .get_info(rpc::GetInfoRequest {})
                .await
                .context("socket does not serve an amux installation")?;
            Ok(Some(front))
        }
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn start(config: InstallationConfig, foreground: bool) -> Result<()> {
    if existing(&config).await?.is_some() {
        println!("Server already running.");
        return Ok(());
    }
    if foreground {
        run(config).await
    } else {
        let executable = std::env::current_exe()?;
        spawn(&config, &executable).await?;
        println!("Server started.");
        Ok(())
    }
}

pub async fn run(config: InstallationConfig) -> Result<()> {
    let installation = Installation::from_config(config).await?;
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        installation
            .serve(async {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = terminate.recv() => {},
                }
            })
            .await?;
    }
    #[cfg(not(unix))]
    {
        let _ = installation;
        anyhow::bail!("installation sockets are not supported on this platform");
    }
    Ok(())
}

pub async fn spawn(config: &InstallationConfig, executable: &Path) -> Result<FrontDoorClient> {
    let stderr_path = config.root.join("amux-startup-stderr.log");
    let mut command = crate::client_common::daemon_command(
        executable,
        false,
        config.path.as_deref(),
        Some(&stderr_path),
    );
    let mut child = command
        .spawn()
        .context("failed to start installation daemon")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(serde_yaml::to_string(config)?.as_bytes())?;
    }
    let result = async {
        for _ in 0..100 {
            // Check the child before accepting an existing listener: a concurrent
            // starter must not hide this child's validation or root-lock failure.
            if let Some(status) = child.try_wait()? {
                return Err(crate::client_common::startup_exit_error(
                    status,
                    &stderr_path,
                ));
            }
            if let Some(front) = existing(config).await? {
                return Ok(front);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!(
            "installation did not start within 10s; inspect {}",
            stderr_path.display()
        ))
    }
    .await;
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    } else {
        let _ = std::fs::remove_file(stderr_path);
    }
    result
}

pub async fn stop(config: &InstallationConfig) -> Result<()> {
    let Some(mut front) = existing(config).await? else {
        println!("No server running.");
        return Ok(());
    };
    front
        .installation
        .shutdown(rpc::InstallationShutdownRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
        })
        .await?;
    // Wait for socket cleanup and release of the installation lock before a
    // subsequent start is allowed to race the old process's final teardown.
    tokio::time::timeout(Duration::from_secs(10), async {
        while config.front_door_socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("installation did not finish shutting down within 10s")?;
    println!("Server shutting down.");
    Ok(())
}

pub async fn list(config: &InstallationConfig) -> Result<()> {
    let mut front = existing(config)
        .await?
        .ok_or_else(|| anyhow!("No server running. Start it with `amux server start`."))?;
    let profiles = front
        .profiles
        .list_profiles(rpc::ListProfilesRequest {})
        .await?
        .into_inner()
        .profiles;
    println!("Profiles:");
    for profile in profiles {
        let status = if !profile.startup_error.is_empty() {
            profile.startup_error.as_str()
        } else if profile.available {
            "ready"
        } else {
            "unavailable"
        };
        println!(
            "  {}  {}  {}  {}",
            profile.id, profile.label, profile.email, status
        );
    }
    Ok(())
}

pub async fn suspend(config: &InstallationConfig) -> Result<()> {
    let Some(mut front) = existing(config).await? else {
        println!("No server running.");
        return Ok(());
    };
    let report = front
        .installation
        .suspend_all(rpc::SuspendAllRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            reason: rpc::SuspendReason::User as i32,
        })
        .await?
        .into_inner();
    println!(
        "Suspended {} agent(s).",
        report
            .profiles
            .iter()
            .map(|p| p.suspended_count)
            .sum::<u64>()
    );
    Ok(())
}

pub async fn resume(config: &InstallationConfig) -> Result<()> {
    let mut front = match existing(config).await? {
        Some(front) => front,
        None => spawn(config, &std::env::current_exe()?).await?,
    };
    let report = front
        .installation
        .resume_all(rpc::ResumeAllRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
        })
        .await?
        .into_inner();
    let resumed: u64 = report.profiles.iter().map(|p| p.resumed_count).sum();
    let failed: u64 = report.profiles.iter().map(|p| p.failed_count).sum();
    print!("Resumed {resumed} agent(s).");
    if failed > 0 {
        print!(" ({failed} failed to resume)");
    }
    println!();
    Ok(())
}

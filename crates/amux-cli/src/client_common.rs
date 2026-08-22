use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use amux::{Client, Config, ConnectError, Server, TransportError};
use anyhow::{Context, Result, anyhow};

pub(super) async fn get_client(config: &Config) -> Result<Client> {
    let executable =
        std::env::current_exe().context("failed to determine current executable path")?;
    get_client_with_executable(config, executable.as_path()).await
}

pub(super) async fn require_running_client(
    config: &Config,
    retry_command: Option<&str>,
) -> Result<Client> {
    match open_daemon(config).await {
        Ok(client) => Ok(client),
        Err(error) if server_unavailable_error(config, &error) => {
            remove_stale_socket(config, &error);
            Err(anyhow!(server_not_running_message(retry_command)))
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn get_client_with_executable(
    config: &Config,
    executable: &Path,
) -> Result<Client> {
    match open_daemon(config).await {
        Ok(client) => return Ok(client),
        Err(error) if server_unavailable_error(config, &error) => {
            remove_stale_socket(config, &error);
        }
        Err(error) => return Err(error.into()),
    }

    spawn_daemon_and_connect(config, executable, false).await
}

pub(super) async fn spawn_daemon(config: &Config, cloud: bool) -> Result<Client> {
    let executable =
        std::env::current_exe().context("failed to determine current executable path")?;
    spawn_daemon_and_connect(config, executable.as_path(), cloud).await
}

pub(super) async fn open_daemon(config: &Config) -> std::result::Result<Client, ConnectError> {
    Server::builder()
        .config(config.clone())
        .daemon()
        .open()
        .await
}

async fn spawn_daemon_and_connect(
    config: &Config,
    executable: &Path,
    cloud: bool,
) -> Result<Client> {
    config.validate(cloud)?;

    tracing::info!(cloud, "starting server");

    let config_yaml =
        serde_yaml::to_string(config).context("failed to serialize config for daemon")?;
    let startup_stderr_path = startup_stderr_path(config);
    let mut cmd = daemon_command(executable, cloud, Some(&startup_stderr_path));
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start server via {}", executable.display()))?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config_yaml.as_bytes());
    }

    let client = wait_for_server_connection(config, &mut child, &startup_stderr_path).await?;
    let _ = std::fs::remove_file(&startup_stderr_path);
    Ok(client)
}

fn daemon_command(
    executable: &Path,
    cloud: bool,
    startup_stderr_path: Option<&Path>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(executable);
    let stderr = startup_stderr_path
        .and_then(open_startup_stderr_file)
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);
    cmd.arg("server")
        .arg("start")
        .arg("--foreground")
        .arg("--config-from-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(stderr);
    if cloud {
        cmd.arg("--cloud");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008);
    }
    cmd
}

async fn wait_for_server_connection(
    config: &Config,
    child: &mut std::process::Child,
    startup_stderr_path: &Path,
) -> Result<Client> {
    for _ in 0..50 {
        match open_daemon(config).await {
            Ok(client) => return Ok(client),
            Err(ConnectError::Transport(TransportError::Io(_))) => {
                if let Some(status) = child
                    .try_wait()
                    .context("failed to inspect server process")?
                {
                    return Err(startup_exit_error(status, startup_stderr_path));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }

    if let Some(status) = child
        .try_wait()
        .context("failed to inspect server process")?
    {
        return Err(startup_exit_error(status, startup_stderr_path));
    }

    let log_path = std::env::var("AMUX_LOG")
        .unwrap_or_else(|_| amux::default_log_path().display().to_string());
    Err(anyhow!(startup_timeout_error(
        &log_path,
        startup_stderr_path
    )))
}

pub(super) fn server_unavailable_error(config: &Config, error: &ConnectError) -> bool {
    match error {
        #[cfg(unix)]
        ConnectError::Transport(TransportError::Io(e))
            if config.socket_path.exists() && is_server_unavailable(e) =>
        {
            true
        }
        ConnectError::Transport(TransportError::Io(e)) if is_server_unavailable(e) => true,
        _ => false,
    }
}

pub(super) fn remove_stale_socket(config: &Config, error: &ConnectError) {
    #[cfg(unix)]
    if let ConnectError::Transport(TransportError::Io(e)) = error
        && config.socket_path.exists()
        && is_server_unavailable(e)
    {
        tracing::warn!(error = %e, "stale local socket detected, removing");
        let _ = std::fs::remove_file(&config.socket_path);
    }
}

fn is_server_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn server_not_running_message(retry_command: Option<&str>) -> String {
    let mut message =
        "amux server is not running.\n\nStart it with:\n  amux server start".to_string();
    if let Some(command) = retry_command {
        message.push_str("\n\nThen run:\n  ");
        message.push_str(command);
    }
    message
}

fn startup_stderr_path(config: &Config) -> PathBuf {
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("amux-startup-stderr.log")
}

fn open_startup_stderr_file(path: &Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to create startup stderr directory");
        return None;
    }

    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        Ok(file) => Some(file),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to open startup stderr log");
            None
        }
    }
}

fn startup_exit_error(
    status: std::process::ExitStatus,
    startup_stderr_path: &Path,
) -> anyhow::Error {
    let mut message = format!(
        "server exited before accepting connections ({})",
        exit_status_summary(status)
    );
    if let Some(stderr) = read_startup_stderr(startup_stderr_path) {
        message.push_str(":\n");
        message.push_str(&stderr);
    }
    anyhow!(message)
}

fn startup_timeout_error(log_path: &str, startup_stderr_path: &Path) -> String {
    let mut message = format!("server failed to start within 5s - check {log_path} for details");
    if let Some(stderr) = read_startup_stderr(startup_stderr_path) {
        message.push_str("\n\nstartup stderr:\n");
        message.push_str(&stderr);
    }
    message
}

fn exit_status_summary(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    }
}

fn read_startup_stderr(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    format_startup_diagnostics(&contents)
}

fn format_startup_diagnostics(contents: &str) -> Option<String> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Check the update marker file and print a banner if a valid update is available.
/// The marker is ignored (and deleted) if its current_version doesn't match our binary.
pub(super) fn print_update_banner(state_path: &Path) {
    let reporter = crate::update::MarkerFileReporter::from_state_path(state_path);
    if reporter.subscription_required() {
        println!(
            "Cloud subscription required — manage at amux.sh/account; local agents remain available."
        );
    }
    let current = env!("CARGO_PKG_VERSION");
    if let Some(minimum_version) = reporter.read_active_update_required(current) {
        println!(
            "An update is REQUIRED for cloud connectivity (minimum v{minimum_version}, \
             you have v{current}). Run 'amux update' to update."
        );
        return;
    }

    match reporter.read_update_marker() {
        Some(info) if info.current_version == env!("CARGO_PKG_VERSION") => {
            println!(
                "An update is available ({} vs {}). Run 'amux update' to update.",
                info.update_version, info.current_version
            );
            println!("Updating will suspend and resume all running agents.");
        }
        Some(_) => {
            reporter.clear_update_marker();
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{format_startup_diagnostics, server_not_running_message};

    #[test]
    fn server_not_running_message_can_include_retry_command() {
        assert_eq!(
            server_not_running_message(Some("amux pair --connect")),
            "amux server is not running.\n\nStart it with:\n  amux server start\n\nThen run:\n  amux pair --connect"
        );
        assert_eq!(
            server_not_running_message(None),
            "amux server is not running.\n\nStart it with:\n  amux server start"
        );
    }

    #[test]
    fn trims_empty_startup_diagnostics() {
        assert_eq!(format_startup_diagnostics(" \n\t "), None);
        assert_eq!(
            format_startup_diagnostics("\nError: startup failed\n"),
            Some("Error: startup failed".to_string())
        );
    }
}

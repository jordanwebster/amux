use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

#[cfg(unix)]
pub(crate) async fn ensure_authenticated() -> Result<()> {
    use codex_sdk::{AccountReadParams, CodexConfig, connect_daemon, ensure_daemon};

    let codex_home = codex_home()?;
    let _daemon = ensure_daemon(&codex_home)
        .await
        .context("failed to start the Codex app-server for authentication preflight")?;
    let codex = connect_daemon(
        &codex_home,
        CodexConfig {
            client_name: "amux".to_string(),
            client_title: Some("amux".to_string()),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            ..CodexConfig::default()
        },
    )
    .await
    .context("failed to connect to the Codex app-server for authentication preflight")?;
    let response = codex
        .read_account(AccountReadParams::default())
        .await
        .context("failed to read the Codex account")?;
    codex.close().await;
    require_account(response.account.is_some(), response.requires_openai_auth)
}

#[cfg(not(unix))]
pub(crate) async fn ensure_authenticated() -> Result<()> {
    Err(anyhow!("Codex agents are supported only on Unix platforms"))
}

#[cfg(unix)]
fn codex_home() -> Result<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| anyhow!("CODEX_HOME or HOME is required for Codex agents"))
}

fn require_account(has_account: bool, requires_openai_auth: bool) -> Result<()> {
    if has_account || !requires_openai_auth {
        Ok(())
    } else {
        Err(anyhow!(
            "Codex is not authenticated; run `codex login` and try again"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthenticated_preflight_names_the_recovery_command() {
        let error = require_account(false, true).unwrap_err().to_string();
        assert!(error.contains("codex login"));
    }

    #[test]
    fn account_or_auth_optional_server_passes_preflight() {
        assert!(require_account(true, true).is_ok());
        assert!(require_account(false, false).is_ok());
    }
}

use crate::config::Config;
use crate::oauth;
use crate::state::{CloudState, State, StateError};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct CloudSetupState {
    pub use_cloud_mode: Option<bool>,
    pub has_refresh_token: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ClaudePluginSetupState {
    pub applied_plugin_version: Option<String>,
    pub applied_marketplace_path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("oauth error: {0}")]
    OAuth(#[from] oauth::OAuthError),
}

/// Check whether cloud onboarding is incomplete.
pub fn needs_init(config: &Config) -> bool {
    let state = State::load(&config.state_path).unwrap_or_default();
    match state.cloud.use_cloud_mode {
        None => true,
        Some(true) => state.cloud.refresh_token.is_none(),
        Some(false) => false,
    }
}

/// Read current cloud onboarding state.
pub fn cloud_setup_state(config: &Config) -> Result<CloudSetupState, SetupError> {
    let state = State::load(&config.state_path)?;
    Ok(CloudSetupState {
        use_cloud_mode: state.cloud.use_cloud_mode,
        has_refresh_token: state.cloud.refresh_token.is_some(),
    })
}

/// Reset cloud onboarding fields in persistent state.
pub fn reset_cloud_state(config: &Config) -> Result<(), SetupError> {
    State::update(&config.state_path, |s| {
        s.cloud = CloudState::default();
    })?;
    Ok(())
}

/// Persist cloud mode preference.
pub fn set_use_cloud_mode(config: &Config, use_cloud_mode: bool) -> Result<(), SetupError> {
    State::update(&config.state_path, |s| {
        s.cloud.use_cloud_mode = Some(use_cloud_mode);
    })?;
    Ok(())
}

/// Run OAuth device flow and persist refresh token.
pub async fn authenticate_cloud(config: &Config) -> Result<(), SetupError> {
    let refresh_token = oauth::device_flow(&config.cloud_url).await?;
    State::update(&config.state_path, |s| {
        s.cloud.refresh_token = Some(refresh_token);
    })?;
    Ok(())
}

/// Read Claude plugin setup state persisted for Claude Code integration.
pub fn claude_plugin_setup_state() -> ClaudePluginSetupState {
    let state_path = State::default_path();
    State::load(&state_path)
        .ok()
        .map(|s| ClaudePluginSetupState {
            applied_plugin_version: s.claude.applied_plugin_version,
            applied_marketplace_path: s.claude.applied_marketplace_path,
        })
        .unwrap_or_default()
}

/// Persist the Claude plugin version and marketplace path last successfully
/// applied to Claude Code.
pub fn set_claude_plugin_setup_state(
    version: &str,
    marketplace_path: &Path,
) -> Result<(), SetupError> {
    let state_path = State::default_path();
    State::update(&state_path, |s| {
        s.claude.applied_plugin_version = Some(version.to_string());
        s.claude.applied_marketplace_path = Some(marketplace_path.to_path_buf());
    })?;
    Ok(())
}

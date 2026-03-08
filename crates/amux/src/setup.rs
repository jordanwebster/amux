use crate::config::Config;
use crate::oauth;
use crate::state::{CloudState, State, StateError};

#[derive(Debug)]
pub struct CloudSetupState {
    pub use_cloud_mode: Option<bool>,
    pub has_refresh_token: bool,
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

/// Current Claude plugin version tracked in state (if present).
pub fn claude_plugin_version() -> Option<u32> {
    let state_path = State::default_path();
    State::load(&state_path)
        .ok()
        .and_then(|s| s.claude.plugin_version)
}

/// Persist Claude plugin version to state.
pub fn set_claude_plugin_version(version: u32) -> Result<(), SetupError> {
    let state_path = State::default_path();
    State::update(&state_path, |s| {
        s.claude.plugin_version = Some(version);
    })?;
    Ok(())
}

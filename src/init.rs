//! Initialization flow for amux.
//!
//! Handles first-time setup including:
//! - Cloud mode configuration (yes/no prompt)
//! - OAuth device flow authentication

use crate::config::Config;
use crate::oauth;
use crate::state::{CloudState, State, StateError};
use std::io::{self, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InitError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("State error: {0}")]
    State(#[from] StateError),
    #[error("OAuth error: {0}")]
    OAuth(#[from] oauth::OAuthError),
}

/// Check if initialization is needed.
///
/// Returns true if:
/// - Cloud mode has not been configured yet (use_cloud_mode is None)
/// - Cloud mode is enabled but no refresh token is stored
pub fn needs_init(config: &Config) -> bool {
    let state = State::load(&config.state_path).unwrap_or_default();

    match state.cloud.use_cloud_mode {
        None => true,                                      // Not configured yet
        Some(true) => state.cloud.refresh_token.is_none(), // Cloud mode but no token
        Some(false) => false,                              // Local mode, all set
    }
}

/// Run the initialization flow.
///
/// If reset is true, clears existing state first.
pub async fn run_init(config: &Config, reset: bool) -> Result<(), InitError> {
    let state_path = &config.state_path;

    // If reset, clear existing cloud state
    if reset {
        State::update(state_path, |s| {
            s.cloud = CloudState::default();
        })?;
        println!("State cleared.");
    }

    let state = State::load(state_path)?;

    // Step 1: Ask about cloud mode if not set
    if state.cloud.use_cloud_mode.is_none() {
        println!();
        println!("amux can connect your local machine to the cloud, allowing you to");
        println!("access your agents from anywhere (mobile, web, other machines).");
        println!();
        println!("Do you want to enable cloud mode?");
        println!("  1. Yes (recommended)");
        println!("  2. No (local only)");
        print!("\nChoice [1]: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let choice = input.trim();

        let use_cloud = choice.is_empty() || choice == "1";

        State::update(state_path, |s| {
            s.cloud.use_cloud_mode = Some(use_cloud);
        })?;

        if !use_cloud {
            println!("\nCloud mode disabled. You can run 'amux init' anytime to reconfigure.");
            return Ok(());
        }
    }

    // Step 2: OAuth device flow if cloud mode enabled but no token
    let state = State::load(state_path)?;
    if state.cloud.use_cloud_mode == Some(true) && state.cloud.refresh_token.is_none() {
        println!("\nStarting authentication...");
        let refresh_token = oauth::device_flow(&config.cloud_url).await?;

        State::update(state_path, |s| {
            s.cloud.refresh_token = Some(refresh_token);
        })?;

        println!("\nAuthentication successful!");
        println!("Your local amux server will now connect to the cloud automatically.");
    }

    Ok(())
}

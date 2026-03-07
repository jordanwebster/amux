//! Initialization flow for amux.
//!
//! Handles first-time setup including:
//! - Cloud mode configuration (yes/no prompt)
//! - OAuth device flow authentication

use amux::config::Config;
use amux::oauth;
use amux::state::{CloudState, State, StateError};
use std::io::{self, Write};

#[derive(Debug)]
pub enum InitError {
    Io(io::Error),
    State(StateError),
    OAuth(oauth::OAuthError),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::Io(e) => write!(f, "IO error: {}", e),
            InitError::State(e) => write!(f, "State error: {}", e),
            InitError::OAuth(e) => write!(f, "OAuth error: {}", e),
        }
    }
}

impl std::error::Error for InitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InitError::Io(e) => Some(e),
            InitError::State(e) => Some(e),
            InitError::OAuth(e) => Some(e),
        }
    }
}

impl From<io::Error> for InitError {
    fn from(e: io::Error) -> Self {
        InitError::Io(e)
    }
}

impl From<StateError> for InitError {
    fn from(e: StateError) -> Self {
        InitError::State(e)
    }
}

impl From<oauth::OAuthError> for InitError {
    fn from(e: oauth::OAuthError) -> Self {
        InitError::OAuth(e)
    }
}

/// Check if initialization is needed.
pub fn needs_init(config: &Config) -> bool {
    let state = State::load(&config.state_path).unwrap_or_default();

    match state.cloud.use_cloud_mode {
        None => true,
        Some(true) => state.cloud.refresh_token.is_none(),
        Some(false) => false,
    }
}

/// Run the initialization flow.
pub async fn run_init(config: &Config, reset: bool) -> Result<(), InitError> {
    let state_path = &config.state_path;

    if reset {
        State::update(state_path, |s| {
            s.cloud = CloudState::default();
        })?;
        println!("State cleared.");
    }

    let state = State::load(state_path)?;

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

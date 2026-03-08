//! Initialization flow for amux.
//!
//! Handles first-time setup including:
//! - Cloud mode configuration (yes/no prompt)
//! - OAuth device flow authentication

use amux::Config;
use amux::setup;
use amux::setup::SetupError;
use std::io::{self, Write};

#[derive(Debug)]
pub enum InitError {
    Io(io::Error),
    Setup(SetupError),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::Io(e) => write!(f, "IO error: {}", e),
            InitError::Setup(e) => write!(f, "Setup error: {}", e),
        }
    }
}

impl std::error::Error for InitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InitError::Io(e) => Some(e),
            InitError::Setup(e) => Some(e),
        }
    }
}

impl From<io::Error> for InitError {
    fn from(e: io::Error) -> Self {
        InitError::Io(e)
    }
}

impl From<SetupError> for InitError {
    fn from(e: SetupError) -> Self {
        InitError::Setup(e)
    }
}

/// Check if initialization is needed.
pub fn needs_init(config: &Config) -> bool {
    setup::needs_init(config)
}

/// Run the initialization flow.
pub async fn run_init(config: &Config, reset: bool) -> Result<(), InitError> {
    if reset {
        setup::reset_cloud_state(config)?;
        println!("State cleared.");
    }

    let mut status = setup::cloud_setup_state(config)?;

    if status.use_cloud_mode.is_none() {
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

        setup::set_use_cloud_mode(config, use_cloud)?;

        if !use_cloud {
            println!("\nCloud mode disabled. You can run 'amux init' anytime to reconfigure.");
            return Ok(());
        }

        status = setup::cloud_setup_state(config)?;
    }

    if status.use_cloud_mode == Some(true) && !status.has_refresh_token {
        println!("\nStarting authentication...");
        setup::authenticate_cloud(config).await?;

        println!("\nAuthentication successful!");
        println!("Your local amux server will now connect to the cloud automatically.");
    }

    Ok(())
}

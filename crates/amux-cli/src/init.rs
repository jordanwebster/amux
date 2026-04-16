//! Initialization flow for amux.
//!
//! Handles first-time setup including:
//! - Cloud mode configuration (yes/no prompt)
//! - OAuth device flow authentication

use std::io::{self, Write};

use amux::setup::SetupError;
use amux::{Config, setup};

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
pub async fn run_init(config: &mut Config, reset: bool) -> Result<(), InitError> {
    if reset {
        setup::reset_cloud_state(config)?;
        println!("State cleared.");
    }

    let mut status = setup::cloud_setup_state(config)?;

    if matches!(status, setup::CloudState::NotConfigured) {
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
        }

        status = setup::cloud_setup_state(config)?;
    }

    if matches!(status, setup::CloudState::Unauthenticated) {
        println!("\nStarting authentication...");
        setup::authenticate_cloud(config).await?;

        println!("\nAuthentication successful!");
        println!("Your local amux server will now connect to the cloud automatically.");
    }

    maybe_prompt_prevent_idle_sleep(config)?;

    Ok(())
}

fn maybe_prompt_prevent_idle_sleep(config: &mut Config) -> Result<(), InitError> {
    if !needs_prevent_idle_sleep_setup(config) {
        return Ok(());
    }

    println!();
    println!("To keep your agents reachable remotely, amux can keep this machine");
    println!("awake while it runs in the background.");
    println!();
    println!("This prevents idle sleep, but the display can still sleep.");
    println!("On laptops, this may use more battery.");
    println!();
    println!("Do you want amux to keep this machine awake?");
    println!("  1. Yes (recommended for remote access)");
    println!("  2. No");
    let enabled = prompt_prevent_idle_sleep_choice()?;

    setup::set_prevent_idle_sleep(config, enabled)?;
    config.prevent_idle_sleep = enabled;

    if !enabled {
        println!();
        println!("Remote access will stop when this machine goes to sleep.");
        println!(
            "You can change this later by setting `prevent_idle_sleep: true` in your amux config."
        );
    }

    Ok(())
}

fn prompt_prevent_idle_sleep_choice() -> Result<bool, InitError> {
    loop {
        print!("\nChoice [1]: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        match parse_prevent_idle_sleep_choice(&input) {
            Some(enabled) => return Ok(enabled),
            None => {
                println!("Please enter 1 or 2.");
            }
        }
    }
}

fn needs_prevent_idle_sleep_setup(config: &Config) -> bool {
    setup::prevent_idle_sleep_supported()
        && matches!(setup::prevent_idle_sleep_preference(config), Ok(None))
}

fn parse_prevent_idle_sleep_choice(input: &str) -> Option<bool> {
    let choice = input.trim().to_ascii_lowercase();
    match choice.as_str() {
        "" | "1" | "y" | "yes" => Some(true),
        "2" | "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use amux::{Config, setup};
    use tempfile::tempdir;

    use super::{needs_init, parse_prevent_idle_sleep_choice};

    #[test]
    fn needs_init_when_prevent_idle_sleep_is_unset() {
        if !setup::prevent_idle_sleep_supported() {
            return;
        }

        let dir = tempdir().unwrap();
        let config = Config {
            path: Some(dir.path().join("config.yaml")),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };
        setup::set_use_cloud_mode(&config, false).unwrap();

        assert!(needs_init(&config));
    }

    #[test]
    fn sleep_preference_completion_does_not_force_reinit() {
        if !setup::prevent_idle_sleep_supported() {
            return;
        }

        let dir = tempdir().unwrap();
        let config = Config {
            path: Some(dir.path().join("config.yaml")),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };
        setup::set_use_cloud_mode(&config, false).unwrap();
        setup::set_prevent_idle_sleep(&config, false).unwrap();

        assert!(!needs_init(&config));
    }

    #[test]
    fn prevent_idle_sleep_choice_parsing_is_conservative() {
        assert_eq!(parse_prevent_idle_sleep_choice(""), Some(true));
        assert_eq!(parse_prevent_idle_sleep_choice("1"), Some(true));
        assert_eq!(parse_prevent_idle_sleep_choice("yes"), Some(true));
        assert_eq!(parse_prevent_idle_sleep_choice("n"), Some(false));
        assert_eq!(parse_prevent_idle_sleep_choice("2"), Some(false));
        assert_eq!(parse_prevent_idle_sleep_choice("maybe"), None);
    }
}

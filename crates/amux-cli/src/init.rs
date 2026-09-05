//! Initialization flow for amux.
//!
//! `run_init` is a state-machine loop driven by a pure `next_step` function:
//! `next_step` inspects the current `Config` + `State` and decides what piece
//! of setup (if any) still needs to happen. Each step function prompts the
//! user (or performs work), persists to disk, updates the in-memory `Config`,
//! and returns — then the loop re-evaluates.

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

/// Carries entry-point context through the init loop so individual steps can
/// gate on "was this triggered from explicit `amux init`, or implicitly from a
/// command-time precondition?". Today's steps don't consult `explicit`, but
/// the field is present so future preference prompts (e.g. "will you use
/// Claude?") can self-gate without touching call sites.
#[derive(Debug, Clone, Copy)]
pub struct InitContext {
    pub explicit: bool,
}

impl InitContext {
    pub fn explicit() -> Self {
        Self { explicit: true }
    }

    pub fn implicit() -> Self {
        Self { explicit: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitStep {
    EnsureDeviceIdentity,
    PromptIdleSleep,
    Done,
}

/// Choose the next local setup step. Cloud credentials are managed by login.
fn next_step(config: &Config, identity_ready: bool, _ctx: &InitContext) -> InitStep {
    if !identity_ready {
        return InitStep::EnsureDeviceIdentity;
    }
    if setup::prevent_idle_sleep_supported() && config.prevent_idle_sleep.is_none() {
        return InitStep::PromptIdleSleep;
    }
    InitStep::Done
}

/// True iff at least one init step would run given the current state.
pub fn needs_init(config: &Config) -> bool {
    needs_init_inner(config, setup::device_identity_ready(config))
}

fn needs_init_inner(config: &Config, identity_ready: bool) -> bool {
    next_step(config, identity_ready, &InitContext::implicit()) != InitStep::Done
}

/// Drive the init state machine to completion.
pub async fn run_init(config: &mut Config, ctx: InitContext, reset: bool) -> Result<(), InitError> {
    tracing::debug!(explicit = ctx.explicit, reset, "running init");

    if reset {
        setup::clear_prevent_idle_sleep(config)?;
        println!("Setup preferences reset.");
    }

    loop {
        match next_step(config, setup::device_identity_ready(config), &ctx) {
            InitStep::EnsureDeviceIdentity => setup::ensure_device_identity(config)?,
            InitStep::PromptIdleSleep => prompt_idle_sleep(config)?,
            InitStep::Done => return Ok(()),
        }
    }
}

fn prompt_idle_sleep(config: &mut Config) -> Result<(), InitError> {
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

    let enabled = loop {
        print!("\nChoice [1]: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        match parse_idle_sleep_choice(&input) {
            Some(v) => break v,
            None => println!("Please enter 1 or 2."),
        }
    };

    setup::set_prevent_idle_sleep(config, enabled)?;

    if !enabled {
        println!();
        println!("Remote access will stop when this machine goes to sleep.");
        println!(
            "You can change this later by setting `prevent_idle_sleep: true` in your amux config."
        );
    }
    Ok(())
}

fn parse_idle_sleep_choice(input: &str) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "" | "1" | "y" | "yes" => Some(true),
        "2" | "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use amux::{Config, setup};
    use tempfile::tempdir;

    use super::{InitContext, InitStep, needs_init_inner, next_step, parse_idle_sleep_choice};

    fn test_config(dir: &tempfile::TempDir) -> Config {
        Config {
            path: Some(dir.path().join("config.yaml")),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        }
    }

    #[test]
    fn next_step_missing_identity_wants_device_identity() {
        let config = Config::default();
        assert_eq!(
            next_step(&config, false, &InitContext::implicit()),
            InitStep::EnsureDeviceIdentity
        );
    }

    #[test]
    fn next_step_wants_idle_sleep_if_unset_and_supported() {
        if !setup::prevent_idle_sleep_supported() {
            return;
        }
        let config = Config::default();
        assert_eq!(
            next_step(&config, true, &InitContext::implicit()),
            InitStep::PromptIdleSleep
        );
    }

    #[test]
    fn config_split_init_without_credentials_needs_no_authentication() {
        let config = Config {
            prevent_idle_sleep: Some(false),
            ..Config::default()
        };
        assert_eq!(
            next_step(&config, true, &InitContext::implicit()),
            InitStep::Done
        );
    }

    #[test]
    fn next_step_explicit_flag_does_not_affect_todays_steps() {
        let config = Config::default();
        assert_eq!(
            next_step(&config, true, &InitContext::implicit()),
            next_step(&config, true, &InitContext::explicit())
        );
    }

    #[test]
    fn needs_init_false_when_everything_set() {
        let dir = tempdir().unwrap();
        let mut config = test_config(&dir);
        if setup::prevent_idle_sleep_supported() {
            setup::set_prevent_idle_sleep(&mut config, false).unwrap();
        }
        assert!(!needs_init_inner(&config, true));
    }

    #[test]
    fn needs_init_true_when_identity_is_missing() {
        let dir = tempdir().unwrap();
        let mut config = test_config(&dir);
        if setup::prevent_idle_sleep_supported() {
            setup::set_prevent_idle_sleep(&mut config, false).unwrap();
        }
        assert!(needs_init_inner(&config, false));
    }

    #[test]
    fn idle_sleep_choice_parsing_is_conservative() {
        assert_eq!(parse_idle_sleep_choice(""), Some(true));
        assert_eq!(parse_idle_sleep_choice("1"), Some(true));
        assert_eq!(parse_idle_sleep_choice("yes"), Some(true));
        assert_eq!(parse_idle_sleep_choice("n"), Some(false));
        assert_eq!(parse_idle_sleep_choice("2"), Some(false));
        assert_eq!(parse_idle_sleep_choice("maybe"), None);
    }
}

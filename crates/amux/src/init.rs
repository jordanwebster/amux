//! Initialization flow for amux.
//!
//! `run_init` is a state-machine loop driven by a pure `next_step` function:
//! `next_step` inspects the current `Config` + `State` and decides what piece
//! of setup (if any) still needs to happen. Each step function prompts the
//! user (or performs work), persists to disk, updates the in-memory `Config`,
//! and returns — then the loop re-evaluates.

use std::io::{self, BufRead, IsTerminal, Write};

use amux::connections::{INSTALL_LINK, onramp_lines};
use amux::setup::{self, SetupError};
use node::{Config, PairingSecret};

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
    #[cfg(test)]
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

/// Create the installation preferences and its first unbound profile. The
/// supervisor owns identity creation; setup only asks about a shared preference.
pub async fn initialize(
    profile_path: Option<&std::path::Path>,
    reset: bool,
    requested_name: Option<&str>,
) -> anyhow::Result<()> {
    use node::InstallationConfig;
    use node::installation::rpc;

    let mut created = false;
    let mut installation = match profile_path {
        Some(_) => crate::front_door::configuration(profile_path)?,
        None => {
            let path = InstallationConfig::default_path();
            if !path.exists() {
                created = true;
                let config = InstallationConfig::default();
                std::fs::create_dir_all(path.parent().unwrap())?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                file.write_all(serde_yaml::to_string(&config)?.as_bytes())?;
                file.sync_all()?;
            }
            InstallationConfig::from_file(&path)?
        }
    };
    let was_running = crate::front_door::existing(&installation).await?.is_some();
    let mut preferences = Config {
        path: installation.path.clone(),
        host_name: installation.host_name.clone(),
        prevent_idle_sleep: installation.prevent_idle_sleep,
        ..Config::default()
    };
    let choose_name = created || reset || requested_name.is_some();
    let mut host_name_changed = false;
    if choose_name {
        let default = settings::default_host_name();
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let stdout = io::stdout();
        let mut output = stdout.lock();
        let selected = select_host_name(
            &default,
            requested_name,
            stdin.is_terminal(),
            &mut input,
            &mut output,
        )?;
        host_name_changed = selected != installation.host_name;
        setup::set_host_name(&mut preferences, selected)?;
        installation.host_name.clone_from(&preferences.host_name);
    }
    let mut preferences_changed = false;
    if reset {
        setup::clear_prevent_idle_sleep(&mut preferences)?;
        preferences_changed = true;
    }
    if setup::prevent_idle_sleep_supported() && preferences.prevent_idle_sleep.is_none() {
        prompt_idle_sleep(&mut preferences)?;
        preferences_changed = true;
    }
    installation.prevent_idle_sleep = preferences.prevent_idle_sleep;
    let mut front = crate::front_door::connect(&installation, true).await?;
    let mut directory = crate::profiles::directory(&mut front).await?;
    if directory.is_empty() {
        let profile = front
            .profiles
            .create_profile(rpc::CreateProfileRequest {
                operation_id: uuid::Uuid::new_v4().to_string(),
                label: None,
            })
            .await?
            .into_inner();
        println!("Created unbound profile {}.", profile.id);
        directory.push(profile);
    }
    println!("Installation ready. Run `amux login` to connect a cloud account.");
    if let Some(profile_id) = onramp_profile_id(profile_path, &installation, &directory)? {
        let admin = front.admin(node::installation::ProfileId(profile_id));
        if admin.list_peers().await?.is_empty() {
            if admin.pairing_is_active().await? {
                admin.cancel_pairing().await?;
            }
            let pairing = admin
                .start_pin_pairing_with_ttl(node::ONRAMP_PAIR_MODE_TTL)
                .await?;
            let PairingSecret::Pin(code) = pairing.secret else {
                unreachable!("the init on-ramp requests PIN pairing")
            };
            println!();
            for line in onramp_lines(
                &installation.host_name,
                &code,
                std::time::Duration::from_secs(pairing.ttl_seconds),
                INSTALL_LINK,
            ) {
                println!("{line}");
            }
        }
    }
    if was_running && host_name_changed && preferences_changed {
        println!(
            "Restart the server to advertise the new host name and apply changed keep-awake preferences."
        );
    } else if was_running && host_name_changed {
        println!("Restart the server to advertise the new host name.");
    } else if was_running && preferences_changed {
        println!("Restart the server to apply changed keep-awake preferences.");
    }
    Ok(())
}

fn select_host_name<R: BufRead, W: Write>(
    default: &str,
    requested: Option<&str>,
    interactive: bool,
    input: &mut R,
    output: &mut W,
) -> anyhow::Result<String> {
    if let Some(name) = requested {
        settings::validate_host_name(name)?;
        return Ok(name.to_string());
    }
    if !interactive {
        settings::validate_host_name(default)?;
        return Ok(default.to_string());
    }

    loop {
        write!(output, "What should this host be called? [{default}]: ")?;
        output.flush()?;
        let mut line = String::new();
        input.read_line(&mut line)?;
        let entered = line.trim_end_matches(['\r', '\n']);
        let name = if entered.is_empty() { default } else { entered };
        match settings::validate_host_name(name) {
            Ok(()) => return Ok(name.to_string()),
            Err(error) => writeln!(output, "{error}")?,
        }
    }
}

fn onramp_profile_id(
    profile_path: Option<&std::path::Path>,
    installation: &node::InstallationConfig,
    directory: &[node::installation::rpc::ProfileInfo],
) -> anyhow::Result<Option<uuid::Uuid>> {
    if let Some(path) = profile_path {
        return Ok(Some(
            node::load_profile_config(&std::fs::canonicalize(path)?)?
                .profile_id
                .0,
        ));
    }
    let remembered = std::fs::read_to_string(crate::profiles::last_used(installation)).ok();
    Ok(
        crate::profiles::select(directory, None, remembered.as_deref())
            .ok()
            .map(|profile| profile.id.parse())
            .transpose()?,
    )
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
    use amux::setup;
    use node::Config;
    use tempfile::tempdir;

    use super::{
        InitContext, InitStep, needs_init_inner, next_step, parse_idle_sleep_choice,
        select_host_name,
    };

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

    #[test]
    fn interactive_host_name_accepts_the_default() {
        let mut input = "\n".as_bytes();
        let mut output = Vec::new();
        let selected =
            select_host_name("Jordan's Mac", None, true, &mut input, &mut output).unwrap();
        assert_eq!(selected, "Jordan's Mac");
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "What should this host be called? [Jordan's Mac]: "
        );
    }

    #[test]
    fn interactive_host_name_accepts_a_custom_name() {
        let mut input = "Studio Mac\n".as_bytes();
        let mut output = Vec::new();
        let selected =
            select_host_name("Jordan's Mac", None, true, &mut input, &mut output).unwrap();
        assert_eq!(selected, "Studio Mac");
    }

    #[test]
    fn interactive_host_name_reprompts_after_invalid_input() {
        let invalid = "x".repeat(257);
        let answers = format!("{invalid}\nKitchen Mac\n");
        let mut input = answers.as_bytes();
        let mut output = Vec::new();
        let selected =
            select_host_name("Jordan's Mac", None, true, &mut input, &mut output).unwrap();
        assert_eq!(selected, "Kitchen Mac");
        let output = String::from_utf8(output).unwrap();
        assert_eq!(
            output.matches("What should this host be called?").count(),
            2
        );
        assert!(output.contains("host_name must be at most 256 bytes"));
    }

    #[test]
    fn requested_host_name_skips_the_prompt() {
        let mut input = "ignored\n".as_bytes();
        let mut output = Vec::new();
        let selected = select_host_name(
            "Jordan's Mac",
            Some("Scripted Mac"),
            true,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(selected, "Scripted Mac");
        assert!(output.is_empty());
    }

    #[test]
    fn non_terminal_host_name_uses_the_default_without_prompting() {
        let mut input = "ignored\n".as_bytes();
        let mut output = Vec::new();
        let selected =
            select_host_name("Jordan's Mac", None, false, &mut input, &mut output).unwrap();
        assert_eq!(selected, "Jordan's Mac");
        assert!(output.is_empty());
    }
}

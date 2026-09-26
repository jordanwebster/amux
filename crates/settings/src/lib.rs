use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use gethostname::gethostname;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_HOST_NAME_BYTES: usize = 256;

fn xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    if let Ok(value) = std::env::var(env_var) {
        return PathBuf::from(value);
    }
    #[cfg(windows)]
    {
        let base = if default_suffix.starts_with(".config") {
            std::env::var("APPDATA").ok()
        } else {
            std::env::var("LOCALAPPDATA")
                .ok()
                .or_else(|| std::env::var("APPDATA").ok())
        };
        if let Some(base) = base {
            return PathBuf::from(base);
        }
    }
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE").ok()
    } else {
        std::env::var("HOME").ok()
    };
    home.map(PathBuf::from)
        .map(|home| home.join(default_suffix))
        .unwrap_or_else(|| PathBuf::from(default_suffix))
}

fn amux_xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    xdg_dir(env_var, default_suffix).join("amux")
}

#[cfg(not(target_os = "ios"))]
fn default_state_path() -> PathBuf {
    amux_xdg_dir("XDG_STATE_HOME", ".local/state").join("state.yaml")
}

#[cfg(target_os = "ios")]
fn default_state_path() -> PathBuf {
    ios_application_support_dir().join("state.yaml")
}

#[cfg(not(target_os = "ios"))]
fn default_data_dir() -> PathBuf {
    amux_xdg_dir("XDG_DATA_HOME", ".local/share")
}

#[cfg(target_os = "ios")]
fn default_data_dir() -> PathBuf {
    ios_application_support_dir()
}

fn keymap_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("keymaps")
}

#[cfg(target_os = "ios")]
fn ios_application_support_dir() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/amux"))
        .unwrap_or_else(|| PathBuf::from("Library/Application Support/amux"))
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("configuration disagrees on {field}: expected {}, found {}", expected.display(), actual.display())]
    Disagreement {
        field: &'static str,
        expected: PathBuf,
        actual: PathBuf,
    },
}

impl Clone for ConfigError {
    fn clone(&self) -> Self {
        match self {
            Self::Io(error) => Self::Io(match error.raw_os_error() {
                Some(code) => std::io::Error::from_raw_os_error(code),
                None => std::io::Error::new(error.kind(), error.to_string()),
            }),
            Self::Invalid(message) => Self::Invalid(message.clone()),
            Self::Disagreement {
                field,
                expected,
                actual,
            } => Self::Disagreement {
                field,
                expected: expected.clone(),
                actual: actual.clone(),
            },
        }
    }
}

const DEFAULT_CLOUD_URL: &str = "https://amux.sh";

/// Local-network listener settings for a device profile.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LanConfig {
    /// Whether this profile accepts direct connections from the LAN.
    pub listen: bool,
    /// Listener port. Zero asks the operating system for an ephemeral port.
    pub port: u16,
}

impl Default for LanConfig {
    fn default() -> Self {
        Self {
            listen: true,
            port: 0,
        }
    }
}

/// The host name a configuration falls back to when none is written: the
/// system hostname without its mDNS `.local` suffix. This runs on every default
/// and deserialization, so it must stay cheap and free of subprocesses; the
/// friendlier suggestion is computed once, by setup, and written to the file.
pub fn default_host_name() -> String {
    fallback_host_name(
        &gethostname()
            .into_string()
            .unwrap_or_else(|_| "unknown".to_string()),
    )
}

/// The name `amux init` suggests for a new host: the Mac's Computer Name where
/// there is one, otherwise [`default_host_name`].
pub fn suggested_host_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(name) = macos_computer_name() {
        return name;
    }
    default_host_name()
}

#[cfg(target_os = "macos")]
fn macos_computer_name() -> Option<String> {
    let output = Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn fallback_host_name(host_name: &str) -> String {
    host_name
        .strip_suffix(".local")
        .unwrap_or(host_name)
        .to_string()
}

/// Apply the validation shared by persisted settings and setup prompts.
pub fn validate_host_name(host_name: &str) -> Result<(), ConfigError> {
    if host_name.is_empty() {
        return Err(ConfigError::Invalid("host_name must not be empty".into()));
    }
    if host_name.len() > MAX_HOST_NAME_BYTES {
        return Err(ConfigError::Invalid(format!(
            "host_name must be at most {MAX_HOST_NAME_BYTES} bytes"
        )));
    }
    Ok(())
}

fn default_cloud_url() -> String {
    DEFAULT_CLOUD_URL.to_string()
}

/// Per-user runtime directory for the amux socket on Unix.
///
/// - macOS: `$TMPDIR/amux/` (already per-user, e.g. `/var/folders/xx/.../T/`)
/// - Linux: `$XDG_RUNTIME_DIR/amux/` (per-user tmpfs, e.g. `/run/user/1000/`)
/// - Fallback: `/tmp/amux-<uid>/` (UID-embedded for isolation)
#[cfg(unix)]
pub(crate) fn default_socket_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            return PathBuf::from(tmpdir).join("amux");
        }
    } else if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("amux");
    }
    // Fallback: embed UID for per-user isolation
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/amux-{uid}"))
}

#[cfg(unix)]
fn default_socket_path() -> PathBuf {
    default_socket_dir().join("amux.sock")
}

#[cfg(windows)]
fn default_socket_path() -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_string());
    PathBuf::from(format!(r"\\.\pipe\amux-{user}"))
}

/// A control-key leader parsed from the `ctrl+<char>` format (e.g. `ctrl+a`).
#[derive(Debug, Clone)]
pub struct LeaderKey {
    /// The lowercase character (e.g. 'a' for ctrl+a)
    pub char: u8,
}

impl LeaderKey {
    /// Raw byte value for this key (ctrl+a = 0x01, ctrl+b = 0x02, etc.)
    pub fn raw_byte(&self) -> u8 {
        self.char - b'a' + 1
    }

    /// CSI u escape sequence: ESC[<ascii>;5u
    pub fn csi_u_sequence(&self) -> Vec<u8> {
        let ascii = self.char.to_string();
        let mut seq = vec![27, b'['];
        seq.extend_from_slice(ascii.as_bytes());
        seq.extend_from_slice(b";5u");
        seq
    }

    fn parse(s: &str) -> std::result::Result<Self, String> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        let ch = lower
            .strip_prefix("ctrl+")
            .ok_or_else(|| format!("invalid leader key '{s}': expected 'ctrl+<a-z>'"))?;
        if ch.len() != 1 {
            return Err(format!(
                "invalid leader key '{s}': expected single character after 'ctrl+'"
            ));
        }
        let byte = ch.as_bytes()[0];
        if !byte.is_ascii_lowercase() {
            return Err(format!("invalid leader key '{s}': expected 'ctrl+<a-z>'"));
        }
        Ok(Self { char: byte })
    }
}

impl Default for LeaderKey {
    fn default() -> Self {
        Self { char: b'a' }
    }
}

impl fmt::Display for LeaderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ctrl+{}", self.char as char)
    }
}

impl Serialize for LeaderKey {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for LeaderKey {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        LeaderKey::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Keybind configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keybinds {
    /// Leader key prefix for keybinds (default: ctrl+a)
    pub leader: LeaderKey,
}

/// Defaults for newly created Claude agents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeSettings {
    /// How a new Claude agent runs when a creation surface does not say.
    pub driver: ClaudeInterface,
}

/// Claude in a terminal, or headless through its SDK.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeInterface {
    #[default]
    Pty,
    Sdk,
}

/// A setting that is on or off, spelled `on` or `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Switch {
    On,
    Off,
}

/// Whether the supervisor installs releases itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Updates {
    Auto,
    Manual,
}

/// Which release manifest the supervisor follows. Preview is for people who
/// asked for builds ahead of stable, so it is never the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Preview,
}

/// Storage budgets. The starting points are not measured yet; each comment
/// names its basis so a measurement can replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSettings {
    /// Per profile, for the agents this host runs: rows, directories and
    /// their blobs. Basis: a generous multiple of the largest transcripts
    /// seen so far, pending measurement against typical sizes.
    pub own_budget_mib: u64,
    /// Per runtime, for rows replicated from other hosts. Basis: the bound
    /// the old client cache had, 256 MiB.
    pub replica_rows_mib: u64,
    /// Per runtime, for blobs fetched from other hosts, evicted least
    /// recently read first. Basis: twice the row budget, since images
    /// dominate what is fetched; to be measured.
    pub replica_blobs_mib: u64,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            own_budget_mib: 2048,
            replica_rows_mib: 256,
            replica_blobs_mib: 512,
        }
    }
}

/// Local-network discovery.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoverySettings {
    /// Matched exactly against other daemons' advertised scope: a daemon
    /// lists only candidates in its own scope. Empty for real machines; a
    /// worktree or a test network sets its own so it never meets them.
    pub scope: String,
}

/// What each agent process is started with, copied into its spec at spawn.
/// A change affects the next spawn or resume, never a running agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSettings {
    /// How long an agent keeps running after its daemon disappears before
    /// it drains and exits. Basis: minutes, enough to ride out a daemon
    /// update or crash restart.
    pub grace_secs: u64,
    /// How long an orphaned agent waits on an open ask while draining.
    /// Basis: minutes, the same order as the grace.
    pub drain_secs: u64,
    /// The interpreter's facts ring: bytes per segment and segments kept.
    /// Basis: 2–4 MB and two segments, enough to replay recent history into
    /// a dump without keeping a second transcript.
    pub facts_ring_mib: u64,
    pub facts_ring_segments: u32,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            grace_secs: 300,
            drain_secs: 300,
            facts_ring_mib: 4,
            facts_ring_segments: 2,
        }
    }
}

/// Which mode Enter opens a Claude agent in from the fleet
/// (`docs/CHAT.md` A1). The shipped default is raw attach — the
/// battle-tested path stays the path of least surprise; flipping the
/// default is this settings change, not a migration. Mobile clients are
/// chat-only and carry no such setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenMode {
    /// Raw byte passthrough to the agent's own TUI.
    #[default]
    Raw,
    /// The structured chat view.
    Chat,
}

/// Where the palette comes from: the terminal amux was started in, a
/// shipped theme name, or a YAML theme file path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ThemeSetting {
    /// Derived from what the terminal reports about its own colours, with
    /// the shipped dark palette as the fallback for a terminal that does
    /// not answer.
    #[default]
    Terminal,
    Dark,
    Light,
    File(PathBuf),
}

impl Serialize for ThemeSetting {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Terminal => serializer.serialize_str("terminal"),
            Self::Dark => serializer.serialize_str("dark"),
            Self::Light => serializer.serialize_str("light"),
            Self::File(path) => path.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ThemeSetting {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "terminal" => Self::Terminal,
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::File(PathBuf::from(value)),
        })
    }
}

/// The terminal colour capability preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorSetting {
    #[default]
    Auto,
    TrueColor,
    Ansi,
}

/// Client UI configuration (the TUI; future desktop clients read the same
/// keys).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    /// The mode the fleet's Enter opens; the non-default mode opens via
    /// Ctrl+Enter (kitty-detected) or `o`.
    pub default_open_mode: OpenMode,
    /// A shipped theme name or a path resolved beside the config file.
    pub theme: ThemeSetting,
    /// Whether to detect, force, or disable truecolor output.
    pub color: ColorSetting,
}

/// Preferences shared by every profile in an installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstallationConfig {
    pub repository_roots: Vec<PathBuf>,
    pub claude: ClaudeSettings,
    pub root: PathBuf,
    pub front_door_socket: PathBuf,
    pub host_name: String,
    /// Whether this install runs the daemon under `amux supervise`. Decided
    /// by the install, not chosen: the desktop installer writes `on`; a host
    /// whose service manager runs the daemon directly leaves it `off`.
    pub supervisor: Switch,
    /// Whether the supervisor also installs releases. Absent means `auto`
    /// under a supervisor and `manual` without one; `auto` without a
    /// supervisor is an error. Read it through [`InstallationConfig::updates`].
    pub updates: Option<Updates>,
    pub channel: Channel,
    /// Whether the supervisor holds the machine's sleep assertion.
    pub keep_awake: Switch,
    pub retention: RetentionSettings,
    pub discovery: DiscoverySettings,
    pub agent: AgentSettings,
    pub keybinds: Keybinds,
    pub ui: UiSettings,
    pub reports_dir: Option<PathBuf>,
    pub keymaps_dir: PathBuf,
    pub update_manifest_url: String,
    pub minimum_client_versions: HashMap<String, String>,
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl Default for InstallationConfig {
    fn default() -> Self {
        Self {
            repository_roots: Vec::new(),
            claude: ClaudeSettings::default(),
            root: default_data_dir(),
            front_door_socket: default_socket_path(),
            host_name: default_host_name(),
            supervisor: Switch::Off,
            updates: None,
            channel: Channel::default(),
            keep_awake: Switch::On,
            retention: RetentionSettings::default(),
            discovery: DiscoverySettings::default(),
            agent: AgentSettings::default(),
            keybinds: Keybinds::default(),
            ui: UiSettings::default(),
            reports_dir: None,
            keymaps_dir: keymap_dir(&default_data_dir()),
            update_manifest_url: format!("{DEFAULT_CLOUD_URL}/manifest.json"),
            minimum_client_versions: HashMap::new(),
            path: None,
        }
    }
}

impl InstallationConfig {
    pub fn default_path() -> PathBuf {
        amux_xdg_dir("XDG_CONFIG_HOME", ".config").join("config.yaml")
    }

    /// Parses installation YAML, rejecting retired keys by name and an
    /// impossible supervisor and updates pair.
    pub fn from_yaml(yaml: &str) -> Result<Self, ConfigError> {
        let config: Self = parse_yaml(yaml)?;
        config.updates()?;
        Ok(config)
    }

    /// The effective updates mode; `auto` without a supervisor is an error,
    /// since only a supervisor installs releases.
    pub fn updates(&self) -> Result<Updates, ConfigError> {
        match (self.supervisor, self.updates) {
            (Switch::Off, Some(Updates::Auto)) => Err(ConfigError::Invalid(
                "updates: auto needs supervisor: on; without a supervisor, updates are deploys (updates: manual)".into(),
            )),
            (_, Some(updates)) => Ok(updates),
            (Switch::On, None) => Ok(Updates::Auto),
            (Switch::Off, None) => Ok(Updates::Manual),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let path = absolute_path(path, &std::env::current_dir()?)?;
        let yaml = std::fs::read_to_string(&path)?;
        let mut config = Self::from_yaml(&yaml)
            .map_err(|error| ConfigError::Invalid(format!("{}: {error}", path.display())))?;
        let base = path.parent().unwrap();
        config.root = absolute_path(&config.root, base)?;
        config.front_door_socket = absolute_path(&config.front_door_socket, base)?;
        config.keymaps_dir = absolute_path(&config.keymaps_dir, base)?;
        config.reports_dir = config
            .reports_dir
            .as_deref()
            .map(|path| absolute_path(path, base))
            .transpose()?;
        if let ThemeSetting::File(theme) = &mut config.ui.theme {
            *theme = absolute_path(theme, base)?;
        }
        config.path = Some(path);
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.updates()?;
        Config {
            host_name: self.host_name.clone(),
            keybinds: self.keybinds.clone(),
            minimum_client_versions: self.minimum_client_versions.clone(),
            ..Config::default()
        }
        .validate()
    }

    pub fn file_path(&self) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| self.root.join("config.yaml"))
    }
}

/// The per-device file named by AMUX_CONFIG. Its installation is explicit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub installation_config: PathBuf,
    pub socket_path: PathBuf,
    pub data_dir: PathBuf,
    pub state_path: PathBuf,
    #[serde(default = "default_cloud_url")]
    pub cloud_url: String,
    #[serde(default)]
    pub lan: LanConfig,
    /// Debug/test override for free-tier entitlement refreshes. Release
    /// runtimes parse the key for config compatibility but never use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_refresh_secs: Option<u64>,
}

impl ProfileConfig {
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let mut config: Self = read_yaml(path)?;
        if !config.installation_config.is_absolute() {
            return Err(ConfigError::Invalid(
                "installation_config must be an absolute path".into(),
            ));
        }
        let base = path.parent().ok_or_else(|| {
            ConfigError::Invalid("profile config must have a parent directory".into())
        })?;
        config.installation_config = absolute_path(&config.installation_config, base)?;
        config.socket_path = absolute_path(&config.socket_path, base)?;
        config.data_dir = absolute_path(&config.data_dir, base)?;
        config.state_path = absolute_path(&config.state_path, base)?;
        #[cfg(not(any(debug_assertions, test)))]
        if !config.cloud_url.starts_with("https://") {
            return Err(ConfigError::Invalid("cloud_url must use HTTPS".into()));
        }
        Ok(config)
    }
}

fn read_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    parse_yaml(&std::fs::read_to_string(path)?)
        .map_err(|error| ConfigError::Invalid(format!("{}: {error}", path.display())))
}

/// Keys that used to exist, each with what replaced it, so an old config
/// file fails with the reason rather than a bare unknown-field error.
const RETIRED_KEYS: &[(&[&str], &str)] = &[
    (
        &["ui", "artifact_cache_mib"],
        "clients keep no attachment cache; attachment bytes live in each agent's directory and go with it",
    ),
    (
        &["prevent_idle_sleep"],
        "replaced by keep_awake (on | off), which the supervisor holds for its lifetime",
    ),
];

/// Parses YAML into a config type after rejecting retired keys by name.
fn parse_yaml<T: serde::de::DeserializeOwned>(yaml: &str) -> Result<T, ConfigError> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|error| ConfigError::Invalid(error.to_string()))?;
    for (path, why) in RETIRED_KEYS {
        let mut node = Some(&value);
        for key in *path {
            node = node.and_then(|node| node.get(*key));
        }
        if node.is_some() {
            return Err(ConfigError::Invalid(format!(
                "`{}` is no longer a setting: {why}",
                path.join(".")
            )));
        }
    }
    serde_yaml::from_value(value).map_err(|error| ConfigError::Invalid(error.to_string()))
}

// Resolve aliases in the existing ancestor, even before a socket or state file
// exists. This gives /tmp and /private/tmp the same identity on macOS.
fn absolute_path(path: &Path, base: &Path) -> Result<PathBuf, ConfigError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    match std::fs::canonicalize(&path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                ConfigError::Invalid(format!("cannot resolve {}", path.display()))
            })?;
            let parent = absolute_path(parent, base)?;
            match path.file_name() {
                Some(name) => Ok(parent.join(name)),
                None => Err(ConfigError::Invalid(format!(
                    "cannot resolve {}",
                    path.display()
                ))),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Human-readable hostname for generating link names.
    #[serde(default = "default_host_name")]
    pub host_name: String,

    /// Cloud API URL for authentication and connection routing
    #[serde(default = "default_cloud_url")]
    pub cloud_url: String,

    /// Path to Unix socket for local connections
    #[serde(default = "default_socket_path")]
    pub socket_path: PathBuf,

    /// TCP port for server-to-server connections (None = don't listen)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,

    /// UDP port for the cloud relay's QUIC listener (None = don't listen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_port: Option<u16>,

    /// Local-network listener settings when this config runs a device.
    #[serde(default)]
    pub lan: LanConfig,

    /// Path to state file.
    #[serde(default = "default_state_path")]
    pub state_path: PathBuf,

    /// Data directory for device identity, trust, and runtime artifacts.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Directory where diagnostic report bundles are written. Defaults to the
    /// `reports` directory beneath `data_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_dir: Option<PathBuf>,

    /// Per-auth-client minimum version requirements (e.g. {"cli": "0.2.0"}).
    /// Cloud peers whose token client_id matches a key and whose host version
    /// is below the value are refused as a version mismatch.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub minimum_client_versions: HashMap<String, String>,

    /// Directories searched for Git repositories when clients create agents. Empty by default.
    #[serde(default)]
    pub repository_roots: Vec<PathBuf>,

    /// Keybind configuration
    #[serde(default)]
    pub keybinds: Keybinds,

    /// Client UI configuration
    #[serde(default)]
    pub ui: UiSettings,

    /// Defaults for newly created Claude agents.
    #[serde(default)]
    pub claude: ClaudeSettings,

    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host_name: default_host_name(),
            cloud_url: default_cloud_url(),
            socket_path: default_socket_path(),
            tcp_port: None,
            udp_port: None,
            lan: LanConfig::default(),
            state_path: default_state_path(),
            data_dir: default_data_dir(),
            reports_dir: None,
            minimum_client_versions: HashMap::new(),
            repository_roots: Vec::new(),
            keybinds: Keybinds::default(),
            ui: UiSettings::default(),
            claude: ClaudeSettings::default(),
            path: None,
        }
    }
}

/// Resolve the driver for a newly created Claude agent.
///
/// A creation-time override wins over the configured default. With neither,
/// the shipped default remains the PTY driver.
pub fn resolve_claude_driver(
    explicit: Option<ClaudeInterface>,
    config: &Config,
) -> ClaudeInterface {
    explicit.unwrap_or(config.claude.driver)
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// The configured diagnostic report directory, or `data_dir/reports`.
    pub fn reports_dir(&self) -> PathBuf {
        self.reports_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("reports"))
    }

    /// Default config file path: `$XDG_CONFIG_HOME/amux/config.yaml`,
    /// falling back to `~/.config/amux/config.yaml`.
    pub fn default_path() -> PathBuf {
        amux_xdg_dir("XDG_CONFIG_HOME", ".config").join("config.yaml")
    }

    /// Validate config. Call early to surface errors before any work begins.
    pub fn validate(&self) -> std::result::Result<(), ConfigError> {
        // Leader key must be ctrl+<a-z>
        let ch = self.keybinds.leader.char;
        if !ch.is_ascii_lowercase() {
            return Err(ConfigError::Invalid(format!(
                "invalid leader key: byte 0x{ch:02x} is not a lowercase letter (expected ctrl+<a-z>)"
            )));
        }

        validate_host_name(&self.host_name)?;

        // Release builds must use HTTPS for cloud URLs to protect tokens in transit
        #[cfg(not(any(debug_assertions, test)))]
        if !self.cloud_url.starts_with("https://") {
            return Err(ConfigError::Invalid("cloud_url must use HTTPS".into()));
        }

        // Validate all minimum_client_versions values are valid semver
        for (name, version) in &self.minimum_client_versions {
            if semver::Version::parse(version).is_err() {
                return Err(ConfigError::Invalid(format!(
                    "invalid minimum_client_versions['{name}']: '{version}' is not valid semver (e.g. \"0.2.0\")"
                )));
            }
        }

        Ok(())
    }

    /// Parses server YAML, rejecting retired keys by name.
    pub fn from_yaml(yaml: &str) -> std::result::Result<Self, ConfigError> {
        parse_yaml(yaml)
    }

    /// Load config from a YAML file
    pub fn from_file(path: &Path) -> std::result::Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let mut config = Self::from_yaml(&contents)?;
        config.path = Some(path.to_path_buf());
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_driver_config_defaults_to_pty_and_accepts_sdk() {
        let absent: Config = serde_yaml::from_str("host_name: test\n").unwrap();
        assert_eq!(resolve_claude_driver(None, &absent), ClaudeInterface::Pty);

        let sdk: Config = serde_yaml::from_str("claude:\n  driver: sdk\n").unwrap();
        assert_eq!(resolve_claude_driver(None, &sdk), ClaudeInterface::Sdk);
        assert_eq!(
            resolve_claude_driver(Some(ClaudeInterface::Pty), &sdk),
            ClaudeInterface::Pty
        );
    }

    #[test]
    fn claude_driver_config_rejects_unknown_keys_and_values() {
        assert!(
            serde_yaml::from_str::<Config>("claude:\n  backend: sdk\n")
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
        assert!(serde_yaml::from_str::<Config>("claude:\n  driver: other\n").is_err());
    }

    /// Verify serde_yaml round-trips Windows-style backslash paths correctly.
    /// serde_yaml serializes paths unquoted, which YAML parses literally.
    /// (Double-quoted YAML strings would break because `\p`, `\U` etc. are
    /// invalid YAML escape sequences.)
    #[test]
    fn yaml_windows_path_roundtrip() {
        let config = Config {
            socket_path: PathBuf::from(r"\\.\pipe\amux-test"),
            state_path: PathBuf::from(r"C:\Users\me\state.yaml"),
            data_dir: PathBuf::from(r"C:\Users\me\amux-data"),
            ..Config::default()
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.socket_path, config.socket_path);
        assert_eq!(parsed.state_path, config.state_path);
        assert_eq!(parsed.data_dir, config.data_dir);
    }

    #[test]
    fn data_dir_defaults_to_default_data_dir() {
        assert_eq!(Config::default().data_dir, default_data_dir());
        let config: Config = serde_yaml::from_str("tcp_port: 9999\n").unwrap();
        assert_eq!(config.data_dir, default_data_dir());
    }

    #[test]
    fn data_dir_yaml_roundtrip() {
        let yaml = "data_dir: /srv/amux-dev/data\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/srv/amux-dev/data"));

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.data_dir, PathBuf::from("/srv/amux-dev/data"));
    }

    #[test]
    fn reports_dir_defaults_beneath_data_dir() {
        let config: Config = serde_yaml::from_str("data_dir: /srv/amux-dev/data\n").unwrap();

        assert_eq!(config.reports_dir, None);
        assert_eq!(
            config.reports_dir(),
            PathBuf::from("/srv/amux-dev/data/reports")
        );
    }

    #[test]
    fn reports_dir_yaml_roundtrip() {
        let yaml = "data_dir: /srv/amux-dev/data\nreports_dir: /srv/amux-reports\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.reports_dir(), PathBuf::from("/srv/amux-reports"));

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.reports_dir, Some(PathBuf::from("/srv/amux-reports")));
        assert_eq!(parsed.reports_dir(), PathBuf::from("/srv/amux-reports"));
    }

    #[test]
    fn retired_claude_plugin_config_is_rejected() {
        let error =
            serde_yaml::from_str::<Config>("claude:\n  manage_plugin: false\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn leader_key_default_is_ctrl_a() {
        let leader = LeaderKey::default();
        assert_eq!(leader.char, b'a');
        assert_eq!(leader.raw_byte(), 0x01);
        // ESC[97;5u
        assert_eq!(
            leader.csi_u_sequence(),
            vec![27, b'[', b'9', b'7', b';', b'5', b'u']
        );
    }

    #[test]
    fn leader_key_ctrl_b() {
        let leader = LeaderKey::parse("ctrl+b").unwrap();
        assert_eq!(leader.char, b'b');
        assert_eq!(leader.raw_byte(), 0x02);
        // ESC[98;5u
        assert_eq!(
            leader.csi_u_sequence(),
            vec![27, b'[', b'9', b'8', b';', b'5', b'u']
        );
    }

    #[test]
    fn leader_key_case_insensitive() {
        let leader = LeaderKey::parse("Ctrl+A").unwrap();
        assert_eq!(leader.char, b'a');
    }

    #[test]
    fn leader_key_invalid() {
        assert!(LeaderKey::parse("alt+a").is_err());
        assert!(LeaderKey::parse("ctrl+1").is_err());
        assert!(LeaderKey::parse("ctrl+ab").is_err());
        assert!(LeaderKey::parse("a").is_err());
    }

    #[test]
    fn leader_key_yaml_roundtrip() {
        let yaml = "leader: ctrl+b\n";
        let keybinds: Keybinds = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(keybinds.leader.char, b'b');

        let serialized = serde_yaml::to_string(&keybinds).unwrap();
        let parsed: Keybinds = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.leader.char, b'b');
    }

    #[test]
    fn config_with_keybinds() {
        let yaml = "keybinds:\n  leader: ctrl+b\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.keybinds.leader.char, b'b');
    }

    #[test]
    fn config_without_keybinds_uses_default() {
        let yaml = "tcp_port: 9999\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.tcp_port, Some(9999));
        assert_eq!(config.keybinds.leader.char, b'a');
    }

    /// The shipped default open mode is raw attach (`docs/CHAT.md` A1).
    #[test]
    fn default_open_mode_is_raw() {
        let config = Config::default();
        assert_eq!(config.ui.default_open_mode, OpenMode::Raw);
        let yaml = "tcp_port: 9999\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.ui.default_open_mode, OpenMode::Raw);
    }

    #[test]
    fn default_open_mode_yaml_roundtrip() {
        let yaml = "ui:\n  default_open_mode: chat\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.ui.default_open_mode, OpenMode::Chat);

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.ui.default_open_mode, OpenMode::Chat);
    }

    #[test]
    fn ui_theme_and_color_defaults_are_terminal_and_auto() {
        let config = Config::default();
        assert_eq!(config.ui.theme, ThemeSetting::Terminal);
        assert_eq!(config.ui.color, ColorSetting::Auto);

        let parsed: Config = serde_yaml::from_str("ui: {}\n").unwrap();
        assert_eq!(parsed.ui.theme, ThemeSetting::Terminal);
        assert_eq!(parsed.ui.color, ColorSetting::Auto);
    }

    #[test]
    fn ui_theme_and_color_yaml_roundtrip() {
        let yaml = "ui:\n  theme: light\n  color: ansi\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.ui.theme, ThemeSetting::Light);
        assert_eq!(config.ui.color, ColorSetting::Ansi);

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.ui.theme, ThemeSetting::Light);
        assert_eq!(parsed.ui.color, ColorSetting::Ansi);
    }

    #[test]
    fn ui_theme_file_path_yaml_roundtrip() {
        let yaml = "ui:\n  theme: themes/forest.yaml\n  color: truecolor\n";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.ui.theme,
            ThemeSetting::File(PathBuf::from("themes/forest.yaml"))
        );
        assert_eq!(config.ui.color, ColorSetting::TrueColor);

        let serialized = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(parsed.ui.theme, config.ui.theme);
        assert_eq!(parsed.ui.color, config.ui.color);
    }

    #[test]
    fn unknown_ui_color_is_rejected() {
        let error = serde_yaml::from_str::<Config>("ui:\n  color: millions\n").unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn unknown_open_mode_is_rejected() {
        let error =
            serde_yaml::from_str::<Config>("ui:\n  default_open_mode: telepathy\n").unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn ports_default_to_none() {
        let config = Config::default();
        assert_eq!(config.tcp_port, None);
        assert_eq!(config.udp_port, None);
        assert_eq!(config.lan, LanConfig::default());
    }

    #[test]
    fn lan_defaults_to_an_ephemeral_listener_and_parses_overrides() {
        let defaulted: Config = serde_yaml::from_str("host_name: test\n").unwrap();
        assert_eq!(defaulted.lan, LanConfig::default());

        let configured: Config =
            serde_yaml::from_str("lan:\n  listen: false\n  port: 4242\n").unwrap();
        assert_eq!(
            configured.lan,
            LanConfig {
                listen: false,
                port: 4242,
            }
        );
    }

    #[test]
    fn config_split_rejects_retired_cloud_mode() {
        let error = serde_yaml::from_str::<Config>("enable_cloud_mode: false\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn unknown_config_field_is_rejected() {
        let error = serde_yaml::from_str::<Config>("check_for_updates: false\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn validate_default_config_ok() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_with_relay_ports() {
        let config = Config {
            tcp_port: Some(9001),
            udp_port: Some(9001),
            ..Config::default()
        };
        assert!(config.validate().is_ok());
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.tcp_port, Some(9001));
        assert_eq!(parsed.udp_port, Some(9001));
    }

    #[test]
    fn validate_leader_key_bad_char() {
        let mut config = Config::default();
        config.keybinds.leader = LeaderKey { char: b'1' };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("leader key"));
    }

    #[test]
    fn validate_minimum_client_versions_valid() {
        let config = Config {
            minimum_client_versions: HashMap::from([("cli".to_string(), "0.2.0".to_string())]),
            ..Config::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_host_name() {
        let config = Config {
            host_name: String::new(),
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host_name"));
    }

    #[test]
    fn validate_accepts_long_host_name() {
        let config = Config {
            host_name: "a".repeat(MAX_HOST_NAME_BYTES),
            ..Config::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_oversized_host_name() {
        let config = Config {
            host_name: "a".repeat(MAX_HOST_NAME_BYTES + 1),
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host_name"));
    }

    #[test]
    fn fallback_host_name_strips_the_mdns_suffix() {
        assert_eq!(
            fallback_host_name("Jordans-MacBook.local"),
            "Jordans-MacBook"
        );
        assert_eq!(fallback_host_name("build-host"), "build-host");
    }

    #[test]
    fn validate_minimum_client_versions_invalid() {
        let config = Config {
            minimum_client_versions: HashMap::from([("cli".to_string(), "v0.2.0".to_string())]),
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("minimum_client_versions"));
        assert!(err.to_string().contains("cli"));
    }

    fn installation(yaml: &str) -> Result<InstallationConfig, ConfigError> {
        InstallationConfig::from_yaml(yaml)
    }

    #[test]
    fn supervisor_and_updates_cover_every_cell_of_the_table() {
        // (supervisor, updates) as written -> effective updates, or an error.
        let cells: &[(&str, Option<Updates>)] = &[
            ("supervisor: on\nupdates: auto\n", Some(Updates::Auto)),
            ("supervisor: on\nupdates: manual\n", Some(Updates::Manual)),
            ("supervisor: off\nupdates: manual\n", Some(Updates::Manual)),
            ("supervisor: off\nupdates: auto\n", None),
            ("supervisor: on\n", Some(Updates::Auto)),
            ("supervisor: off\n", Some(Updates::Manual)),
            ("{}\n", Some(Updates::Manual)),
            ("updates: auto\n", None),
        ];
        for (yaml, expected) in cells {
            match (installation(yaml), expected) {
                (Ok(config), Some(updates)) => {
                    assert_eq!(config.updates().unwrap(), *updates, "{yaml}");
                }
                (Err(error), None) => {
                    assert!(
                        error
                            .to_string()
                            .contains("updates: auto needs supervisor: on"),
                        "{yaml}: {error}"
                    );
                }
                (outcome, expected) => panic!("{yaml}: {outcome:?}, expected {expected:?}"),
            }
        }
    }

    #[test]
    fn a_constructed_config_with_auto_and_no_supervisor_fails_validation() {
        let config = InstallationConfig {
            supervisor: Switch::Off,
            updates: Some(Updates::Auto),
            ..InstallationConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn supervisor_updates_channel_and_keep_awake_parse_and_default() {
        let defaults = installation("{}\n").unwrap();
        assert_eq!(defaults.supervisor, Switch::Off);
        assert_eq!(defaults.channel, Channel::Stable);
        assert_eq!(defaults.keep_awake, Switch::On);

        let set = installation("supervisor: on\nchannel: preview\nkeep_awake: off\n").unwrap();
        assert_eq!(set.supervisor, Switch::On);
        assert_eq!(set.channel, Channel::Preview);
        assert_eq!(set.keep_awake, Switch::Off);

        for bad in [
            "supervisor: yes\n",
            "updates: sometimes\n",
            "channel: beta\n",
            "keep_awake: true\n",
        ] {
            assert!(installation(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn budgets_discovery_and_agent_parameters_parse_with_starting_points() {
        let defaults = installation("{}\n").unwrap();
        assert_eq!(defaults.retention, RetentionSettings::default());
        assert_eq!(defaults.retention.replica_rows_mib, 256);
        assert_eq!(defaults.discovery.scope, "");
        assert_eq!(defaults.agent, AgentSettings::default());
        assert_eq!(defaults.agent.facts_ring_segments, 2);

        let set = installation(
            "retention:\n  own_budget_mib: 100\n  replica_rows_mib: 10\n  replica_blobs_mib: 20\n\
             discovery:\n  scope: rearchitect\n\
             agent:\n  grace_secs: 60\n  drain_secs: 30\n  facts_ring_mib: 2\n  facts_ring_segments: 3\n",
        )
        .unwrap();
        assert_eq!(
            set.retention,
            RetentionSettings {
                own_budget_mib: 100,
                replica_rows_mib: 10,
                replica_blobs_mib: 20,
            }
        );
        assert_eq!(set.discovery.scope, "rearchitect");
        assert_eq!(
            set.agent,
            AgentSettings {
                grace_secs: 60,
                drain_secs: 30,
                facts_ring_mib: 2,
                facts_ring_segments: 3,
            }
        );
        assert!(installation("retention:\n  budget: 1\n").is_err());
        assert_eq!(
            installation("discovery:\n  scope: \"3\"\n")
                .unwrap()
                .discovery
                .scope,
            "3"
        );
    }

    #[test]
    fn retired_keys_are_rejected_with_what_replaced_them() {
        for (yaml, key, why) in [
            (
                "ui:\n  artifact_cache_mib: 48\n",
                "ui.artifact_cache_mib",
                "agent's directory",
            ),
            (
                "prevent_idle_sleep: true\n",
                "prevent_idle_sleep",
                "keep_awake",
            ),
        ] {
            for error in [
                installation(yaml).unwrap_err().to_string(),
                Config::from_yaml(yaml).unwrap_err().to_string(),
            ] {
                assert!(
                    error.contains(&format!("`{key}` is no longer a setting"))
                        && error.contains(why),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn installation_config_round_trips_the_new_keys() {
        let config = installation(
            "supervisor: on\nupdates: manual\nchannel: preview\nkeep_awake: off\ndiscovery:\n  scope: s\n",
        )
        .unwrap();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let again = installation(&yaml).unwrap();
        assert_eq!(again.updates().unwrap(), Updates::Manual);
        assert_eq!(again.channel, Channel::Preview);
        assert_eq!(again.keep_awake, Switch::Off);
        assert_eq!(again.discovery.scope, "s");
    }
}

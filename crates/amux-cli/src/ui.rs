//! Bare `amux` (and `amux ui`): the fleet TUI.
//!
//! Dispatch order is deliberate: an uninitialized machine runs the init flow
//! FIRST — auth stays CLI-owned and the TUI stays auth-passive — then the
//! fleet opens. Expired cloud auth never blocks here: the TUI opens
//! instantly and renders the degraded banner from Model state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use amux::{ColorSetting, Config, ThemeSetting, UiSettings};
use amux_tui::{
    ColorPreference, Theme, ThemeError, TuiConfig, detect_color_mode, parse_theme_file, run_fleet,
    theme_from_file,
};
use amux_ui::{ConnectFailure, Connector, Runtime, RuntimeOptions};
use anyhow::{Context, Result};

use crate::client_common::get_client;
use crate::init::{self, InitContext};
use crate::update::MarkerFileReporter;

pub async fn run(config: Config) -> Result<()> {
    run_inner(config, None, None).await
}

pub(crate) async fn run_for_agent(
    config: Config,
    agent: amux::AgentId,
    codex_configuration: Option<String>,
) -> Result<()> {
    run_inner(config, Some(agent), codex_configuration).await
}

async fn run_inner(
    mut config: Config,
    initial_chat: Option<amux::AgentId>,
    initial_chat_configuration: Option<String>,
) -> Result<()> {
    if init::needs_init(&config) {
        init::run_init(&mut config, InitContext::implicit(), false).await?;
    }

    let config_dir = config
        .path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            Config::default_path()
                .parent()
                .expect("default config path has a parent")
                .to_path_buf()
        });
    let theme = resolve_theme(&config.ui, &config_dir, &ColorEnv::capture())
        .context("failed to resolve ui.theme")?;

    // The local host id comes from the stored device identity — the wire
    // does not mark the local host (see docs/UI.md, subscription policy).
    let local_host_id = amux::setup::local_host_id(&config);
    let subscription_reporter = MarkerFileReporter::from_state_path(&config.state_path);

    let connector: Connector = {
        let config = config.clone();
        Box::new(move || {
            let config = config.clone();
            Box::pin(async move {
                // get_client spawns the daemon when absent; while it runs
                // the TUI shows the "Starting daemon…" state.
                get_client(&config).await.map_err(|error| ConnectFailure {
                    message: format!("{error:#}"),
                    auth_required: false,
                    subscription_required: false,
                })
            })
        })
    };
    let mut runtime = Runtime::start(
        connector,
        RuntimeOptions {
            local_host_id,
            dump_dir: Some(config.data_dir.join("ui-dumps")),
            subscription_status_provider: Some(Arc::new(move || {
                subscription_reporter.subscription_required()
            })),
            ..RuntimeOptions::default()
        },
    );
    // A panic anywhere in the TUI leaves a Msg recording: the terminal.rs
    // panic hook calls amux_ui::write_panic_dump after restoring the
    // terminal.
    runtime.install_panic_dump();

    let tui_config = TuiConfig {
        working_dir: std::env::current_dir()?,
        leader: config.keybinds.leader.char as char,
        theme,
        // The A1 entry-mode setting, from the usual amux config
        // (`ui.default_open_mode`; shipped default: raw attach).
        default_open_mode: match config.ui.default_open_mode {
            amux::OpenMode::Raw => amux_tui::OpenMode::RawAttach,
            amux::OpenMode::Chat => amux_tui::OpenMode::Chat,
        },
        default_agent_type: amux::AgentType::Claude,
        initial_chat,
        initial_chat_configuration,
    };

    let attach_config = config.clone();
    run_fleet(&mut runtime, tui_config, move |agent| {
        let config = attach_config.clone();
        async move { crate::session_client::attach_for_ui(&config, agent).await }
    })
    .await
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ColorEnv {
    pub(crate) colorterm: Option<String>,
    pub(crate) term: Option<String>,
    pub(crate) no_color: bool,
}

impl ColorEnv {
    fn capture() -> Self {
        Self {
            colorterm: std::env::var("COLORTERM").ok(),
            term: std::env::var("TERM").ok(),
            no_color: std::env::var_os("NO_COLOR").is_some(),
        }
    }
}

pub(crate) fn resolve_theme(
    settings: &UiSettings,
    config_dir: &Path,
    env: &ColorEnv,
) -> std::result::Result<Theme, ThemeError> {
    let preference = match settings.color {
        ColorSetting::Auto => ColorPreference::Auto,
        ColorSetting::TrueColor => ColorPreference::TrueColor,
        ColorSetting::Ansi => ColorPreference::Ansi,
    };
    let mode = detect_color_mode(
        preference,
        env.colorterm.as_deref(),
        env.term.as_deref(),
        env.no_color,
    );

    match &settings.theme {
        ThemeSetting::Dark => Ok(Theme::dark(mode)),
        ThemeSetting::Light => Ok(Theme::light(mode)),
        ThemeSetting::File(path) => {
            let path = resolve_theme_path(path, config_dir);
            let yaml = std::fs::read_to_string(path)?;
            let file = parse_theme_file(&yaml)?;
            theme_from_file(&file, mode)
        }
    }
}

fn resolve_theme_path(path: &Path, config_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use amux_tui::ColorMode;

    use super::*;

    const BASE16: &str = r##"
scheme: cli theme fixture
base00: "#101010"
base01: "#202020"
base02: "#303030"
base03: "#404040"
base04: "#505050"
base05: "#606060"
base06: "#707070"
base07: "#808080"
base08: "#900000"
base09: "#a06000"
base0A: "#b09000"
base0B: "#009000"
base0C: "#008090"
base0D: "#0060a0"
base0E: "#7040a0"
base0F: "#804020"
"##;

    fn env(colorterm: Option<&str>, no_color: bool) -> ColorEnv {
        ColorEnv {
            colorterm: colorterm.map(str::to_string),
            term: Some("xterm-256color".into()),
            no_color,
        }
    }

    #[test]
    fn theme_builtin_respects_color_preference() {
        let settings = UiSettings {
            theme: ThemeSetting::Light,
            color: ColorSetting::Ansi,
            ..UiSettings::default()
        };
        let theme = resolve_theme(
            &settings,
            Path::new("/unused"),
            &env(Some("truecolor"), false),
        )
        .expect("resolve built-in theme");
        assert_eq!(theme.name, amux_tui::ThemeName::Light);
        assert_eq!(theme.mode, ColorMode::Ansi);
    }

    #[test]
    fn theme_auto_uses_captured_capability_and_no_color() {
        let settings = UiSettings::default();
        let truecolor = resolve_theme(
            &settings,
            Path::new("/unused"),
            &env(Some("truecolor"), false),
        )
        .expect("resolve truecolor theme");
        assert_eq!(truecolor.mode, ColorMode::TrueColor);

        let ansi = resolve_theme(
            &settings,
            Path::new("/unused"),
            &env(Some("truecolor"), true),
        )
        .expect("resolve NO_COLOR theme");
        assert_eq!(ansi.mode, ColorMode::Ansi);
    }

    #[test]
    fn theme_relative_file_resolves_beside_config() {
        let directory = tempfile::tempdir().expect("tempdir");
        let themes = directory.path().join("themes");
        fs::create_dir(&themes).expect("create themes directory");
        fs::write(themes.join("sample.yaml"), BASE16).expect("write theme fixture");
        let settings = UiSettings {
            theme: ThemeSetting::File(PathBuf::from("themes/sample.yaml")),
            color: ColorSetting::TrueColor,
            ..UiSettings::default()
        };

        let theme = resolve_theme(&settings, directory.path(), &ColorEnv::default())
            .expect("resolve relative imported theme");
        assert_eq!(theme.name, amux_tui::ThemeName::Imported);
        assert_eq!(theme.tokens.background.rgb, (0x10, 0x10, 0x10));
        assert_eq!(theme.mode, ColorMode::TrueColor);
    }

    #[test]
    fn theme_bad_file_is_returned_before_terminal_startup() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("bad.yaml"), "base00: nope\n").expect("write bad theme");
        let settings = UiSettings {
            theme: ThemeSetting::File(PathBuf::from("bad.yaml")),
            ..UiSettings::default()
        };

        assert!(matches!(
            resolve_theme(&settings, directory.path(), &ColorEnv::default()),
            Err(ThemeError::BadColor { key, value }) if key == "base00" && value == "nope"
        ));
    }
}

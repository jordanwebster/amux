//! Bare `amux` (and `amux ui`): the fleet TUI.
//!
//! Dispatch order is deliberate: an uninitialized machine runs the init flow
//! FIRST — auth stays CLI-owned and the TUI stays auth-passive — then the
//! fleet opens. Expired cloud auth never blocks here: the TUI opens
//! instantly and renders the degraded banner from Model state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use amux::{ColorSetting, Config, DebugFormat, ThemeSetting, UiSettings};
use amux_tui::{
    ColorPreference, Theme, ThemeError, TuiConfig, detect_color_mode, parse_theme_file, run_fleet,
    theme_from_file,
};
use amux_ui::{ConnectFailure, Connector, Runtime, RuntimeOptions};
use anyhow::{Context, Result};

use crate::client_common::get_client;
use crate::init::{self, InitContext};
use crate::update::MarkerFileReporter;

const GIT_SHA: &str = env!("GIT_SHA");

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
    // Debug builds record what the chrome saw so a captured report can be
    // replayed. The declaration is compile-gated because release builds do
    // not contain the trace module or carry its storage in TuiConfig.
    #[cfg(debug_assertions)]
    let trace = Some(amux_tui::trace::shared(amux_tui::trace::SEGMENT_LEN));
    // The fold order is the runtime's to report. Reconstructing it from
    // outside would mean guessing how a drain batched, and a wrong guess is
    // a replay that diverges for no visible reason.
    #[cfg(debug_assertions)]
    let msg_tap: Option<amux_ui::MsgTap> = trace.clone().map(|trace| {
        Box::new(move |msg: &amux_ui::Msg| {
            amux_tui::trace::record_shared(&trace, &amux_tui::chrome::TraceEvent::Msg(msg.clone()));
        }) as amux_ui::MsgTap
    });
    let mut runtime = Runtime::start(
        connector,
        RuntimeOptions {
            local_host_id,
            report_dir: Some(config.reports_dir()),
            log_path: Some(amux_cli::diagnostics::resolved_log_path()),
            git_sha: GIT_SHA,
            artifact_cache: Some(amux::default_cache_dir().join("artifacts")),
            artifact_cache_bound: config.ui.artifact_cache_mib.saturating_mul(1024 * 1024),
            subscription_status_provider: Some(Arc::new(move || {
                subscription_reporter.subscription_required()
            })),
            #[cfg(debug_assertions)]
            msg_tap,
            ..RuntimeOptions::default()
        },
    );
    // A panic anywhere in the TUI leaves a report: the terminal.rs panic
    // hook calls amux_ui::write_panic_report after restoring the
    // terminal.
    runtime.install_panic_report();

    // The dump is fetched when the key is pressed, not now: a report is
    // meant to explain the daemon's state at the moment something looked
    // wrong. A missing daemon is a reason string, not a failed capture.
    let dump_config = config.clone();
    let diagnostics =
        amux_cli::diagnostics::source(&config, GIT_SHA, cfg!(debug_assertions), move || {
            let config = dump_config.clone();
            async move {
                crate::server_client::debug(&config, true, DebugFormat::Json)
                    .await
                    .map_err(|error| format!("{error:#}"))
            }
        });

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
        default_agent_type: default_agent_type(&config),
        initial_chat,
        initial_chat_configuration,
        #[cfg(debug_assertions)]
        trace,
        diagnostics,
    };

    let attach_config = config.clone();
    run_fleet(&mut runtime, tui_config, move |agent| {
        let config = attach_config.clone();
        async move { crate::session_client::attach_for_ui(&config, agent).await }
    })
    .await
}

fn default_agent_type(config: &Config) -> amux::AgentType {
    amux::AgentType::Claude {
        driver: amux::resolve_claude_driver(None, config),
    }
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

    const BASE16_SAMPLE: &str = include_str!("../../amux-tui/tests/themes/base16-sample.yaml");

    fn env(colorterm: Option<&str>, no_color: bool) -> ColorEnv {
        ColorEnv {
            colorterm: colorterm.map(str::to_string),
            term: Some("xterm-256color".into()),
            no_color,
        }
    }

    #[test]
    fn claude_driver_tui_create_flow_uses_config() {
        assert_eq!(
            default_agent_type(&Config::default()),
            amux::AgentType::Claude {
                driver: amux::ClaudeDriver::Pty,
            }
        );

        let config: Config = serde_yaml::from_str("claude:\n  driver: sdk\n").unwrap();
        assert_eq!(
            default_agent_type(&config),
            amux::AgentType::Claude {
                driver: amux::ClaudeDriver::Sdk,
            }
        );
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
        let config = directory.path().join("amux.yaml");
        fs::write(&config, "ui:\n  theme: base16-sample.yaml\n").expect("write temporary config");
        fs::write(directory.path().join("base16-sample.yaml"), BASE16_SAMPLE)
            .expect("copy committed theme fixture beside config");
        let settings = UiSettings {
            theme: ThemeSetting::File(PathBuf::from("base16-sample.yaml")),
            color: ColorSetting::TrueColor,
            ..UiSettings::default()
        };

        let config_dir = config.parent().expect("temporary config has a parent");
        let theme = resolve_theme(&settings, config_dir, &ColorEnv::default())
            .expect("resolve committed sample relative to config");
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

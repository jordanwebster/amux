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
    ColorPreference, TerminalColors, Theme, ThemeError, TuiConfig, detect_color_mode,
    parse_theme_file, query_terminal_colors, run_fleet, theme_from_file,
};
use amux_ui::{ConnectFailure, Connector, Runtime, RuntimeOptions};
use anyhow::{Context, Result};

use crate::client_common::get_client;
use crate::init::{self, InitContext};
use crate::update::MarkerFileReporter;

const GIT_SHA: &str = env!("GIT_SHA");

/// How long to wait for a terminal to say what colours it is painting with.
/// Terminals that answer do so in a few milliseconds; the bound is for the
/// ones that never will, over a slow remote connection.
const TERMINAL_COLOR_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

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
    crate::profiles::remember_selection(&config)?;

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
    // Asked before the alternate screen is entered, because the answers
    // arrive on stdin and nothing else may be reading it yet. A terminal
    // that stays silent costs this much startup once and gets the shipped
    // palette.
    let terminal = match config.ui.theme {
        ThemeSetting::Terminal => query_terminal_colors(TERMINAL_COLOR_QUERY_TIMEOUT),
        _ => None,
    };
    let theme = resolve_theme(&config.ui, &config_dir, &ColorEnv::capture(), terminal)
        .context("failed to resolve ui.theme")?;

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
    let mut runtime = Runtime::start(
        connector,
        runtime_options(
            &config,
            #[cfg(debug_assertions)]
            trace.clone(),
        ),
    );
    // A panic anywhere in the TUI leaves a report: the terminal.rs panic
    // hook calls amux_ui::write_panic_report after restoring the
    // terminal.
    runtime.install_panic_report();

    let diagnostics = profile_diagnostics(&config);

    // Switching accounts rebuilds the runtime against the selected
    // profile's own configuration: its reports, its artifact cache, its
    // device identity. Reusing this profile's would file a report about the
    // account the person had just left.
    let installation = crate::front_door::configuration(config.path.as_deref())?;
    let profiles = Some(amux_tui::run::ProfileSwitching {
        front_door: installation.front_door_socket.clone(),
        current: config.socket_path.clone(),
        options: {
            #[cfg(debug_assertions)]
            let trace = trace.clone();
            Box::new(move |entry: &amux_ui::ProfileEntry| {
                let selected = crate::profiles::load(&crate::profiles::config_path_for(
                    &installation,
                    entry.id.0,
                ))?;
                crate::profiles::remember(
                    &crate::profiles::last_used(&installation),
                    &entry.id.0.to_string(),
                )?;
                Ok(amux_tui::run::ProfileOptions {
                    runtime: runtime_options(
                        &selected,
                        #[cfg(debug_assertions)]
                        trace.clone(),
                    ),
                    diagnostics: profile_diagnostics(&selected),
                })
            })
        },
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
        default_agent_type: amux::AgentType::Claude {
            driver: amux::ClaudeDriver::Pty,
        },
        initial_chat,
        initial_chat_configuration,
        profiles,
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

fn profile_diagnostics(config: &Config) -> Option<amux_tui::DiagnosticsSource> {
    // Fetch at the capture keypress, using this selection's configuration.
    // A missing daemon is a reason string, not a failed capture.
    let dump_config = config.clone();
    amux_cli::diagnostics::source(config, GIT_SHA, cfg!(debug_assertions), move || {
        let config = dump_config.clone();
        async move {
            crate::server_client::debug(&config, true, DebugFormat::Json)
                .await
                .map_err(|error| format!("{error:#}"))
        }
    })
}

/// What one profile's runtime is built from.
///
/// The same answer for the profile the fleet opens on and for every profile
/// the switcher moves to, so a switched account's reports, cached
/// attachments and subscription state are its own rather than inherited
/// from the account it replaced.
fn runtime_options(
    config: &Config,
    #[cfg(debug_assertions)] trace: Option<amux_tui::trace::SharedTrace>,
) -> RuntimeOptions {
    // The local host id comes from the stored device identity — the wire
    // does not mark the local host (see docs/UI.md, subscription policy).
    let local_host_id = amux::setup::local_host_id(config);
    let subscription_reporter = MarkerFileReporter::from_state_path(&config.state_path);
    // The fold order is the runtime's to report. Reconstructing it from
    // outside would mean guessing how a drain batched, and a wrong guess is
    // a replay that diverges for no visible reason.
    #[cfg(debug_assertions)]
    let msg_tap: Option<amux_ui::MsgTap> = trace.map(|trace| {
        Box::new(move |msg: &amux_ui::Msg| {
            amux_tui::trace::record_shared(&trace, &amux_tui::chrome::TraceEvent::Msg(msg.clone()));
        }) as amux_ui::MsgTap
    });
    RuntimeOptions {
        local_host_id,
        report_dir: Some(config.reports_dir()),
        log_path: Some(amux_cli::diagnostics::resolved_log_path()),
        git_sha: GIT_SHA,
        artifact_cache: Some(config.artifact_cache_dir()),
        artifact_cache_bound: config.ui.artifact_cache_mib.saturating_mul(1024 * 1024),
        subscription_status_provider: Some(Arc::new(move || {
            subscription_reporter.subscription_required()
        })),
        #[cfg(debug_assertions)]
        msg_tap,
        ..RuntimeOptions::default()
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

/// The palette for this session. `terminal` is what the terminal reported
/// about its own colours, when it was asked and answered; the `terminal`
/// setting derives from that and falls back to the shipped dark palette.
pub(crate) fn resolve_theme(
    settings: &UiSettings,
    config_dir: &Path,
    env: &ColorEnv,
    terminal: Option<TerminalColors>,
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
        ThemeSetting::Terminal => Ok(match terminal {
            Some(colors) => Theme::from_terminal(colors, mode),
            None => Theme::dark(mode),
        }),
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
            None,
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
            None,
        )
        .expect("resolve truecolor theme");
        assert_eq!(truecolor.mode, ColorMode::TrueColor);

        let ansi = resolve_theme(
            &settings,
            Path::new("/unused"),
            &env(Some("truecolor"), true),
            None,
        )
        .expect("resolve NO_COLOR theme");
        assert_eq!(ansi.mode, ColorMode::Ansi);
    }

    #[test]
    fn the_terminal_setting_derives_when_answered_and_ships_dark_when_not() {
        let settings = UiSettings::default();
        let silent = resolve_theme(
            &settings,
            Path::new("/unused"),
            &env(Some("truecolor"), false),
            None,
        )
        .expect("resolve the fallback");
        assert_eq!(silent.name, amux_tui::ThemeName::Dark);

        let reported = TerminalColors {
            background: (0xfd, 0xf6, 0xe3),
            foreground: (0x65, 0x7b, 0x83),
            ansi: [(0x80, 0x80, 0x80); 16],
        };
        let derived = resolve_theme(
            &settings,
            Path::new("/unused"),
            &env(Some("truecolor"), false),
            Some(reported),
        )
        .expect("derive from the terminal");
        assert_eq!(derived.name, amux_tui::ThemeName::Adopted);
        assert_eq!(derived.tokens.background.rgb, reported.background);
        assert_eq!(derived.tokens.text.rgb, reported.foreground);
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
        let theme = resolve_theme(&settings, config_dir, &ColorEnv::default(), None)
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
            resolve_theme(&settings, directory.path(), &ColorEnv::default(), None),
            Err(ThemeError::BadColor { key, value }) if key == "base00" && value == "nope"
        ));
    }
}

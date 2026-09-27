//! Bare `amux`: the terminal client on the selected profile.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use settings::{ColorSetting, InstallationConfig, ThemeSetting, UiSettings};
use tui::{
    ColorPreference, TerminalColors, Theme, ThemeError, TuiConfig, detect_color_mode,
    parse_theme_file, query_terminal_colors, theme_from_file,
};

/// How long to wait for a terminal to say what colours it paints with.
/// Terminals that answer do so in a few milliseconds; the bound is for the
/// ones that never will, over a slow remote connection.
const TERMINAL_COLOR_QUERY: Duration = Duration::from_millis(250);

pub async fn run(config: &InstallationConfig, profile: Option<&str>) -> Result<()> {
    let socket = crate::connect::client_socket(config, profile).await?;
    let client = client::GrpcClient::connect(&socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    // Asked before the alternate screen is entered, because the answers
    // arrive on stdin and nothing else may be reading it yet.
    let terminal = match config.ui.theme {
        ThemeSetting::Terminal => query_terminal_colors(TERMINAL_COLOR_QUERY),
        _ => None,
    };
    let theme = resolve_theme(&config.ui, &ColorEnv::capture(), terminal)
        .context("failed to resolve ui.theme")?;
    let tui_config = TuiConfig {
        working_dir: std::env::current_dir()?,
        leader: char::from(config.keybinds.leader.char),
        theme,
        initial_chat: None,
        attach: false,
    };
    tui::run(Arc::new(client), tui_config, None).await
}

/// The colour facts the environment states, read once at the edge.
pub struct ColorEnv {
    pub colorterm: Option<String>,
    pub term: Option<String>,
    pub no_color: bool,
}

impl ColorEnv {
    fn capture() -> ColorEnv {
        ColorEnv {
            colorterm: std::env::var("COLORTERM").ok(),
            term: std::env::var("TERM").ok(),
            no_color: std::env::var_os("NO_COLOR").is_some(),
        }
    }
}

/// The palette for this session. `terminal` is what the terminal reported
/// about its own colours, when it was asked and answered; the `terminal`
/// setting derives from that and falls back to the shipped dark palette.
pub fn resolve_theme(
    settings: &UiSettings,
    env: &ColorEnv,
    terminal: Option<TerminalColors>,
) -> Result<Theme, ThemeError> {
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
            let yaml = std::fs::read_to_string(path)?;
            theme_from_file(&parse_theme_file(&yaml)?, mode)
        }
    }
}

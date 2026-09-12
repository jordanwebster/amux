//! The chrome TUI — fleet, attention, create/rename/delete, host
//! state — around raw attach.
//!
//! A library the CLI invokes (bare `amux` opens it), never a second
//! executable. It consumes `ui-state` through `ui-runtime`: the renderer is a pure
//! function of (Model, ViewState, FrameContext), and every domain write
//! leaves as a Command through the runtime. `docs/UI.md` owns the design;
//! the golden-frame suite locks every screen.

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;

pub(crate) mod bindings;
pub mod chat;
pub mod chrome;
pub mod clipboard;
pub mod composer;
pub mod diagnostics;
mod hosts;
pub mod keys;
pub(crate) mod markdown;
pub mod render;
pub mod replay;
pub mod report_flow;
pub mod review;
pub mod run;
#[cfg(test)]
mod serde_roundtrip;
pub mod switcher;
pub mod terminal;
pub mod theme;
pub mod trace;
pub mod view;

pub use chat::{ChatView, PaintStats};
pub use diagnostics::{DaemonDump, DiagnosticsSource};
pub use render::{FrameContext, build_lines, render};
pub use review::{ReviewOutcome, ReviewView};
pub use run::{AttachReturn, TuiConfig, run_fleet};
pub use switcher::{SwitcherOutcome, SwitcherState};
pub use terminal::{
    TerminalGuard, install_panic_hook, query_terminal_colors, write_enter_chrome, write_osc52,
    write_restore,
};
pub use theme::{
    ColorMode, ColorPreference, TerminalColors, Theme, ThemeError, ThemeFile, ThemeName, Token,
    Tokens, Variant, detect_color_mode, nearest_ansi, parse_theme_file, theme_from_file,
};
pub use view::{Mode, Notice, NoticeTone, OpenMode, UiAction, ViewState};

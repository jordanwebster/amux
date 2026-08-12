//! amux-tui: the chrome TUI — fleet, attention, create/rename/delete, host
//! state — around raw attach.
//!
//! A library the CLI invokes (bare `amux` opens it), never a second
//! executable. It consumes `amux-ui` exclusively: the renderer is a pure
//! function of (Model, ViewState, FrameContext), and every domain write
//! leaves as a Command through the runtime. `docs/UI.md` owns the design;
//! the golden-frame suite locks every screen.

pub(crate) mod bindings;
pub mod chat;
pub mod keys;
pub mod render;
pub mod run;
pub mod terminal;
pub mod view;

pub use chat::ChatView;
pub use render::{FrameContext, Theme, build_lines, render};
pub use run::{AttachReturn, TuiConfig, run_fleet};
pub use terminal::{TerminalGuard, install_panic_hook, write_enter_chrome, write_restore};
pub use view::{Mode, OpenMode, UiAction, ViewState};

//! The fleet event loop: drain Msgs, fold, draw once — event-driven, no
//! unconditional periodic redraw. Attach suspends the chrome (leaves the
//! alternate screen, restores termios), runs the caller-provided passthrough
//! in-process, and resumes on detach with a repaint from the Model.

use std::io;
use std::time::Duration;

use amux_ui::{AgentId, Command, DumpReason, Runtime};
use anyhow::Result;
use chrono::Utc;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::keys::handle_key;
use crate::render::{FrameContext, Theme, list_capacity, render};
use crate::terminal::TerminalGuard;
use crate::view::{UiAction, ViewState, next_agent_name};

pub struct TuiConfig {
    /// Working directory for created agents (read at the CLI edge, not
    /// here — the TUI performs no environment reads).
    pub working_dir: std::path::PathBuf,
    /// Display label of the configured leader key ("C-a"), for the help
    /// overlay.
    pub leader_label: String,
    /// Default agent type for `n` (no form in V1).
    pub default_agent_type: amux_ui::AgentType,
}

enum ChromeExit {
    Quit,
    /// The shell went away (connection channel closed).
    RuntimeGone,
    Attach(AgentId),
}

/// Run the fleet until the user quits. `attach` is the raw-passthrough
/// handoff, provided by the embedding CLI — the TUI itself never touches
/// `amux::Client`. The terminal is restored before `attach` runs and the
/// chrome resumes (repainting from the Model) when it returns.
pub async fn run_fleet<F, Fut>(
    runtime: &mut Runtime,
    config: TuiConfig,
    mut attach: F,
) -> Result<()>
where
    F: FnMut(AgentId) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut view = ViewState {
        leader_label: config.leader_label.clone(),
        ..ViewState::default()
    };
    loop {
        match chrome_session(runtime, &mut view, &config).await? {
            ChromeExit::Quit | ChromeExit::RuntimeGone => return Ok(()),
            ChromeExit::Attach(agent) => {
                // Terminal is restored (the chrome session's guard dropped
                // before we got here); widen the subscription policy, then
                // hand the real terminal to the passthrough.
                runtime.note_attached(agent);
                if let Err(error) = attach(agent).await {
                    view.notice = Some(format!("attach failed: {error:#}"));
                }
            }
        }
    }
}

/// One alt-screen session: enter chrome, loop until quit or an attach
/// handoff, restore the terminal on every path out (RAII).
async fn chrome_session(
    runtime: &mut Runtime,
    view: &mut ViewState,
    config: &TuiConfig,
) -> Result<ChromeExit> {
    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut dirty = true;

    let exit = loop {
        if dirty {
            terminal.draw(|frame| {
                let area = frame.area();
                let ctx = FrameContext {
                    viewport: (area.width, area.height),
                    theme: Theme,
                    now: Utc::now(),
                };
                render(runtime.model(), view, &ctx, frame);
            })?;
            dirty = false;
        }
        tokio::select! {
            alive = runtime.next() => {
                if !alive {
                    break ChromeExit::RuntimeGone;
                }
                dirty = true;
            }
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) => {
                    dirty = true;
                    let capacity = list_capacity(terminal.size()?.height);
                    match handle_key(view, runtime.model(), key, capacity) {
                        Some(UiAction::Quit) => break ChromeExit::Quit,
                        Some(UiAction::Attach(agent)) => break ChromeExit::Attach(agent),
                        Some(UiAction::Dispatch(command)) => {
                            runtime.dispatch(command);
                        }
                        Some(UiAction::Create { host }) => {
                            let name = next_agent_name(runtime.model());
                            runtime.dispatch(Command::CreateAgent {
                                host,
                                name,
                                agent_type: config.default_agent_type.clone(),
                                working_dir: config.working_dir.clone(),
                            });
                        }
                        Some(UiAction::DebugDump) => {
                            match runtime.dump(DumpReason::UserRequested) {
                                Ok(path) => {
                                    view.notice = Some(format!("dumped to {}", path.display()));
                                }
                                Err(error) => {
                                    view.notice = Some(format!("dump failed: {error}"));
                                }
                            }
                        }
                        None => {}
                    }
                }
                Some(Ok(Event::Resize(..))) => {
                    dirty = true;
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
                None => break ChromeExit::Quit,
            },
            _ = ticker.tick() => {
                // Ticks are data for time-dependent display, scheduled only
                // while something on screen needs them (relative ages).
                if runtime.model().agent_count() > 0 {
                    runtime.observe_now(Utc::now());
                    dirty = true;
                }
            }
        }
    };
    drop(terminal);
    guard.restore();
    Ok(exit)
}

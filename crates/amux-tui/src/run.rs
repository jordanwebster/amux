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

/// What the embedding CLI's attach handoff decided: resume the fleet
/// (optionally with a status-line notice) or exit the TUI entirely
/// (`<leader>d` — detach means back to the shell).
#[derive(Debug)]
pub enum AttachReturn {
    Fleet(Option<String>),
    Exit,
}

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
/// chrome resumes (repainting from the Model) when it returns; a returned
/// notice ("session ended", …) surfaces in the status line.
pub async fn run_fleet<F, Fut>(
    runtime: &mut Runtime,
    config: TuiConfig,
    mut attach: F,
) -> Result<()>
where
    F: FnMut(AgentId) -> Fut,
    Fut: Future<Output = Result<AttachReturn>>,
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
                match attach(agent).await {
                    // `<leader>d`: detach means the shell, not the chrome.
                    Ok(AttachReturn::Exit) => return Ok(()),
                    Ok(AttachReturn::Fleet(notice)) => view.notice = notice,
                    Err(error) => view.notice = Some(format!("attach failed: {error:#}")),
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
                    theme: Theme::default(),
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
                // Reconcile chat view state against the fresh fold: a
                // finished send op may carry the failure fact that
                // resurfaces the draft (C5).
                if let Some(chat) = view.chat.as_mut() {
                    chat.reconcile(runtime.model());
                }
                dirty = true;
            }
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) => {
                    dirty = true;
                    let size = terminal.size()?;
                    let now = Utc::now();
                    // The chat screen, when open, owns the keys (its
                    // focus derivation differs from the fleet's modes);
                    // both handlers share the chrome-wide guarded Ctrl+C
                    // rule, each through its own QuitGuard.
                    let action = match view.chat.as_mut() {
                        Some(chat) => crate::chat::handle_chat_key(
                            chat,
                            runtime.model(),
                            key,
                            (size.width, size.height),
                            now,
                        ),
                        None => {
                            handle_key(view, runtime.model(), key, list_capacity(size.height), now)
                        }
                    };
                    match action {
                        Some(UiAction::Quit) => break ChromeExit::Quit,
                        Some(UiAction::Attach(agent)) => break ChromeExit::Attach(agent),
                        Some(UiAction::Dispatch(command)) => {
                            let op = runtime.dispatch(command.clone());
                            // The shell owns op identity; hand it back so
                            // the chat can watch the outcome (C5). Dispatch
                            // can finish synchronously (a reducer refusal,
                            // the disconnected fail-fast): reconcile NOW so
                            // a refused send resurfaces the draft without
                            // waiting for another runtime message.
                            if let Some(chat) = view.chat.as_mut() {
                                chat.note_dispatched(op, &command);
                                chat.reconcile(runtime.model());
                            }
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
                        Some(UiAction::CloseChat) => {
                            view.close_chat();
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
                Some(Ok(Event::Paste(text))) => {
                    // Bracketed paste (enabled with the chrome, restored
                    // with it): literal insertion into the focused chat
                    // text surface — newlines and tabs are text here,
                    // never bindings; read-only chats and docked panels
                    // without a field drop it.
                    if let Some(chat) = view.chat.as_mut() {
                        crate::chat::handle_chat_paste(chat, runtime.model(), &text);
                        dirty = true;
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
                // Ticks are data for time-dependent display (relative
                // ages; the chat working line's spinner and elapsed time).
                // The interval itself always fires; only the repaint is
                // gated on something on screen needing time — a deliberate
                // V1 simplification of "ticks scheduled only while
                // needed". One 1 Hz tick serves fleet and chat alike.
                let now = Utc::now();
                let model = runtime.model();
                let mut needed = match view.chat.as_ref() {
                    Some(chat) => chat.needs_tick(model),
                    None => model.fleet_agent_count() > 0,
                };
                // The quit guard's disarm check runs only while armed —
                // the arm tick of the gate extension; the disarm itself
                // owes a repaint (the warning footer must vanish).
                if view.quit_guard.expire(now) {
                    needed = true;
                }
                if let Some(chat) = view.chat.as_mut()
                    && chat.quit_guard.expire(now)
                {
                    needed = true;
                }
                if needed {
                    runtime.observe_now(now);
                    dirty = true;
                }
            }
        }
    };
    drop(terminal);
    guard.restore();
    Ok(exit)
}

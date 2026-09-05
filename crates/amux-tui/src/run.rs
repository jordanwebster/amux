//! The fleet event loop: drain Msgs, fold, draw once — event-driven, no
//! unconditional periodic redraw. Attach suspends the chrome (leaves the
//! alternate screen, restores termios), runs the caller-provided passthrough
//! in-process, and resumes on detach with a repaint from the Model.

use std::io;
use std::time::Duration;

use amux_ui::{AgentId, Command, Runtime};
use anyhow::Result;
use chrono::Utc;
use crossterm::event::EventStream;
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::Paragraph;

use crate::chrome::{Chrome, ChromeConfig, InputEvent, ShellEffect, TraceEvent};
use crate::diagnostics::DiagnosticsSource;
use crate::render::Theme;
#[cfg(any(debug_assertions, test))]
use crate::report_flow::{CAPTURE_KEY, FlowStep, Frozen, ReportFlow};
use crate::terminal::{TerminalGuard, write_osc52};
#[cfg(any(debug_assertions, test))]
use crate::trace::{SharedTrace, record_shared};
use crate::view::{Notice, ViewState, next_agent_name};

/// What the embedding CLI's attach handoff decided: resume the fleet
/// (optionally with a status-line notice) or exit the TUI entirely
/// (`<leader>d` — detach means back to the shell).
#[derive(Debug)]
pub enum AttachReturn {
    Fleet(Option<Notice>),
    Exit,
}

pub struct TuiConfig {
    /// Working directory for created agents (read at the CLI edge, not
    /// here — the TUI performs no environment reads).
    pub working_dir: std::path::PathBuf,
    /// The configured leader character (`a` for ctrl+a): labels the help
    /// overlay and composes the chat's leader chords.
    pub leader: char,
    /// The fully resolved palette; renderers never read terminal capability
    /// or theme files themselves.
    pub theme: Theme,
    /// The mode the fleet's Enter opens (A1), from the amux config's
    /// `ui.default_open_mode`.
    pub default_open_mode: crate::view::OpenMode,
    /// Default agent type for `n` (no form in V1).
    pub default_agent_type: amux_ui::AgentType,
    /// Agent to open directly once its inventory row arrives. Used by
    /// `amux new` when the configured default mode is structured chat.
    pub initial_chat: Option<AgentId>,
    /// Creation-time model, approval and sandbox facts for that initial
    /// chat, in the order its header states them.
    pub initial_chat_configuration: Option<Vec<String>>,
    /// The ring this session records into, shared with the runtime's Msg
    /// tap so folds and inputs interleave in the order they happened.
    /// The field itself is absent from builds that record nothing.
    #[cfg(any(debug_assertions, test))]
    pub trace: Option<SharedTrace>,
    /// The daemon dump, log path, reports directory and commit the shell
    /// contributes to a captured report. `None` in a build that captures
    /// none, which is what makes the capture key absent there.
    pub diagnostics: Option<DiagnosticsSource>,
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
    let view = ViewState {
        leader: config.leader,
        default_open_mode: config.default_open_mode,
        ..ViewState::default()
    };
    let mut chrome = Chrome::new(
        view,
        ChromeConfig {
            theme: config.theme,
        },
    );
    let mut initial_chat = config.initial_chat;
    let mut initial_chat_configuration = config.initial_chat_configuration.clone();
    loop {
        match chrome_session(
            runtime,
            &mut chrome,
            &config,
            &mut initial_chat,
            &mut initial_chat_configuration,
        )
        .await?
        {
            ChromeExit::Quit | ChromeExit::RuntimeGone => return Ok(()),
            ChromeExit::Attach(agent) => {
                // Terminal is restored (the chrome session's guard dropped
                // before we got here); widen the subscription policy, then
                // hand the real terminal to the passthrough.
                runtime.note_attached(agent);
                match attach(agent).await {
                    // `<leader>d`: detach means the shell, not the chrome.
                    Ok(AttachReturn::Exit) => return Ok(()),
                    Ok(AttachReturn::Fleet(notice)) => {
                        set_notice(runtime, &config, &mut chrome, notice)
                    }
                    Err(error) => set_notice(
                        runtime,
                        &config,
                        &mut chrome,
                        Some(Notice::problem(format!("attach failed: {error:#}"))),
                    ),
                }
            }
        }
    }
}

/// One alt-screen session: enter chrome, loop until quit or an attach
/// handoff, restore the terminal on every path out (RAII).
async fn chrome_session(
    runtime: &mut Runtime,
    chrome: &mut Chrome,
    config: &TuiConfig,
    initial_chat: &mut Option<AgentId>,
    initial_chat_configuration: &mut Option<Vec<String>>,
) -> Result<ChromeExit> {
    let guard = TerminalGuard::enter()?;
    // The guard probed for the kitty keyboard protocol on the way in;
    // hints and the `?` overlay derive from the effective tier (P10 —
    // hints advertise only what works).
    chrome.view.kitty = crate::terminal::kitty_active();
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A quit or an attach leaves the loop, but it arrives as one effect
    // among several a single step returned. Park it here and break once
    // the whole batch has been performed, so a copy or a dispatch queued
    // behind it is not silently dropped.
    let mut exit_request: Option<ChromeExit> = None;
    // The buffer as the terminal last received it. A capture has to
    // describe what the person was looking at, and by the time the key
    // arrives the renderer has moved on from the state that produced it.
    // Only a build with the capture key ever reads it, so only that build
    // pays for the copy: it stays `None` everywhere else.
    let mut last_frame: Option<ratatui::buffer::Buffer> = None;

    let exit = loop {
        if chrome.take_dirty() {
            // The draw is a chrome step, not a call into the renderer: it
            // fills the chat's paint caches, which the next keypress reads.
            // Only the painting of the finished lines is ours.
            let size = terminal.size()?;
            let now = Utc::now();
            let event = TraceEvent::Draw {
                viewport: (size.width, size.height),
                now,
            };
            #[cfg(any(debug_assertions, test))]
            if let Some(trace) = config.trace.as_ref() {
                // Roll first: the snapshot a new segment starts from is
                // state as of this frame boundary, and the draw below is
                // the first thing replayed from it.
                match trace.lock() {
                    Ok(mut ring) => {
                        ring.roll_if_due(runtime.model(), &chrome.view, chrome.theme(), now)
                    }
                    Err(_) => tracing::warn!("trace segment not rolled: ring lock poisoned"),
                }
                record_shared(trace, &event);
            }
            chrome.step(runtime.model(), &event);
            if let Some(lines) = chrome.take_frame() {
                let completed = terminal.draw(|frame| {
                    frame.render_widget(Paragraph::new(lines), frame.area());
                })?;
                if cfg!(any(debug_assertions, test)) {
                    last_frame = Some(completed.buffer.clone());
                }
            }
        }
        tokio::select! {
            alive = runtime.next() => {
                if !alive {
                    break ChromeExit::RuntimeGone;
                }
                if let Some(agent) = *initial_chat
                    && runtime.model().agent(agent).is_some()
                {
                    let event = TraceEvent::ChatOpened {
                        agent,
                        codex_configuration: initial_chat_configuration.take(),
                    };
                    record(config, &event);
                    let effects = chrome.step(runtime.model(), &event);
                    perform(runtime, config, chrome, effects, &mut exit_request)?;
                    *initial_chat = None;
                }
                record(config, &TraceEvent::Drained);
                chrome.step(runtime.model(), &TraceEvent::Drained);
            }
            maybe_event = events.next() => match maybe_event {
                Some(Ok(event)) => {
                    if let Some(frozen) = capture(&event, config, runtime, chrome, last_frame.as_ref()) {
                        // The flow owns the screen and the event stream
                        // until it is answered; nothing it does reaches
                        // the chrome, the runtime or the trace.
                        let notice = report_flow(
                            &mut terminal,
                            &mut events,
                            frozen,
                            chrome.theme(),
                            config,
                        ).await?;
                        if let Some(notice) = notice {
                            set_notice(runtime, config, chrome, Some(notice));
                        }
                        // The frozen frame is still on the terminal, and a
                        // cancelled flow changed no view state to make the
                        // chrome dirty by itself. Owe the repaint here, so
                        // the next turn draws live state over it either way.
                        chrome.mark_dirty();
                        continue;
                    }
                    if let Some(input) = InputEvent::from_terminal(&event) {
                        let size = terminal.size()?;
                        let event = TraceEvent::Input {
                            event: input,
                            viewport: (size.width, size.height),
                            now: Utc::now(),
                        };
                        record(config, &event);
                        let effects = chrome.step(runtime.model(), &event);
                        perform(runtime, config, chrome, effects, &mut exit_request)?;
                    }
                }
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
                let mut needed = match chrome.view.chat.as_ref() {
                    Some(chat) => chat.needs_tick(model),
                    None => model.fleet_agent_count() > 0,
                };
                // The quit guard's disarm check runs only while armed —
                // the arm tick of the gate extension; the disarm itself
                // owes a repaint (the warning footer must vanish). The
                // expiry is view state, so it goes through a step and is
                // recorded; a tick that disarmed nothing changed nothing
                // and stays out of the ring, where it would otherwise
                // push a quiet session's history out one event a second.
                if chrome.quit_guard_armed() {
                    let event = TraceEvent::Tick { now };
                    chrome.step(model, &event);
                    let expired = !chrome.quit_guard_armed();
                    if expired {
                        record(config, &event);
                    }
                    needed |= expired;
                }
                if needed {
                    runtime.observe_now(now);
                    chrome.mark_dirty();
                }
            }
        }
        if let Some(exit) = exit_request.take() {
            break exit;
        }
    };
    drop(terminal);
    guard.restore();
    Ok(exit)
}

/// Perform the effects one step asked for, in order, recording what the
/// runtime answered as the next event. A dispatch is the case that matters:
/// the op id is the runtime's to mint, so it enters the chrome as its own
/// [`TraceEvent::Dispatched`] rather than being guessed.
fn perform(
    runtime: &mut Runtime,
    config: &TuiConfig,
    chrome: &mut Chrome,
    effects: Vec<ShellEffect>,
    exit_request: &mut Option<ChromeExit>,
) -> Result<()> {
    for effect in effects {
        match effect {
            ShellEffect::Quit => *exit_request = Some(ChromeExit::Quit),
            ShellEffect::Attach(agent) => *exit_request = Some(ChromeExit::Attach(agent)),
            ShellEffect::NoteAttached(agent) => runtime.note_attached(agent),
            ShellEffect::WriteClipboard(text) => {
                // A truncated copy is not the copy that was asked for,
                // so it reads as a problem even though bytes did land.
                let notice = match write_osc52(&mut io::stdout(), &text)? {
                    Some(truncated) => Notice::problem(truncated),
                    None => Notice::done("copied message to clipboard"),
                };
                set_notice(runtime, config, chrome, Some(notice));
            }
            ShellEffect::Dispatch(command) => {
                let op = runtime.dispatch(command.clone());
                let event = TraceEvent::Dispatched { op, command };
                record(config, &event);
                chrome.step(runtime.model(), &event);
                if let Some(chat) = chrome.view.chat.as_mut() {
                    chat.composer_mut()
                        .hydrate_queued_attachments(|id| runtime.queued_attachment_bytes(id));
                }
            }
            ShellEffect::Create { host } => {
                let name = next_agent_name(runtime.model(), &config.default_agent_type);
                runtime.dispatch(Command::CreateAgent {
                    host,
                    name,
                    agent_type: config.default_agent_type.clone(),
                    working_dir: config.working_dir.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Run the report prompt over the frozen frame until it is answered,
/// returning the notice the status line should carry. The loop's own
/// select! is left behind deliberately: no Msg is folded, no tick fires
/// and no event is recorded while the flow is up, which is what keeps the
/// act of reporting out of the recording being reported.
#[cfg(any(debug_assertions, test))]
async fn report_flow(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    events: &mut EventStream,
    frozen: Frozen,
    theme: Theme,
    config: &TuiConfig,
) -> Result<Option<Notice>> {
    let Some(diagnostics) = config.diagnostics.as_ref() else {
        return Ok(None);
    };
    let writer = amux_ui::report::ReportWriter::new(
        diagnostics.reports_dir.clone(),
        amux_ui::BUILD,
        diagnostics.git_sha,
    );
    let mut flow = ReportFlow::begin(frozen, theme);
    loop {
        terminal.draw(|frame| flow.render(frame))?;
        let Some(event) = events.next().await else {
            // The terminal went away mid-flow: there is nobody left to
            // show a notice to, and half an answer is not a report.
            return Ok(None);
        };
        match flow.handle(event?) {
            FlowStep::Continue => {}
            FlowStep::Cancel => return Ok(None),
            FlowStep::Finish(draft) => {
                // One more paint so the prompt says what is happening
                // while the daemon dump and the self-replay are awaited.
                terminal.draw(|frame| flow.render(frame))?;
                return Ok(Some(match flow.finish(draft, &writer).await {
                    Ok(path) => Notice::done(format!("wrote {}", path.display())),
                    Err(error) => {
                        Notice::problem(format!("the report could not be written: {error}"))
                    }
                }));
            }
        }
    }
}

/// A build that captures no reports never has a frozen screen to prompt
/// over: the capture above cannot produce one, and this says so in the
/// type rather than in a second copy of the event loop.
#[cfg(not(any(debug_assertions, test)))]
async fn report_flow(
    _terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    _events: &mut EventStream,
    frozen: std::convert::Infallible,
    _theme: Theme,
    _config: &TuiConfig,
) -> Result<Option<Notice>> {
    match frozen {}
}

/// Freeze everything a report needs, if this event is the capture key and
/// this build can capture at all. Deliberately ahead of both key handlers:
/// the key means the same thing on the fleet and inside a chat, and a
/// handler that swallowed it first would make that depend on the screen.
#[cfg(any(debug_assertions, test))]
fn capture(
    event: &crossterm::event::Event,
    config: &TuiConfig,
    runtime: &Runtime,
    chrome: &Chrome,
    last_frame: Option<&ratatui::buffer::Buffer>,
) -> Option<Frozen> {
    let crossterm::event::Event::Key(key) = event else {
        return None;
    };
    if key.code != CAPTURE_KEY.code || key.modifiers != CAPTURE_KEY.modifiers {
        return None;
    }
    let diagnostics = config.diagnostics.as_ref()?;
    let trace = config.trace.as_ref()?;
    let last_frame = last_frame?;
    Some(Frozen::take(
        last_frame,
        chrome.theme(),
        trace,
        runtime,
        diagnostics,
        Utc::now(),
    ))
}

/// A build that captures no reports has no capture key: the event falls
/// through to the ordinary handlers.
#[cfg(not(any(debug_assertions, test)))]
fn capture(
    _event: &crossterm::event::Event,
    _config: &TuiConfig,
    _runtime: &Runtime,
    _chrome: &Chrome,
    _last_frame: Option<&ratatui::buffer::Buffer>,
) -> Option<std::convert::Infallible> {
    None
}

/// Put a status-line notice up the way every other view change happens:
/// as a recorded event through the chrome's one mutation path. The shell
/// is what learned the outcome — an attach that ended, a clipboard write,
/// a written report — but writing the view directly would leave the
/// status line out of the trace, and a replay would draw the frame
/// without it.
fn set_notice(runtime: &Runtime, config: &TuiConfig, chrome: &mut Chrome, notice: Option<Notice>) {
    let event = TraceEvent::Notice(notice);
    record(config, &event);
    chrome.step(runtime.model(), &event);
}

/// Append one event to this session's ring, if it has one.
#[cfg(any(debug_assertions, test))]
fn record(config: &TuiConfig, event: &TraceEvent) {
    if let Some(trace) = config.trace.as_ref() {
        record_shared(trace, event);
    }
}

/// Release builds still step the chrome through the same events, but do
/// not retain them or carry trace storage in their configuration.
#[cfg(not(any(debug_assertions, test)))]
fn record(_config: &TuiConfig, _event: &TraceEvent) {}

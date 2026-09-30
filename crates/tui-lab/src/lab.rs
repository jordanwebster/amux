//! The interactive lab: the terminal client over a scenario, in the person's
//! own terminal, with a few keys of its own layered over the client's.
//!
//! - F2 or Ctrl+] f: capture the screen with a one-line note.
//! - F3 or Ctrl+] v: cycle the design variant.
//! - Ctrl+] r: restart the scenario from its beginning.
//! - Ctrl+] ?: list these keys.
//!
//! Under `tui-lab watch`, SIGUSR1 asks the lab to save its place and exit
//! for a relaunch on the new build, and SIGUSR2 says a rebuild failed.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt as _;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph};
use tokio::sync::watch;
use tui::{Flow, TerminalGuard, Theme, Tone};

use crate::boot::{boot, restore_chat};
use crate::place::{self, Place};
use crate::scenario::Scenario;

/// How many design variants F3 cycles through.
const VARIANTS: u8 = 4;

pub enum Leave {
    /// The person quit.
    Quit,
    /// Relaunch on a new build, from the saved place.
    Relaunch,
    /// Start the scenario again from its beginning.
    Reset,
}

enum Mode {
    Normal,
    /// Ctrl+] was pressed; the next key is the lab's.
    Chord,
    /// Typing a note for the frame captured when F2 was pressed.
    Note {
        frame: Buffer,
        text: String,
    },
}

fn lab_chord(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // A legacy terminal sends Ctrl+] as 0x1D, which crossterm reads as
    // Ctrl+5; the kitty protocol reports it as itself.
    ctrl && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
}

const HELP: &str = "lab: F2 feedback · F3 variant · ^] r restart scenario · ^] ? keys";

pub async fn run(scenario: &Scenario, place: Option<Place>, theme: Theme) -> Result<Leave> {
    // The client's layout lives with the lab's other state, so it outlasts
    // a relaunch as it outlasts a real client's restart.
    let layout = Some(place::dir().join("layout.json"));
    let mut booted = boot(scenario, place.as_ref(), false, theme, layout).await?;
    booted.world.start_timeline();
    let world = booted.world.clone();
    let app = &mut booted.app;
    let mut restore = place.clone();
    app.notice(format!("{} · {HELP}", scenario.name), Tone::Info);

    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut events = EventStream::new();
    let mut fleet_changed = app.fleet.changed();
    let mut chat: Option<(Vec<u8>, watch::Receiver<()>)> = None;
    let mut mode = Mode::Normal;
    let mut last = Buffer::empty(Rect::default());
    #[cfg(unix)]
    let mut relaunch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
    #[cfg(unix)]
    let mut build_failed =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined2())?;
    // Time moves in a scenario even when nothing on screen asks for a tick.
    let mut heartbeat = tokio::time::interval(Duration::from_millis(500));

    let leave = 'lab: loop {
        app.housekeep();
        if let Some(saved) = &restore
            && restore_chat(app, saved)
        {
            restore = None;
        }
        let open = app.chat.as_ref().map(|chat| chat.view.agent_id.clone());
        if chat.as_ref().map(|(id, _)| id) != open.as_ref() {
            chat = app
                .chat
                .as_ref()
                .map(|open| (open.view.agent_id.clone(), open.session.changed()));
        }
        let completed = terminal.draw(|frame| {
            app.draw(frame);
            if let Mode::Note { text, .. } = &mode {
                note_overlay(frame, text);
            }
        })?;
        if !matches!(mode, Mode::Note { .. }) {
            last = completed.buffer.clone();
        }
        let tick = app.next_tick();

        #[cfg(unix)]
        let signal = async {
            tokio::select! {
                _ = relaunch.recv() => Signal::Relaunch,
                _ = build_failed.recv() => Signal::BuildFailed,
            }
        };
        #[cfg(not(unix))]
        let signal = std::future::pending::<Signal>();

        let flow = tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => {
                    let Some(mut flow) =
                        lab_event(app, &mut mode, &last, event, theme, scenario, world.fired())
                    else {
                        break Leave::Reset;
                    };
                    // Input already waiting (a trackpad's burst of wheel
                    // events) applies before the one redraw.
                    while matches!(flow, Flow::Continue)
                        && let Some(event) = tui::run::ready(&mut events).await
                    {
                        match lab_event(app, &mut mode, &last, event?, theme, scenario, world.fired())
                        {
                            Some(next) => flow = next,
                            None => break 'lab Leave::Reset,
                        }
                    }
                    flow
                }
                Some(Err(error)) => return Err(error.into()),
                None => Flow::Quit,
            },
            changed = fleet_changed.changed() => {
                if changed.is_err() {
                    Flow::Quit
                } else {
                    app.fleet_changed();
                    Flow::Continue
                }
            }
            _ = async {
                match chat.as_mut() {
                    Some((_, changed)) => { let _ = changed.changed().await; }
                    None => std::future::pending::<()>().await,
                }
            } => Flow::Continue,
            Some(event) = app.receiver.recv() => app.event(event),
            _ = async {
                match tick {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                app.tick();
                Flow::Continue
            }
            _ = heartbeat.tick() => Flow::Continue,
            signal = signal => match signal {
                Signal::Relaunch => break Leave::Relaunch,
                Signal::BuildFailed => {
                    app.notice(build_failure(), Tone::Warn);
                    Flow::Continue
                }
            },
        };
        match flow {
            Flow::Continue | Flow::Attach(_) => {}
            Flow::Quit => break Leave::Quit,
        }
    };
    let saved = Place::of(app, &scenario.name, world.fired());
    if !matches!(leave, Leave::Reset) {
        let _ = saved.save();
    }
    drop(events);
    drop(terminal);
    guard.restore();
    Ok(leave)
}

// Only Unix delivers these; elsewhere the lab never hears from a watcher.
#[cfg_attr(not(unix), allow(dead_code))]
enum Signal {
    Relaunch,
    BuildFailed,
}

enum LabKey {
    Pass,
    Handled,
    Reset,
}

/// One input event: the lab's own keys first, the note being typed, then
/// the client. None asks for the scenario to restart.
fn lab_event(
    app: &mut tui::App,
    mode: &mut Mode,
    last: &Buffer,
    event: Event,
    theme: Theme,
    scenario: &Scenario,
    fired: usize,
) -> Option<Flow> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            match lab_key(app, mode, last, key, theme, scenario, fired) {
                LabKey::Pass => Some(app.input(Event::Key(key))),
                LabKey::Handled => Some(Flow::Continue),
                LabKey::Reset => None,
            }
        }
        event => {
            if matches!(mode, Mode::Note { .. }) {
                if let Event::Paste(pasted) = &event
                    && let Mode::Note { text, .. } = mode
                {
                    text.push_str(pasted);
                }
                Some(Flow::Continue)
            } else {
                Some(app.input(event))
            }
        }
    }
}

fn lab_key(
    app: &mut tui::App,
    mode: &mut Mode,
    last: &Buffer,
    key: KeyEvent,
    theme: Theme,
    scenario: &Scenario,
    fired: usize,
) -> LabKey {
    match mode {
        Mode::Note { frame, text } => {
            match key.code {
                KeyCode::Enter => {
                    let place = Place::of(app, &scenario.name, fired);
                    match place::save_feedback(frame, theme, &place, text.trim()) {
                        Ok(dir) => {
                            app.notice(format!("feedback saved: {}", dir.display()), Tone::Info)
                        }
                        Err(error) => {
                            app.notice(format!("feedback not saved: {error:#}"), Tone::Warn)
                        }
                    }
                    *mode = Mode::Normal;
                }
                KeyCode::Esc => {
                    app.notice("feedback discarded", Tone::Info);
                    *mode = Mode::Normal;
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
                _ => {}
            }
            LabKey::Handled
        }
        Mode::Chord => {
            *mode = Mode::Normal;
            match key.code {
                KeyCode::Char('f') => {
                    *mode = Mode::Note {
                        frame: last.clone(),
                        text: String::new(),
                    };
                    LabKey::Handled
                }
                KeyCode::Char('v') => {
                    cycle_variant(app);
                    LabKey::Handled
                }
                KeyCode::Char('r') => LabKey::Reset,
                _ => {
                    app.notice(HELP, Tone::Info);
                    LabKey::Handled
                }
            }
        }
        Mode::Normal => match key.code {
            KeyCode::F(2) => {
                *mode = Mode::Note {
                    frame: last.clone(),
                    text: String::new(),
                };
                LabKey::Handled
            }
            KeyCode::F(3) => {
                cycle_variant(app);
                LabKey::Handled
            }
            _ if lab_chord(&key) => {
                *mode = Mode::Chord;
                app.notice(
                    "lab: f feedback · v variant · r restart scenario",
                    Tone::Info,
                );
                LabKey::Handled
            }
            _ => LabKey::Pass,
        },
    }
}

fn cycle_variant(app: &mut tui::App) {
    let next = (tui::variant::get() + 1) % VARIANTS;
    tui::variant::set(next);
    app.notice(format!("variant {next}"), Tone::Info);
}

fn note_overlay(frame: &mut ratatui::Frame<'_>, text: &str) {
    let area = frame.area();
    if area.height == 0 {
        return;
    }
    let bar = Rect {
        x: area.x,
        y: area.bottom() - 1,
        width: area.width,
        height: 1,
    };
    frame.render_widget(Clear, bar);
    let line = Line::from(format!(
        " feedback on this screen: {text}▏   enter saves · esc drops"
    ));
    frame.render_widget(
        Paragraph::new(line).style(Style::default().add_modifier(Modifier::REVERSED)),
        bar,
    );
}

fn build_failure() -> String {
    let log = std::fs::read_to_string(place::build_log()).unwrap_or_default();
    let first_error = log
        .lines()
        .find(|line| line.starts_with("error"))
        .unwrap_or("see the build log");
    format!(
        "rebuild failed: {first_error} ({})",
        place::build_log().display()
    )
}

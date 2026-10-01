//! Headless frames: a scenario at a timeline step, after a sequence of keys,
//! drawn at one or more sizes to text and PNG. Scripts play at once here,
//! so a frame does not depend on timing.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tui::Theme;

use crate::boot::{boot, restore_chat, seed_draft, settle};
use crate::place::{Place, write_frame};
use crate::scenario::Scenario;

const QUIET: Duration = Duration::from_millis(150);

pub struct Request<'a> {
    pub scenario: &'a Scenario,
    pub step: usize,
    pub sizes: Vec<(u16, u16)>,
    /// Keys and mouse events delivered before drawing, and pauses.
    pub keys: Vec<Step>,
    pub out: &'a Path,
    pub theme: Theme,
    pub variant: u8,
    /// A client layout file to start from, as a relaunch would.
    pub layout: Option<std::path::PathBuf>,
    /// The agent whose chat to open, instead of the scenario's.
    pub open: Option<String>,
}

pub async fn render(request: Request<'_>) -> Result<Vec<String>> {
    // A frame is drawn once, so its code is highlighted from the start.
    let loaded = tui::highlight::preload();
    if std::env::var_os("LAB_TIMING").is_some() {
        let block = "/// A tunnel with no frames for this long is closed.\nconst IDLE_TIMEOUT: Duration = Duration::from_secs(90);\n\nif last_frame.elapsed() > IDLE_TIMEOUT {\n    tunnel.close(Reason::Idle).await;\n}\n";
        eprintln!(
            "grammars loaded in {loaded:?}; first rust block in {:?}",
            tui::highlight::time_block(block, "rust")
        );
    }
    let place = Place {
        scenario: request.scenario.name.clone(),
        fired: request.step,
        open: request
            .open
            .clone()
            .or_else(|| request.scenario.open.clone()),
        variant: request.variant,
        ..Place::default()
    };
    let mut booted = boot(
        request.scenario,
        Some(&place),
        true,
        request.theme,
        request.layout.clone(),
    )
    .await?;
    let world = booted.world.clone();
    let app = &mut booted.app;
    let (width, height) = request.sizes.first().copied().unwrap_or((120, 40));
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    draw_settle(app, &world, &mut terminal, &place).await?;
    for step in &request.keys {
        match step {
            Step::Input(event) => {
                let _ = app.input(event.clone());
            }
            Step::Wait(pause) => {
                let until = tokio::time::Instant::now() + *pause;
                while tokio::time::Instant::now() < until {
                    settle(app, QUIET).await;
                }
            }
        }
        draw_settle(app, &world, &mut terminal, &place).await?;
    }
    let mut written = Vec::new();
    for (width, height) in request.sizes {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| app.draw(frame))?;
        settle(app, QUIET).await;
        let frame = terminal.draw(|frame| app.draw(frame))?.buffer.clone();
        let stem = format!(
            "{}-step{}-{width}x{height}",
            request.scenario.name, request.step
        );
        write_frame(&frame, request.theme, request.out, &stem)?;
        written.push(stem);
    }
    Ok(written)
}

/// Drawing asks for older rows and opens chats, so draw and settle a few
/// times until the frame has what it asked for.
async fn draw_settle(
    app: &mut tui::App,
    world: &crate::world::World,
    terminal: &mut Terminal<TestBackend>,
    place: &Place,
) -> Result<()> {
    for _ in 0..3 {
        settle(app, QUIET).await;
        restore_chat(app, place);
        seed_draft(app, world);
        terminal.draw(|frame| app.draw(frame))?;
    }
    Ok(())
}

pub fn parse_size(text: &str) -> Result<(u16, u16)> {
    let Some((w, h)) = text.split_once('x') else {
        bail!("a size is WIDTHxHEIGHT, like 120x40: {text:?}");
    };
    Ok((w.parse()?, h.parse()?))
}

/// One step of a render's input: an event, or time passing (`wait:MS`), so
/// a frame can show what the world does a while later.
#[derive(Clone, Debug)]
pub enum Step {
    Input(Event),
    Wait(Duration),
}

/// Keys written like a shell line: named keys (`enter`, `esc`, `tab`,
/// `backtab`, `up`, `down`, `left`, `right`, `pgup`, `pgdn`, `home`, `end`,
/// `bs`, `del`, `space`, `f1`..`f12`), `C-x` for Ctrl+x and `C-enter`,
/// `hover:X,Y` and `click:X,Y` for the mouse at a cell (zero-based),
/// `wheelup` and `wheeldown` for one wheel event (`wheeldown:X,Y` over a
/// cell), `wait:MS` to let time pass, and anything else typed
/// as text (quote it to keep spaces:
/// `'fix the bug' enter`).
pub fn parse_keys(text: &str) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    for word in shell_words::split(text)? {
        if let Some(ms) = word.strip_prefix("wait:") {
            steps.push(Step::Wait(Duration::from_millis(ms.parse()?)));
            continue;
        }
        steps.extend(
            parse_events(&shell_words::quote(&word))?
                .into_iter()
                .map(Step::Input),
        );
    }
    Ok(steps)
}

fn parse_events(text: &str) -> Result<Vec<Event>> {
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let mut out = Vec::new();
    for word in shell_words::split(text)? {
        if let Some((what, at)) = word.split_once(':')
            && let Some((x, y)) = at.split_once(',')
            && matches!(what, "hover" | "click" | "wheelup" | "wheeldown")
        {
            let kind = match what {
                "hover" => MouseEventKind::Moved,
                "wheelup" => MouseEventKind::ScrollUp,
                "wheeldown" => MouseEventKind::ScrollDown,
                _ => MouseEventKind::Down(MouseButton::Left),
            };
            out.push(Event::Mouse(MouseEvent {
                kind,
                column: x.parse()?,
                row: y.parse()?,
                modifiers: KeyModifiers::NONE,
            }));
            continue;
        }
        if let Some(kind) = match word.as_str() {
            "wheelup" => Some(MouseEventKind::ScrollUp),
            "wheeldown" => Some(MouseEventKind::ScrollDown),
            _ => None,
        } {
            out.push(Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }));
            continue;
        }
        if word == "C-enter" {
            out.push(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL,
            )));
            continue;
        }
        let named = match word.as_str() {
            "enter" => Some(KeyCode::Enter),
            "esc" => Some(KeyCode::Esc),
            "tab" => Some(KeyCode::Tab),
            "backtab" => Some(KeyCode::BackTab),
            "up" => Some(KeyCode::Up),
            "down" => Some(KeyCode::Down),
            "left" => Some(KeyCode::Left),
            "right" => Some(KeyCode::Right),
            "pgup" => Some(KeyCode::PageUp),
            "pgdn" => Some(KeyCode::PageDown),
            "home" => Some(KeyCode::Home),
            "end" => Some(KeyCode::End),
            "bs" => Some(KeyCode::Backspace),
            "del" => Some(KeyCode::Delete),
            "space" => Some(KeyCode::Char(' ')),
            w if w.len() > 1
                && w.starts_with('f')
                && w[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
            {
                Some(KeyCode::F(w[1..].parse().unwrap()))
            }
            _ => None,
        };
        if let Some(code) = named {
            out.push(key(code));
        } else if let Some(rest) = word.strip_prefix("C-")
            && let Some(c) = rest.chars().next()
            && rest.chars().count() == 1
        {
            out.push(Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::CONTROL,
            )));
        } else {
            out.extend(word.chars().map(|c| key(KeyCode::Char(c))));
        }
    }
    Ok(out)
}

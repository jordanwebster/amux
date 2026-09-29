//! Headless frames: a scenario at a timeline step, after a sequence of keys,
//! drawn at one or more sizes to text and PNG. Scripts play at once here,
//! so a frame does not depend on timing.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tui::Theme;

use crate::boot::{boot, restore_chat, settle};
use crate::place::{Place, write_frame};
use crate::scenario::Scenario;

const QUIET: Duration = Duration::from_millis(150);

pub struct Request<'a> {
    pub scenario: &'a Scenario,
    pub step: usize,
    pub sizes: Vec<(u16, u16)>,
    pub keys: Vec<KeyEvent>,
    pub out: &'a Path,
    pub theme: Theme,
}

pub async fn render(request: Request<'_>) -> Result<Vec<String>> {
    let place = Place {
        scenario: request.scenario.name.clone(),
        fired: request.step,
        open: request.scenario.open.clone(),
        ..Place::default()
    };
    let mut booted = boot(request.scenario, Some(&place), true, request.theme).await?;
    let app = &mut booted.app;
    let (width, height) = request.sizes.first().copied().unwrap_or((120, 40));
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    draw_settle(app, &mut terminal, &place).await?;
    for key in &request.keys {
        let _ = app.input(Event::Key(*key));
        draw_settle(app, &mut terminal, &place).await?;
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
    terminal: &mut Terminal<TestBackend>,
    place: &Place,
) -> Result<()> {
    for _ in 0..3 {
        settle(app, QUIET).await;
        restore_chat(app, place);
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

/// Keys written like a shell line: named keys (`enter`, `esc`, `tab`,
/// `backtab`, `up`, `down`, `left`, `right`, `pgup`, `pgdn`, `home`, `end`,
/// `bs`, `del`, `space`, `f1`..`f12`), `C-x` for Ctrl+x, and anything else
/// typed as text (quote it to keep spaces: `'fix the bug' enter`).
pub fn parse_keys(text: &str) -> Result<Vec<KeyEvent>> {
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let mut out = Vec::new();
    for word in shell_words::split(text)? {
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
            out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        } else {
            out.extend(word.chars().map(|c| key(KeyCode::Char(c))));
        }
    }
    Ok(out)
}

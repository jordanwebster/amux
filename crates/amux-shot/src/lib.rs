//! Deterministic raster captures of named `amux-tui` fixtures.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use amux_tui::chat::{FeedScroll, handle_chat_mouse};
use amux_tui::clipboard::ClipboardContent;
use amux_tui::fixtures::{
    Fixture, NamedState, ScriptStep, apply_step, fixture, long_feed, recording_start,
};
use amux_tui::{ColorMode, FrameContext, Theme, render};
use amux_ui::{DiffBase, StructuredProtocol};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use fontdue::{Font, FontSettings};
use gif::{Encoder as GifEncoder, Frame as GifFrame, Repeat};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The fixed terminal viewport used by every capture.
pub const VIEWPORT: (u16, u16) = (120, 40);
/// Pixel width of one terminal cell.
pub const CELL_WIDTH: u32 = 10;
/// Pixel height of one terminal cell. Glyphs are clipped to their own cell,
/// so the cell must contain the font's whole vertical extent or every row
/// loses the tips of its ascenders and the tails of its descenders. At
/// `FONT_SIZE`, JetBrains Mono asks for 16.32px above the baseline and 4.80px
/// below it, which needs 22 rather than the 20 an even 16px cell suggests.
pub const CELL_HEIGHT: u32 = 22;

const FONT_SIZE: f32 = 16.0;
/// Distance from the top of a cell to the text baseline, leaving whole pixels
/// for the ascent above it and the descent below it.
const BASELINE: i32 = 17;
const REGULAR_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
const BOLD_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-Bold.ttf");
const ITALIC_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-Italic.ttf");
const BOLD_ITALIC_FONT: &[u8] = include_bytes!("../assets/JetBrainsMono-BoldItalic.ttf");
const SYMBOL_FONT: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

#[cfg(test)]
// Static non-ASCII chrome emitted by amux-tui. Model and user content is
// intentionally unbounded and still uses the documented missing-glyph box.
const TUI_CHROME_GLYPHS: &str = "·±–—‘’“”←↑→↓↪⟳⌃⌄─│┌┐└┘▌▸●◌◐◑◒◓⚠✓✔✗›…⋮⋯⊘";

/// RGB pixels produced from a terminal buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// One PNG recorded in a directory manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ManifestEntry {
    pub state: String,
    pub theme: String,
    pub color: String,
    pub viewport: [u16; 2],
    pub pixels: [u32; 2],
    pub file: String,
    pub sha256: String,
}

/// One completed render-set invocation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ManifestSet {
    pub name: String,
    pub files: Vec<String>,
}

/// The capture record stored beside rendered files.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
    #[serde(default)]
    pub sets: Vec<ManifestSet>,
}

/// Feed position recorded after one synthetic wheel event.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "PascalCase")]
pub enum RecordedScrollState {
    Following,
    ScrolledBack {
        top_line: usize,
        entry_watermark: u64,
    },
}

impl From<&FeedScroll> for RecordedScrollState {
    fn from(scroll: &FeedScroll) -> Self {
        match scroll {
            FeedScroll::Following => Self::Following,
            FeedScroll::Paused {
                top_line,
                entry_watermark,
            } => Self::ScrolledBack {
                top_line: *top_line,
                entry_watermark: *entry_watermark,
            },
        }
    }
}

/// The mouse input applied between two frames of a scroll recording.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum RecordedMouseEvent {
    WheelUp,
    WheelDown,
}

/// One applied event and the state visible in its resulting frame.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ScrollEventRecord {
    pub frame: usize,
    pub event: RecordedMouseEvent,
    pub scroll: RecordedScrollState,
}

/// One agent's receipt in `events.json`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ScrollRecording {
    pub agent: String,
    pub gif: String,
    pub frames: usize,
    pub initial_scroll: RecordedScrollState,
    pub events: Vec<ScrollEventRecord>,
}

/// Append-safe receipt shared by the Claude and Codex recordings.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ScrollEventLog {
    pub version: u32,
    pub viewport: [u16; 2],
    pub recordings: Vec<ScrollRecording>,
}

/// One scripted input and the frame it produced.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct KeyEventRecord {
    pub frame: usize,
    pub input: String,
}

/// One scripted recording's receipt in `events.json`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct KeyRecording {
    pub name: String,
    pub gif: String,
    pub frames: usize,
    pub events: Vec<KeyEventRecord>,
}

/// The receipt beside a scripted GIF: every input, in order, and the frame
/// it produced, so a reader can tell what was pressed without reading the
/// recorder's source.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct KeyEventLog {
    pub version: u32,
    pub viewport: [u16; 2],
    pub recordings: Vec<KeyRecording>,
}

/// Errors from state lookup, rendering, encoding, or verification.
#[derive(Debug, Error)]
pub enum ShotError {
    #[error("UnknownState({0})")]
    UnknownState(String),
    #[error("UnknownSet({0})")]
    UnknownSet(String),
    #[error("theme error: {0}")]
    Theme(#[from] amux_tui::ThemeError),
    #[error("render error: {0}")]
    Render(String),
    #[error("font error: {0}")]
    Font(String),
    #[error("PNG error: {0}")]
    Encode(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("verification failed: {0}")]
    Verify(String),
}

struct Fonts {
    regular: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
    symbols: Font,
}

impl Fonts {
    fn load() -> Result<Self, ShotError> {
        fn parse(bytes: &'static [u8], face: &str) -> Result<Font, ShotError> {
            Font::from_bytes(bytes, FontSettings::default())
                .map_err(|error| ShotError::Font(format!("{face}: {error}")))
        }

        Ok(Self {
            regular: parse(REGULAR_FONT, "regular")?,
            bold: parse(BOLD_FONT, "bold")?,
            italic: parse(ITALIC_FONT, "italic")?,
            bold_italic: parse(BOLD_ITALIC_FONT, "bold italic")?,
            symbols: parse(SYMBOL_FONT, "symbols fallback")?,
        })
    }

    fn for_cell(&self, cell: &Cell) -> &Font {
        match (
            cell.modifier.contains(Modifier::BOLD),
            cell.modifier.contains(Modifier::ITALIC),
        ) {
            (true, true) => &self.bold_italic,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (false, false) => &self.regular,
        }
    }

    fn for_character(&self, cell: &Cell, character: char) -> &Font {
        let primary = self.for_cell(cell);
        if primary.lookup_glyph_index(character) == 0 {
            &self.symbols
        } else {
            primary
        }
    }
}

/// Render one fixture through ratatui's `TestBackend`.
pub fn render_buffer(state: NamedState, theme: Theme) -> Result<Buffer, ShotError> {
    let fixture = fixture(state);
    render_fixture_buffer(&fixture, theme)
}

fn render_fixture_buffer(fixture: &Fixture, theme: Theme) -> Result<Buffer, ShotError> {
    let backend = TestBackend::new(VIEWPORT.0, VIEWPORT.1);
    let mut terminal =
        Terminal::new(backend).map_err(|error| ShotError::Render(error.to_string()))?;
    let context = FrameContext {
        viewport: VIEWPORT,
        theme,
        now: fixture.now,
    };
    terminal
        .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
        .map_err(|error| ShotError::Render(error.to_string()))?;
    Ok(terminal.backend().buffer().clone())
}

/// Record twelve wheel-up and twelve wheel-down events against a long feed.
/// The first GIF frame is the untouched following state; every later frame
/// is rendered after routing one real `MouseEvent` through the chat handler.
pub fn record_scroll(
    protocol: StructuredProtocol,
    theme: Theme,
    out: &Path,
) -> Result<ScrollRecording, ShotError> {
    const EVENT_COUNT_PER_DIRECTION: usize = 12;
    const FRAME_DELAY_CENTISECONDS: u16 = 12;

    // An SDK-driven chat renders only its unsupported placeholder; a long
    // scrolling feed recorded for it would be an invention, not evidence.
    if matches!(protocol, StructuredProtocol::ClaudeSdk) {
        return Err(ShotError::Render(
            "an SDK-driven chat has no native feed to scroll".to_string(),
        ));
    }

    fs::create_dir_all(out)?;
    let agent = protocol_name(protocol);
    let gif_name = format!("{agent}-wheel.gif");
    let gif_path = out.join(&gif_name);
    let temporary = out.join(format!("{gif_name}.tmp"));
    let mut fixture = long_feed(protocol, 1_000);

    let initial_buffer = render_fixture_buffer(&fixture, theme)?;
    let initial_raster = rasterize(&initial_buffer, theme)?;
    let width = u16::try_from(initial_raster.width)
        .map_err(|_| ShotError::Encode("GIF width exceeds u16".to_string()))?;
    let height = u16::try_from(initial_raster.height)
        .map_err(|_| ShotError::Encode("GIF height exceeds u16".to_string()))?;
    let writer = BufWriter::new(File::create(&temporary)?);
    let mut encoder = GifEncoder::new(writer, width, height, &[])
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    write_gif_frame(&mut encoder, initial_raster, FRAME_DELAY_CENTISECONDS)?;

    let initial_scroll = fixture
        .view
        .chat
        .as_ref()
        .map(|chat| RecordedScrollState::from(chat.scroll()))
        .ok_or_else(|| ShotError::Render(format!("{agent} long-feed fixture has no chat")))?;
    let mut events = Vec::with_capacity(EVENT_COUNT_PER_DIRECTION * 2);
    for (index, kind) in std::iter::repeat_n(RecordedMouseEvent::WheelUp, EVENT_COUNT_PER_DIRECTION)
        .chain(std::iter::repeat_n(
            RecordedMouseEvent::WheelDown,
            EVENT_COUNT_PER_DIRECTION,
        ))
        .enumerate()
    {
        let mouse = MouseEvent {
            kind: match kind {
                RecordedMouseEvent::WheelUp => MouseEventKind::ScrollUp,
                RecordedMouseEvent::WheelDown => MouseEventKind::ScrollDown,
            },
            column: 4,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        let chat =
            fixture.view.chat.as_mut().ok_or_else(|| {
                ShotError::Render(format!("{agent} long-feed fixture has no chat"))
            })?;
        if !handle_chat_mouse(chat, &fixture.model, mouse, VIEWPORT) {
            return Err(ShotError::Render(format!(
                "{agent} wheel event {} did not move the long feed",
                index + 1
            )));
        }
        let scroll = RecordedScrollState::from(chat.scroll());
        let buffer = render_fixture_buffer(&fixture, theme)?;
        let raster = rasterize(&buffer, theme)?;
        write_gif_frame(&mut encoder, raster, FRAME_DELAY_CENTISECONDS)?;
        events.push(ScrollEventRecord {
            frame: index + 1,
            event: kind,
            scroll,
        });
    }

    let mut writer = encoder
        .into_inner()
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    writer.flush()?;
    fs::rename(temporary, gif_path)?;

    let recording = ScrollRecording {
        agent: agent.to_string(),
        gif: gif_name,
        frames: events.len() + 1,
        initial_scroll,
        events,
    };
    if recording.events.last().map(|event| &event.scroll) != Some(&RecordedScrollState::Following) {
        return Err(ShotError::Render(format!(
            "{agent} did not resume Following after the final wheel event"
        )));
    }
    update_scroll_event_log(out, recording.clone())?;
    Ok(recording)
}

/// The keystroke recordings: one frame per scripted input, through the
/// chat's own handlers.
///
/// A GIF of a screen nobody can reach is worth nothing, so nothing here
/// reaches inside the view. The script is what a person presses; the
/// frames are what the program drew in answer.
fn record_script(
    name: &str,
    mut fixture: Fixture,
    script: &[ScriptStep],
    theme: Theme,
    theme_label: &str,
    out: &Path,
) -> Result<KeyRecording, ShotError> {
    const FRAME_DELAY_CENTISECONDS: u16 = 22;

    fs::create_dir_all(out)?;
    let gif_name = format!("{name}.gif");
    let gif_path = out.join(&gif_name);
    let temporary = out.join(format!("{gif_name}.tmp"));

    let initial = rasterize(&render_fixture_buffer(&fixture, theme)?, theme)?;
    let width = u16::try_from(initial.width)
        .map_err(|_| ShotError::Encode("GIF width exceeds u16".to_string()))?;
    let height = u16::try_from(initial.height)
        .map_err(|_| ShotError::Encode("GIF height exceeds u16".to_string()))?;
    let writer = BufWriter::new(File::create(&temporary)?);
    let mut encoder = GifEncoder::new(writer, width, height, &[])
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    write_gif_frame(&mut encoder, initial, FRAME_DELAY_CENTISECONDS)?;

    let mut events = Vec::with_capacity(script.len());
    for (index, step) in script.iter().enumerate() {
        apply_step(&mut fixture, step);
        let raster = rasterize(&render_fixture_buffer(&fixture, theme)?, theme)?;
        write_gif_frame(&mut encoder, raster, FRAME_DELAY_CENTISECONDS)?;
        events.push(KeyEventRecord {
            frame: index + 1,
            input: step.label(),
        });
    }

    let mut writer = encoder
        .into_inner()
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    writer.flush()?;
    fs::rename(temporary, gif_path)?;

    // The last frame also lands as a still, in the manifest beside the
    // PNGs: a GIF is not something `verify` can check, and the screen a
    // recording ends on is worth looking at on its own.
    let still = format!("{name}-final.png");
    let raster = rasterize(&render_fixture_buffer(&fixture, theme)?, theme)?;
    let path = out.join(&still);
    write_png(&raster, &path)?;
    let bytes = fs::read(&path)?;
    append_entry(
        out,
        ManifestEntry {
            state: format!("{name}-recording"),
            theme: theme_label.to_string(),
            color: color_mode_name(theme.mode).to_string(),
            viewport: [VIEWPORT.0, VIEWPORT.1],
            pixels: [raster.width, raster.height],
            file: still.clone(),
            sha256: hex_digest(&bytes),
        },
    )?;
    // Its own set name: a recording lands in the same directory as the
    // rendered set it illustrates, and the two must not overwrite each
    // other's receipt.
    append_set(out, &format!("{name}-recording"), vec![still])?;

    let recording = KeyRecording {
        name: name.to_string(),
        gif: gif_name,
        frames: events.len() + 1,
        events,
    };
    update_key_event_log(out, recording.clone())?;
    Ok(recording)
}

/// The trip through a review: the chord, the frozen diff, a selection over
/// a removed row and the added one under it, a comment saved, `q` back to
/// the draft, and Enter on the token to resume the page.
pub fn record_review(
    out: &Path,
    theme: Theme,
    theme_label: &str,
) -> Result<KeyRecording, ShotError> {
    record_script(
        "review",
        recording_start(),
        &review_script(),
        theme,
        theme_label,
        out,
    )
}

/// The inputs `record-review` presses, in order.
pub fn review_script() -> Vec<ScriptStep> {
    let mut script = vec![
        ScriptStep::Key(control('a')),
        ScriptStep::Key(plain('r')),
        ScriptStep::FrozenDiff(DiffBase::WorkingTree),
    ];
    for character in ['j', 'j', 'v', 'j', 'c'] {
        script.push(ScriptStep::Key(plain(character)));
    }
    script.push(ScriptStep::Type(
        "the old name is public; keep a re-export for one release".to_string(),
    ));
    script.extend([
        ScriptStep::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ScriptStep::Key(plain('q')),
        // After `q` the cursor sits past the token; one step back puts it
        // on the token, where Enter resumes the page instead of sending.
        ScriptStep::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        ScriptStep::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    ]);
    script
}

/// The trip through a draft that carries attachments: two of them added,
/// one deleted, an ask taking the surface over and handing it back, the
/// guarded clear and its undo, and the review chord pressed while the
/// agent is still working.
pub fn record_draft(
    out: &Path,
    theme: Theme,
    theme_label: &str,
) -> Result<KeyRecording, ShotError> {
    record_script(
        "draft",
        recording_start(),
        &draft_script(),
        theme,
        theme_label,
        out,
    )
}

/// The inputs `record-draft` presses, in order.
pub fn draft_script() -> Vec<ScriptStep> {
    vec![
        ScriptStep::Type("compare ".to_string()),
        ScriptStep::Clipboard(ClipboardContent::Image {
            mime: "image/png".to_string(),
            bytes: vec![b'p'; 120_433],
        }),
        ScriptStep::Type(" against the trace ".to_string()),
        ScriptStep::Paste(pasted_lines(240)),
        // One backspace takes the whole paste out; the image stays.
        ScriptStep::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        // An ask takes the surface, then resolves and hands it back with
        // the draft untouched.
        ScriptStep::Conversation(NamedState::ClaudePermissionAsk),
        ScriptStep::Conversation(NamedState::ClaudeWorking),
        // The guarded clear, and the yank that undoes it.
        ScriptStep::Key(control('c')),
        ScriptStep::Key(control('y')),
        // The review chord while the agent is still working: the page
        // opens over what it has written so far.
        ScriptStep::Key(control('a')),
        ScriptStep::Key(plain('r')),
        ScriptStep::FrozenDiff(DiffBase::WorkingTree),
    ]
}

fn plain(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
}

fn control(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn pasted_lines(count: usize) -> String {
    (1..=count).map(|n| format!("stack frame {n}\n")).collect()
}

fn update_key_event_log(out: &Path, recording: KeyRecording) -> Result<(), ShotError> {
    let path = out.join("events.json");
    let mut log = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<KeyEventLog>(&bytes)
            .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => KeyEventLog {
            version: 1,
            viewport: [VIEWPORT.0, VIEWPORT.1],
            recordings: Vec::new(),
        },
        Err(error) => return Err(error.into()),
    };
    if log.version != 1 || log.viewport != [VIEWPORT.0, VIEWPORT.1] {
        return Err(ShotError::Verify(format!(
            "{} is not a compatible 120x40 event log",
            path.display()
        )));
    }
    if let Some(existing) = log
        .recordings
        .iter_mut()
        .find(|existing| existing.name == recording.name)
    {
        *existing = recording;
    } else {
        log.recordings.push(recording);
    }
    log.recordings
        .sort_by(|left, right| left.name.cmp(&right.name));

    let temporary = out.join("events.json.tmp");
    let mut bytes = serde_json::to_vec_pretty(&log)
        .map_err(|error| ShotError::Verify(format!("event log serialization: {error}")))?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn write_gif_frame<W: Write>(
    encoder: &mut GifEncoder<W>,
    raster: Raster,
    delay: u16,
) -> Result<(), ShotError> {
    let width = u16::try_from(raster.width)
        .map_err(|_| ShotError::Encode("GIF width exceeds u16".to_string()))?;
    let height = u16::try_from(raster.height)
        .map_err(|_| ShotError::Encode("GIF height exceeds u16".to_string()))?;
    let mut frame = GifFrame::from_rgb_speed(width, height, &raster.pixels, 10);
    frame.delay = delay;
    encoder
        .write_frame(&frame)
        .map_err(|error| ShotError::Encode(error.to_string()))
}

fn update_scroll_event_log(out: &Path, recording: ScrollRecording) -> Result<(), ShotError> {
    let path = out.join("events.json");
    let mut log = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<ScrollEventLog>(&bytes)
            .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => ScrollEventLog {
            version: 1,
            viewport: [VIEWPORT.0, VIEWPORT.1],
            recordings: Vec::new(),
        },
        Err(error) => return Err(error.into()),
    };
    if log.version != 1 || log.viewport != [VIEWPORT.0, VIEWPORT.1] {
        return Err(ShotError::Verify(format!(
            "{} is not a compatible 120x40 scroll log",
            path.display()
        )));
    }
    if let Some(existing) = log
        .recordings
        .iter_mut()
        .find(|existing| existing.agent == recording.agent)
    {
        *existing = recording;
    } else {
        log.recordings.push(recording);
    }
    log.recordings
        .sort_by(|left, right| left.agent.cmp(&right.agent));

    let temporary = out.join("events.json.tmp");
    let mut bytes = serde_json::to_vec_pretty(&log)
        .map_err(|error| ShotError::Verify(format!("event log serialization: {error}")))?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn protocol_name(protocol: StructuredProtocol) -> &'static str {
    match protocol {
        StructuredProtocol::Claude => "claude",
        StructuredProtocol::ClaudeSdk => "claude-sdk",
        StructuredProtocol::Codex => "codex",
    }
}

/// Rasterize every cell, including its foreground, background, bold, italic,
/// and dim attributes, with the embedded JetBrains Mono faces.
pub fn rasterize(buffer: &Buffer, theme: Theme) -> Result<Raster, ShotError> {
    let fonts = Fonts::load()?;
    let width = u32::from(buffer.area.width) * CELL_WIDTH;
    let height = u32::from(buffer.area.height) * CELL_HEIGHT;
    let mut raster = Raster {
        width,
        height,
        pixels: vec![0; width as usize * height as usize * 3],
    };

    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            paint_cell(&mut raster, &fonts, cell, theme, x, y);
        }
    }
    Ok(raster)
}

fn paint_cell(raster: &mut Raster, fonts: &Fonts, cell: &Cell, theme: Theme, x: u16, y: u16) {
    let mut fg = color_rgb(cell.fg, theme, false);
    let mut bg = color_rgb(cell.bg, theme, true);
    if cell.modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.modifier.contains(Modifier::DIM) {
        fg = blend(bg, fg, 0.55);
    }
    if cell.modifier.contains(Modifier::HIDDEN) {
        fg = bg;
    }

    let left = u32::from(x) * CELL_WIDTH;
    let top = u32::from(y) * CELL_HEIGHT;
    fill_rect(raster, left, top, CELL_WIDTH, CELL_HEIGHT, bg);

    let symbol = cell.symbol();
    if symbol.trim().is_empty() || fg == bg {
        return;
    }

    let chars = symbol.chars().filter(|character| !character.is_control());
    let mut pen_x = left as i32;
    for character in chars {
        let font = fonts.for_character(cell, character);
        let glyph = font.lookup_glyph_index(character);
        if glyph == 0 && character != '\0' {
            draw_missing_glyph(raster, left, top, fg);
            break;
        }
        let (metrics, bitmap) = font.rasterize(character, FONT_SIZE);
        let glyph_left = pen_x + metrics.xmin;
        let glyph_top = top as i32 + BASELINE - metrics.height as i32 - metrics.ymin;
        for glyph_y in 0..metrics.height {
            for glyph_x in 0..metrics.width {
                let alpha = bitmap[glyph_y * metrics.width + glyph_x];
                if alpha == 0 {
                    continue;
                }
                let pixel_x = glyph_left + glyph_x as i32;
                let pixel_y = glyph_top + glyph_y as i32;
                if pixel_x < left as i32
                    || pixel_x >= (left + CELL_WIDTH) as i32
                    || pixel_y < top as i32
                    || pixel_y >= (top + CELL_HEIGHT) as i32
                {
                    continue;
                }
                blend_pixel(raster, pixel_x as u32, pixel_y as u32, fg, alpha);
            }
        }
        pen_x += metrics.advance_width.round() as i32;
        if pen_x >= (left + CELL_WIDTH) as i32 {
            break;
        }
    }
}

fn draw_missing_glyph(raster: &mut Raster, left: u32, top: u32, color: [u8; 3]) {
    for x in left + 2..left + CELL_WIDTH.saturating_sub(2) {
        put_pixel(raster, x, top + 3, color);
        put_pixel(raster, x, top + CELL_HEIGHT - 4, color);
    }
    for y in top + 3..top + CELL_HEIGHT.saturating_sub(3) {
        put_pixel(raster, left + 2, y, color);
        put_pixel(raster, left + CELL_WIDTH - 3, y, color);
    }
}

fn color_rgb(color: Color, theme: Theme, background: bool) -> [u8; 3] {
    match color {
        Color::Reset => {
            let token = if background {
                theme.tokens.background
            } else {
                theme.tokens.text
            };
            resolved_rgb(token.resolve(theme.mode), token.rgb)
        }
        Color::Black => [0, 0, 0],
        Color::Red => [205, 49, 49],
        Color::Green => [13, 188, 121],
        Color::Yellow => [229, 229, 16],
        Color::Blue => [36, 114, 200],
        Color::Magenta => [188, 63, 188],
        Color::Cyan => [17, 168, 205],
        Color::Gray => [204, 204, 204],
        Color::DarkGray => [102, 102, 102],
        Color::LightRed => [241, 76, 76],
        Color::LightGreen => [35, 209, 139],
        Color::LightYellow => [245, 245, 67],
        Color::LightBlue => [59, 142, 234],
        Color::LightMagenta => [214, 112, 214],
        Color::LightCyan => [41, 184, 219],
        Color::White => [242, 242, 242],
        Color::Rgb(red, green, blue) => [red, green, blue],
        Color::Indexed(index) => indexed_rgb(index),
    }
}

fn resolved_rgb(color: Color, fallback: (u8, u8, u8)) -> [u8; 3] {
    match color {
        Color::Rgb(red, green, blue) => [red, green, blue],
        Color::Reset => [fallback.0, fallback.1, fallback.2],
        other => color_rgb_for_named(other).unwrap_or([fallback.0, fallback.1, fallback.2]),
    }
}

fn color_rgb_for_named(color: Color) -> Option<[u8; 3]> {
    Some(match color {
        Color::Black => [0, 0, 0],
        Color::Red => [205, 49, 49],
        Color::Green => [13, 188, 121],
        Color::Yellow => [229, 229, 16],
        Color::Blue => [36, 114, 200],
        Color::Magenta => [188, 63, 188],
        Color::Cyan => [17, 168, 205],
        Color::Gray => [204, 204, 204],
        Color::DarkGray => [102, 102, 102],
        Color::LightRed => [241, 76, 76],
        Color::LightGreen => [35, 209, 139],
        Color::LightYellow => [245, 245, 67],
        Color::LightBlue => [59, 142, 234],
        Color::LightMagenta => [214, 112, 214],
        Color::LightCyan => [41, 184, 219],
        Color::White => [242, 242, 242],
        Color::Indexed(index) => indexed_rgb(index),
        Color::Rgb(red, green, blue) => [red, green, blue],
        Color::Reset => return None,
    })
}

fn indexed_rgb(index: u8) -> [u8; 3] {
    const ANSI: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 49, 49],
        [13, 188, 121],
        [229, 229, 16],
        [36, 114, 200],
        [188, 63, 188],
        [17, 168, 205],
        [204, 204, 204],
        [102, 102, 102],
        [241, 76, 76],
        [35, 209, 139],
        [245, 245, 67],
        [59, 142, 234],
        [214, 112, 214],
        [41, 184, 219],
        [242, 242, 242],
    ];
    if index < 16 {
        return ANSI[index as usize];
    }
    if index < 232 {
        let value = index - 16;
        let channel = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
        return [
            channel(value / 36),
            channel((value % 36) / 6),
            channel(value % 6),
        ];
    }
    let gray = 8 + (index - 232) * 10;
    [gray, gray, gray]
}

fn blend(background: [u8; 3], foreground: [u8; 3], amount: f32) -> [u8; 3] {
    std::array::from_fn(|index| {
        (f32::from(background[index]) * (1.0 - amount) + f32::from(foreground[index]) * amount)
            .round() as u8
    })
}

fn fill_rect(raster: &mut Raster, x: u32, y: u32, width: u32, height: u32, color: [u8; 3]) {
    for pixel_y in y..y + height {
        for pixel_x in x..x + width {
            put_pixel(raster, pixel_x, pixel_y, color);
        }
    }
}

fn blend_pixel(raster: &mut Raster, x: u32, y: u32, foreground: [u8; 3], alpha: u8) {
    let offset = (y as usize * raster.width as usize + x as usize) * 3;
    let amount = f32::from(alpha) / 255.0;
    for (channel, foreground) in foreground.into_iter().enumerate() {
        raster.pixels[offset + channel] = (f32::from(raster.pixels[offset + channel])
            * (1.0 - amount)
            + f32::from(foreground) * amount)
            .round() as u8;
    }
}

fn put_pixel(raster: &mut Raster, x: u32, y: u32, color: [u8; 3]) {
    let offset = (y as usize * raster.width as usize + x as usize) * 3;
    raster.pixels[offset..offset + 3].copy_from_slice(&color);
}

/// Write a raster as a deterministic RGB PNG.
pub fn write_png(raster: &Raster, out: &Path) -> Result<(), ShotError> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(out)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, raster.width, raster.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Best);
    let mut png = encoder
        .write_header()
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    png.write_image_data(&raster.pixels)
        .map_err(|error| ShotError::Encode(error.to_string()))?;
    Ok(())
}

/// Render, encode, and record one capture in the manifest beside `out`.
pub fn render_to_path(
    state: NamedState,
    theme: Theme,
    theme_label: &str,
    out: &Path,
) -> Result<ManifestEntry, ShotError> {
    let buffer = render_buffer(state, theme)?;
    let raster = rasterize(&buffer, theme)?;
    write_png(&raster, out)?;
    let bytes = fs::read(out)?;
    let file = out
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ShotError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PNG output needs a file name",
            ))
        })?
        .to_string();
    let entry = ManifestEntry {
        state: state.name().to_string(),
        theme: theme_label.to_string(),
        color: color_mode_name(theme.mode).to_string(),
        viewport: [VIEWPORT.0, VIEWPORT.1],
        pixels: [raster.width, raster.height],
        file,
        sha256: hex_digest(&bytes),
    };
    append_entry(
        out.parent().unwrap_or_else(|| Path::new(".")),
        entry.clone(),
    )?;
    Ok(entry)
}

/// Record a receipt after a complete render-set invocation.
pub fn append_set(dir: &Path, name: &str, files: Vec<String>) -> Result<(), ShotError> {
    let mut manifest = read_manifest_or_default(&dir.join("manifest.json"))?;
    let set = ManifestSet {
        name: name.to_string(),
        files,
    };
    if let Some(existing) = manifest
        .sets
        .iter_mut()
        .find(|existing| existing.name == name)
    {
        *existing = set;
    } else {
        manifest.sets.push(set);
    }
    write_manifest(dir, &manifest)
}

fn append_entry(dir: &Path, entry: ManifestEntry) -> Result<(), ShotError> {
    let mut manifest = read_manifest_or_default(&dir.join("manifest.json"))?;
    if let Some(existing) = manifest
        .entries
        .iter_mut()
        .find(|existing| existing.file == entry.file)
    {
        *existing = entry;
    } else {
        manifest.entries.push(entry);
    }
    write_manifest(dir, &manifest)
}

fn read_manifest_or_default(path: &Path) -> Result<Manifest, ShotError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(error) => Err(error.into()),
    }
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), ShotError> {
    fs::create_dir_all(dir)?;
    let path = dir.join("manifest.json");
    let temporary = dir.join("manifest.json.tmp");
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| ShotError::Verify(format!("manifest serialization: {error}")))?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Verify every manifest and PNG below `dir`.
pub fn verify(dir: &Path) -> Result<Manifest, ShotError> {
    let mut manifests = Vec::new();
    find_manifests(dir, &mut manifests)?;
    if manifests.is_empty() {
        return Err(ShotError::Verify(format!(
            "no manifest.json found below {}",
            dir.display()
        )));
    }

    let mut combined = Manifest::default();
    for path in manifests {
        let bytes = fs::read(&path)?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
        let base = path.parent().expect("manifest has a parent");
        for entry in &manifest.entries {
            verify_entry(base, entry)?;
        }
        for set in &manifest.sets {
            for file in &set.files {
                if !manifest.entries.iter().any(|entry| &entry.file == file) {
                    return Err(ShotError::Verify(format!(
                        "set `{}` is missing manifest entry `{file}`",
                        set.name
                    )));
                }
                if !base.join(file).is_file() {
                    return Err(ShotError::Verify(format!(
                        "set `{}` is missing file `{file}`",
                        set.name
                    )));
                }
            }
        }
        combined.entries.extend(manifest.entries);
        combined.sets.extend(manifest.sets);
    }
    Ok(combined)
}

fn find_manifests(path: &Path, manifests: &mut Vec<PathBuf>) -> Result<(), ShotError> {
    if path.is_file() {
        if path.file_name().is_some_and(|name| name == "manifest.json") {
            manifests.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            find_manifests(&entry.path(), manifests)?;
        } else if entry.file_name() == "manifest.json" {
            manifests.push(entry.path());
        }
    }
    manifests.sort();
    Ok(())
}

fn verify_entry(base: &Path, entry: &ManifestEntry) -> Result<(), ShotError> {
    if entry.viewport != [VIEWPORT.0, VIEWPORT.1] {
        return Err(ShotError::Verify(format!(
            "{} records viewport {}x{} instead of {}x{}",
            entry.file, entry.viewport[0], entry.viewport[1], VIEWPORT.0, VIEWPORT.1
        )));
    }
    let expected_pixels = [
        VIEWPORT.0 as u32 * CELL_WIDTH,
        VIEWPORT.1 as u32 * CELL_HEIGHT,
    ];
    if entry.pixels != expected_pixels {
        return Err(ShotError::Verify(format!(
            "{} records pixel size {}x{} instead of {}x{}",
            entry.file, entry.pixels[0], entry.pixels[1], expected_pixels[0], expected_pixels[1]
        )));
    }
    let path = base.join(&entry.file);
    let bytes = fs::read(&path)
        .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
    let digest = hex_digest(&bytes);
    if digest != entry.sha256 {
        return Err(ShotError::Verify(format!(
            "{} has sha256 {digest}, expected {}",
            path.display(),
            entry.sha256
        )));
    }

    let decoder = png::Decoder::new(BufReader::new(File::open(&path)?));
    let mut reader = decoder
        .read_info()
        .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|error| ShotError::Verify(format!("{}: {error}", path.display())))?;
    if [info.width, info.height] != expected_pixels {
        return Err(ShotError::Verify(format!(
            "{} is {}x{}, expected {}x{}",
            path.display(),
            info.width,
            info.height,
            expected_pixels[0],
            expected_pixels[1]
        )));
    }
    Ok(())
}

fn color_mode_name(mode: ColorMode) -> &'static str {
    match mode {
        ColorMode::TrueColor => "truecolor",
        ColorMode::Ansi => "ansi",
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(result, "{byte:02x}").expect("writing to a String cannot fail");
    }
    result
}

#[cfg(test)]
mod tests {
    use std::fs;

    use amux_tui::fixtures::NamedState;
    use amux_tui::{ColorMode, Theme};
    use amux_ui::StructuredProtocol;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};
    use tempfile::tempdir;

    use super::{
        CELL_HEIGHT, CELL_WIDTH, Fonts, RecordedScrollState, TUI_CHROME_GLYPHS, VIEWPORT,
        append_set, rasterize, record_scroll, render_to_path, verify,
    };

    #[test]
    fn every_tui_chrome_glyph_has_a_vendored_face_in_every_style() {
        let fonts = Fonts::load().unwrap();
        let modifiers = [
            Modifier::empty(),
            Modifier::BOLD,
            Modifier::ITALIC,
            Modifier::BOLD | Modifier::ITALIC,
        ];

        for character in TUI_CHROME_GLYPHS.chars() {
            assert!(!character.is_ascii(), "glyph inventory is non-ASCII only");
            for modifier in modifiers {
                let mut cell = ratatui::buffer::Cell::default();
                cell.modifier = modifier;
                let font = fonts.for_character(&cell, character);
                assert_ne!(
                    font.lookup_glyph_index(character),
                    0,
                    "U+{:04X} `{character}` is missing for {modifier:?}",
                    character as u32
                );
            }
        }
    }

    #[test]
    fn png_has_exact_cell_dimensions() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("idle.png");
        render_to_path(
            NamedState::ClaudeIdle,
            Theme::dark(ColorMode::TrueColor),
            "dark",
            &output,
        )
        .unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(output).unwrap()));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, VIEWPORT.0 as u32 * CELL_WIDTH);
        assert_eq!(reader.info().height, VIEWPORT.1 as u32 * CELL_HEIGHT);
    }

    #[test]
    fn two_renders_are_byte_identical() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.png");
        let second = directory.path().join("second.png");
        let theme = Theme::dark(ColorMode::TrueColor);
        render_to_path(NamedState::ClaudeIdle, theme, "dark", &first).unwrap();
        render_to_path(NamedState::ClaudeIdle, theme, "dark", &second).unwrap();
        assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());

        let manifest: super::Manifest =
            serde_json::from_slice(&fs::read(directory.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.entries.len(), 2, "each render appends a row");
    }

    #[test]
    fn scroll_gif_has_one_initial_frame_plus_every_event_and_resumes_following() {
        let directory = tempdir().unwrap();
        let recording = record_scroll(
            StructuredProtocol::Claude,
            Theme::dark(ColorMode::TrueColor),
            directory.path(),
        )
        .unwrap();

        assert_eq!(recording.frames, 25);
        assert_eq!(recording.events.len(), 24);
        assert_eq!(
            recording.events.last().map(|event| &event.scroll),
            Some(&RecordedScrollState::Following)
        );

        let file = fs::File::open(directory.path().join(&recording.gif)).unwrap();
        let mut decoder = gif::DecodeOptions::new();
        decoder.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = decoder.read_info(std::io::BufReader::new(file)).unwrap();
        assert_eq!(decoder.width(), VIEWPORT.0 * CELL_WIDTH as u16);
        assert_eq!(decoder.height(), VIEWPORT.1 * CELL_HEIGHT as u16);
        let mut frames = 0;
        while decoder.read_next_frame().unwrap().is_some() {
            frames += 1;
        }
        assert_eq!(frames, 25);

        let event_log: super::ScrollEventLog =
            serde_json::from_slice(&fs::read(directory.path().join("events.json")).unwrap())
                .unwrap();
        assert_eq!(event_log.recordings, [recording]);
    }

    #[test]
    fn rerender_replaces_the_file_entry_and_named_set_receipt() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("x.png");
        render_to_path(
            NamedState::ClaudeIdle,
            Theme::dark(ColorMode::TrueColor),
            "dark",
            &output,
        )
        .unwrap();
        append_set(directory.path(), "chat", vec!["stale.png".to_string()]).unwrap();

        render_to_path(
            NamedState::ClaudeIdle,
            Theme::light(ColorMode::TrueColor),
            "light",
            &output,
        )
        .unwrap();
        append_set(directory.path(), "chat", vec!["x.png".to_string()]).unwrap();

        let manifest = verify(directory.path()).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.entries[0].theme, "light");
        assert_eq!(manifest.sets.len(), 1);
        assert_eq!(manifest.sets[0].files, ["x.png"]);
    }

    #[test]
    fn rasterizer_honors_cell_colors_and_modifiers() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 5, 1));
        for x in 0..4 {
            let cell = &mut buffer[(x, 0)];
            cell.set_symbol("A");
            cell.fg = Color::White;
            cell.bg = Color::Black;
        }
        buffer[(1, 0)].modifier = Modifier::BOLD;
        buffer[(2, 0)].modifier = Modifier::ITALIC;
        buffer[(3, 0)].modifier = Modifier::DIM;
        buffer[(4, 0)].bg = Color::Red;

        let raster = rasterize(&buffer, Theme::dark(ColorMode::TrueColor)).unwrap();
        let region = |cell: u32| {
            let mut bytes = Vec::new();
            for y in 0..CELL_HEIGHT {
                let start = (y * raster.width + cell * CELL_WIDTH) as usize * 3;
                bytes.extend_from_slice(&raster.pixels[start..start + CELL_WIDTH as usize * 3]);
            }
            bytes
        };

        assert_ne!(region(0), region(1), "bold uses the bold face");
        assert_ne!(region(0), region(2), "italic uses the italic face");
        assert_ne!(
            region(0),
            region(3),
            "dim blends foreground toward background"
        );
        let brightness = |bytes: Vec<u8>| bytes.into_iter().map(u64::from).sum::<u64>();
        assert!(brightness(region(0)) > brightness(region(3)));
        assert_eq!(&region(4)[..3], &[205, 49, 49]);
    }

    #[test]
    fn verify_rejects_a_truncated_png() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("idle.png");
        render_to_path(
            NamedState::ClaudeIdle,
            Theme::dark(ColorMode::TrueColor),
            "dark",
            &output,
        )
        .unwrap();
        let mut bytes = fs::read(&output).unwrap();
        bytes.truncate(bytes.len() / 2);
        fs::write(&output, bytes).unwrap();
        let error = verify(directory.path()).unwrap_err();
        assert!(error.to_string().contains("verification failed"));
    }
}

#[cfg(test)]
mod script_tests {
    use std::process::Command as ProcessCommand;

    use amux_tui::UiAction;
    use amux_tui::chat::handle_chat_key;
    use amux_tui::fixtures::{apply_step, deliver_diff_response};
    use amux_ui::{ArtifactKind, ArtifactRef, BaseIdentity, Command, DiffFile, DiffResponse};
    use tempfile::tempdir;

    use super::*;

    /// The text of the frame a script has reached.
    fn screen(fixture: &Fixture) -> String {
        let buffer =
            render_fixture_buffer(fixture, Theme::dark(ColorMode::TrueColor)).expect("frame");
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).expect("cell in area").symbol());
            }
            text.push('\n');
        }
        text
    }

    fn git(project: &Path, arguments: &[&str]) -> String {
        let output = ProcessCommand::new("git")
            .args(arguments)
            .current_dir(project)
            .output()
            .expect("run git for frozen-review boundary");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output is UTF-8")
            .trim()
            .to_string()
    }

    fn repository_diff(project: &Path) -> DiffResponse {
        let patch = git(project, &["diff", "--binary", "HEAD", "--"]);
        let head = git(project, &["rev-parse", "HEAD"]);
        let blob = git(project, &["hash-object", "sample.txt"]);
        let id = format!("sha256:{}", hex_digest(patch.as_bytes()))
            .parse()
            .expect("sha256 diff id");
        DiffResponse {
            artifact: ArtifactRef {
                id,
                kind: ArtifactKind::Diff,
                name: "working-tree.diff".into(),
                mime: "text/x-diff".into(),
                size: patch.len() as u64,
            },
            patch,
            identity: BaseIdentity {
                base: DiffBase::WorkingTree,
                head,
                merge_base: None,
                blobs: vec![("sample.txt".into(), blob)],
            },
            files: vec![DiffFile {
                path: "sample.txt".into(),
                added: 1,
                removed: 1,
            }],
        }
    }

    fn press(fixture: &mut Fixture, key: KeyEvent) -> Option<UiAction> {
        let chat = fixture.view.chat.as_mut().expect("Claude chat open");
        handle_chat_key(chat, &fixture.model, key, VIEWPORT, fixture.now)
    }

    /// A recording is only worth keeping if each input did what the script
    /// says it did, so the draft script is walked one step at a time and
    /// the screen checked where it matters.
    #[test]
    fn the_draft_script_attaches_deletes_survives_a_takeover_and_opens_the_review() {
        let script = draft_script();
        let mut fixture = recording_start();
        let mut screens = Vec::new();
        for step in &script {
            apply_step(&mut fixture, step);
            screens.push(screen(&fixture));
        }

        assert!(
            screens[3].contains("[Image #1]") && screens[3].contains("[Pasted #1"),
            "both attachments ride one draft: {}",
            screens[3]
        );
        assert!(
            screens[4].contains("[Image #1]") && !screens[4].contains("[Pasted #1"),
            "one backspace took only the paste: {}",
            screens[4]
        );
        assert!(
            !screens[5].contains("[Image #1]"),
            "the ask owns the surface and the draft is hidden behind it: {}",
            screens[5]
        );
        assert!(
            screens[6].contains("[Image #1]"),
            "the draft came back whole when the ask resolved: {}",
            screens[6]
        );
        assert!(
            !screens[7].contains("[Image #1]"),
            "the guarded clear emptied the draft: {}",
            screens[7]
        );
        assert!(
            screens[8].contains("[Image #1]"),
            "the yank put the draft back, token and all: {}",
            screens[8]
        );
        assert!(
            screens[11].contains("j/k rows") && screens[11].contains("v select"),
            "the review page is open over a working agent: {}",
            screens[11]
        );
    }

    /// This is the repository boundary behind the last scene in the draft
    /// recording. The patch is obtained from a real git checkout, the file is
    /// changed again after the page opens, and only then do we exercise resume,
    /// the working-phase send gate, and the eventual exported mention.
    #[test]
    fn a_working_review_keeps_the_post_open_repository_change_out_of_its_draft() {
        let directory = tempdir().expect("temporary repository");
        let project = directory.path();
        git(project, &["init", "-q", "--initial-branch=main"]);
        git(project, &["config", "user.name", "amux frozen review"]);
        git(
            project,
            &["config", "user.email", "frozen-review@example.invalid"],
        );
        fs::write(project.join("sample.txt"), "base value\n").expect("write base file");
        git(project, &["add", "sample.txt"]);
        git(project, &["commit", "-q", "-m", "seed fixture"]);
        fs::write(project.join("sample.txt"), "first frozen value\n")
            .expect("write patch opened by the review");
        let opened = repository_diff(project);

        let mut fixture = recording_start();
        apply_step(
            &mut fixture,
            &ScriptStep::Conversation(NamedState::ClaudeWorking),
        );
        assert_eq!(press(&mut fixture, control('a')), None);
        let Some(UiAction::Dispatch(request @ Command::RequestDiff { .. })) =
            press(&mut fixture, plain('r'))
        else {
            panic!("a working agent still permits the review request");
        };
        deliver_diff_response(&mut fixture, request, opened.clone());

        fs::write(project.join("sample.txt"), "second newer value\n")
            .expect("change the repository after review open");
        let changed = repository_diff(project);
        assert_ne!(opened.patch, changed.patch, "the repository patch changed");
        assert_ne!(
            opened.artifact.id, changed.artifact.id,
            "the changed patch has a different content identity"
        );
        assert_ne!(
            opened.identity.blobs, changed.identity.blobs,
            "the changed file has a different new-side blob identity"
        );

        let frozen_screen = screen(&fixture);
        assert!(frozen_screen.contains("first frozen value"));
        assert!(!frozen_screen.contains("second newer value"));

        for key in ['j', 'j', 'c'] {
            assert_eq!(press(&mut fixture, plain(key)), None);
        }
        for character in "keep the opened version".chars() {
            assert_eq!(press(&mut fixture, plain(character)), None);
        }
        assert_eq!(
            press(
                &mut fixture,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            ),
            None
        );
        assert_eq!(press(&mut fixture, plain('q')), None);

        assert_eq!(press(&mut fixture, control('a')), None);
        assert_eq!(
            press(&mut fixture, plain('r')),
            None,
            "resuming the draft must not request a newer diff"
        );
        let resumed_screen = screen(&fixture);
        assert!(resumed_screen.contains("first frozen value"));
        assert!(!resumed_screen.contains("second newer value"));
        assert_eq!(press(&mut fixture, plain('q')), None);

        assert_eq!(
            press(
                &mut fixture,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            ),
            None,
            "send remains gated while the agent works"
        );
        let gated_screen = screen(&fixture);
        assert!(gated_screen.contains("draft kept — send gated while working"));
        assert!(gated_screen.contains("[Review · 1 comment]"));

        apply_step(
            &mut fixture,
            &ScriptStep::Conversation(NamedState::ClaudeIdle),
        );
        let Some(UiAction::Dispatch(Command::SendPromptWithAttachments {
            text, attachments, ..
        })) = press(
            &mut fixture,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        else {
            panic!("the retained draft sends when the turn becomes idle");
        };
        let opened_blob = &opened.identity.blobs[0].1;
        let changed_blob = &changed.identity.blobs[0].1;
        assert!(text.contains(opened.artifact.id.as_str()));
        assert!(text.contains(&opened.identity.head));
        assert!(text.contains(opened_blob));
        assert!(text.contains("keep the opened version"));
        assert!(!text.contains(changed.artifact.id.as_str()));
        assert!(!text.contains(changed_blob));
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id, opened.artifact.id);
        assert!(attachments[0].bytes.is_none());

        println!(
            "frozen-review-boundary: opened_diff={} changed_diff={} opened_blob={} changed_blob={} refetch=false send_while_working=gated draft=retained export=opened_identity",
            opened.artifact.id, changed.artifact.id, opened_blob, changed_blob
        );
    }

    /// The review script has to end where the chord started: back on the
    /// page, resumed from the token rather than sent.
    #[test]
    fn the_review_script_comments_leaves_and_resumes_from_the_token() {
        let script = review_script();
        let mut fixture = recording_start();
        let mut screens = Vec::new();
        for step in &script {
            apply_step(&mut fixture, step);
            screens.push(screen(&fixture));
        }
        let last = screens.len() - 1;
        assert!(
            screens[last - 2].contains("[Review \u{b7} 1 comment]"),
            "`q` left the review in the draft as one token: {}",
            screens[last - 2]
        );
        assert!(
            screens[last].contains("keep a re-export for one release")
                && screens[last].contains("j/k rows"),
            "enter on the token reopened the page with the comment on it: {}",
            screens[last]
        );
    }
}

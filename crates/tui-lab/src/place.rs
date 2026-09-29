//! Where the person was, kept across a relaunch, and what a feedback capture
//! writes. Both live under the repository's gitignored `notes/tui-lab/`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use ratatui::buffer::Buffer;
use serde::{Deserialize, Serialize};
use tui::Theme;
use tui::app::App;
use tui::chat::layout::Anchor;

/// The lab's working directory: `notes/tui-lab/` in this checkout.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../notes/tui-lab")
        .components()
        .collect()
}

pub fn build_log() -> PathBuf {
    dir().join("build.log")
}

/// What survives a relaunch: the scenario and how far its timeline got,
/// and the screen's place. Acts the person made against the fake runtime
/// (sent prompts, answered asks) do not survive; the world is rebuilt from
/// the scenario.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Place {
    pub scenario: String,
    pub fired: usize,
    /// The open chat's agent id.
    #[serde(default)]
    pub open: Option<String>,
    /// The fleet's selected agent id.
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub draft: String,
    /// The chat's scroll anchor: the row at the top and its hidden lines.
    #[serde(default)]
    pub anchor: Option<(String, usize)>,
    #[serde(default)]
    pub variant: u8,
}

impl Place {
    pub fn load() -> Option<Place> {
        let text = std::fs::read_to_string(dir().join("place.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(dir())?;
        std::fs::write(dir().join("place.json"), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// Reads the place off the running app.
    pub fn of(app: &App, scenario: &str, fired: usize) -> Place {
        let id = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        let chat = app.chat.as_ref();
        Place {
            scenario: scenario.to_owned(),
            fired,
            open: chat.map(|chat| id(&chat.view.agent_id)),
            selected: app.fleet_view.selected().map(|key| id(&key.agent)),
            draft: chat
                .map(|chat| {
                    chat.view
                        .editor
                        .text()
                        .replace(attachments_placeholder(), "")
                })
                .unwrap_or_default(),
            anchor: chat.and_then(|chat| match &chat.view.anchor {
                Anchor::Bottom => None,
                Anchor::Top { key, offset } => Some((key.clone(), *offset)),
            }),
            variant: tui::variant::get(),
        }
    }
}

fn attachments_placeholder() -> char {
    '\u{FFFC}'
}

/// The screen as plain text, one line per row, trailing blanks trimmed.
pub fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        for x in area.left()..area.right() {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Writes one frame as `<stem>.txt` and `<stem>.png`.
pub fn write_frame(buffer: &Buffer, theme: Theme, dir: &Path, stem: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{stem}.txt")), buffer_text(buffer))?;
    let raster = shot::rasterize(buffer, theme).context("rasterizing the frame")?;
    shot::write_png(&raster, &dir.join(format!("{stem}.png"))).context("writing the PNG")?;
    Ok(())
}

#[derive(Serialize)]
struct FeedbackMeta<'a> {
    at: String,
    note: &'a str,
    place: &'a Place,
    size: [u16; 2],
}

/// Saves a feedback capture: the frame, the note, and where it was taken.
pub fn save_feedback(buffer: &Buffer, theme: Theme, place: &Place, note: &str) -> Result<PathBuf> {
    let now = chrono::Local::now();
    let dir = dir()
        .join("feedback")
        .join(now.format("%Y%m%d-%H%M%S").to_string());
    write_frame(buffer, theme, &dir, "screen")?;
    let meta = FeedbackMeta {
        at: now.to_rfc3339(),
        note,
        place,
        size: [buffer.area.width, buffer.area.height],
    };
    std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
    std::fs::write(dir.join("note.txt"), format!("{note}\n"))?;
    Ok(dir)
}

//! The system clipboard, read for something to attach.
//!
//! Ctrl+V is the one path an image reaches a draft by: a terminal cannot
//! deliver image bytes through a bracketed paste, so the TUI asks the
//! platform clipboard directly. Everything here is I/O against the host,
//! kept behind [`ClipboardContent`] so the key handlers — and their tests
//! — deal only in the value.

use std::path::PathBuf;

/// What the clipboard held when Ctrl+V was pressed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardContent {
    Image { mime: String, bytes: Vec<u8> },
    Path(PathBuf),
    Text(String),
    Empty,
}

/// Reads the clipboard, preferring an image over the text form.
///
/// A clipboard holding one line that names a readable file is a `Path`, so
/// copying a file in a file manager and pressing Ctrl+V attaches the file
/// rather than typing its name. Anything else is text, and an unavailable
/// clipboard is `Empty` — no clipboard is a normal state on a headless
/// host, never an error worth stating.
pub fn read_clipboard() -> ClipboardContent {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return ClipboardContent::Empty;
    };
    if let Ok(image) = clipboard.get_image()
        && let Some(bytes) = encode_png(&image)
    {
        return ClipboardContent::Image {
            mime: "image/png".to_string(),
            bytes,
        };
    }
    match clipboard.get_text() {
        Ok(text) => classify(text),
        Err(_) => ClipboardContent::Empty,
    }
}

/// Splits clipboard text into a file path or plain text.
pub(crate) fn classify(text: String) -> ClipboardContent {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ClipboardContent::Empty;
    }
    if !trimmed.contains('\n') {
        let path = PathBuf::from(trimmed);
        // Only an absolute path: a relative one would resolve against the
        // TUI's working directory, which is not where the person copied it.
        if path.is_absolute() && path.is_file() {
            return ClipboardContent::Path(path);
        }
    }
    ClipboardContent::Text(text)
}

/// Encodes the clipboard's RGBA image as a PNG.
fn encode_png(image: &arboard::ImageData<'_>) -> Option<Vec<u8>> {
    let width = u32::try_from(image.width).ok()?;
    let height = u32::try_from(image.height).ok()?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&image.bytes).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_text_naming_a_readable_file_is_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.md");
        std::fs::write(&file, b"notes").unwrap();
        assert_eq!(
            classify(format!("  {}\n", file.display())),
            ClipboardContent::Path(file)
        );
    }

    #[test]
    fn a_path_that_names_nothing_stays_text() {
        let text = "/no/such/file.md".to_string();
        assert_eq!(classify(text.clone()), ClipboardContent::Text(text));
    }

    #[test]
    fn empty_clipboard_text_is_empty() {
        assert_eq!(classify("  \n ".to_string()), ClipboardContent::Empty);
    }
}

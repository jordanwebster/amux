//! Clipboard content becoming a composer token.
//!
//! Both native chats share this: Ctrl+V means the same thing everywhere,
//! and the decision of what the clipboard *is* belongs neither to Claude
//! nor to Codex. Reading the clipboard is the caller's job, so the key
//! handlers can be tested with a stub against no real host clipboard.

use std::path::Path;

use amux_ui::attachments::{ARTIFACT_SIZE_CAP, ArtifactKind, DraftAttachment};

use crate::clipboard::ClipboardContent;
use crate::composer::Composer;

/// The name an image pasted straight from the clipboard carries. It has no
/// source file, and the feed line needs something to say.
const CLIPBOARD_IMAGE: &str = "clipboard.png";

/// File extensions the composer attaches as images rather than files, so
/// the backend delivers them natively to a model that can see.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// Attaches what the clipboard held, or returns the refusal to state.
///
/// Text falls through to the ordinary paste rules, so Ctrl+V on a long
/// copy makes the same token a long bracketed paste would.
pub(crate) fn attach_clipboard(
    composer: &mut Composer,
    content: ClipboardContent,
) -> Option<String> {
    match content {
        ClipboardContent::Image { mime, bytes } => {
            attach_bytes(composer, ArtifactKind::Image, CLIPBOARD_IMAGE, &mime, bytes)
        }
        ClipboardContent::Path(path) => attach_path(composer, &path),
        ClipboardContent::Text(text) => {
            composer.paste_or_attach(&text);
            None
        }
        ClipboardContent::Empty => None,
    }
}

fn attach_path(composer: &mut Composer, path: &Path) -> Option<String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let (kind, mime) = if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        (ArtifactKind::Image, format!("image/{extension}"))
    } else {
        (ArtifactKind::File, "application/octet-stream".to_string())
    };
    match std::fs::read(path) {
        Ok(bytes) => attach_bytes(composer, kind, &name, &mime, bytes),
        Err(error) => Some(format!("{name} could not be read: {error}")),
    }
}

/// Refuses over the cap here rather than at the daemon: a person who
/// pasted a 200 MiB file should learn it before pressing Enter.
fn attach_bytes(
    composer: &mut Composer,
    kind: ArtifactKind,
    name: &str,
    mime: &str,
    bytes: Vec<u8>,
) -> Option<String> {
    let size = bytes.len() as u64;
    if size > ARTIFACT_SIZE_CAP {
        return Some(format!(
            "{name} is {} and the limit is {}",
            human_size(size),
            human_size(ARTIFACT_SIZE_CAP),
        ));
    }
    composer.attach(DraftAttachment::from_bytes(kind, name, mime, bytes));
    None
}

pub(crate) fn human_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_over_the_cap_is_refused_before_the_draft_grows() {
        let mut composer = Composer::default();
        let stated = attach_clipboard(
            &mut composer,
            ClipboardContent::Image {
                mime: "image/png".into(),
                bytes: vec![0; 12 * 1024 * 1024],
            },
        );
        assert_eq!(
            stated.as_deref(),
            Some("clipboard.png is 12.0 MiB and the limit is 10.0 MiB")
        );
        assert!(composer.tokens().is_empty());
    }

    #[test]
    fn an_image_file_path_attaches_as_an_image_not_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Screenshot.PNG");
        std::fs::write(&file, b"png bytes").unwrap();
        let mut composer = Composer::default();
        assert_eq!(
            attach_clipboard(&mut composer, ClipboardContent::Path(file)),
            None
        );
        assert_eq!(composer.tokens()[0].label, "[Image #1]");
    }

    #[test]
    fn an_empty_clipboard_changes_nothing() {
        let mut composer = Composer::default();
        assert_eq!(
            attach_clipboard(&mut composer, ClipboardContent::Empty),
            None
        );
        assert!(composer.is_empty());
    }
}

//! The working tree's changed files as a list: grouped under their
//! directory, sorted by path, each file's lines added and removed at the
//! right in the theme's green and red. The Overview's Changes section and
//! the review page's file list both draw it; each places it and sets its
//! width.

use ratatui::text::Line;

use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// A changed file as the list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLine {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// The list drawn, `width` columns wide from its own left edge.
#[derive(Default)]
pub struct ChangesOut {
    pub lines: Vec<Line<'static>>,
    /// Each file's line and its path, in the list's order.
    pub files: Vec<(usize, String)>,
    /// The directory lines: a group scrolled under the top edge keeps its
    /// directory pinned.
    pub dir_lines: Vec<usize>,
}

impl ChangesOut {
    /// The directory line to pin over line `row`, the first under the top
    /// edge: the one naming the group `row` sits in, when it has scrolled
    /// out above. The next group's own line takes over as it reaches `row`.
    pub fn pin(&self, row: usize) -> Option<usize> {
        let dir = self.dir_lines.iter().copied().rfind(|line| *line <= row)?;
        (dir < row && row < self.lines.len()).then_some(dir)
    }
}

/// Files under their directory: a faint line naming it, shortened in the
/// middle so both ends survive, then each file's name two columns in with
/// its lines added and removed at the right; a blank line between groups.
pub fn changes_lines(files: &[FileLine], width: usize, theme: Theme) -> ChangesOut {
    let mut out = ChangesOut::default();
    for (n, (dir, files)) in grouped(files).into_iter().enumerate() {
        if n > 0 {
            out.lines.push(Line::default());
        }
        let indent = if dir.is_empty() {
            0
        } else {
            let mut line = Line::default();
            push(&mut line, shorten_middle(dir, width), theme.faint(), width);
            out.dir_lines.push(out.lines.len());
            out.lines.push(line);
            2
        };
        for file in files {
            // A zero side is left out: "+1", or "−5" for a pure deletion.
            let added = if file.added > 0 {
                format!("+{}", file.added)
            } else {
                String::new()
            };
            let removed = match (file.removed, added.is_empty()) {
                (0, _) => String::new(),
                (n, true) => format!("\u{2212}{n}"),
                (n, false) => format!(" \u{2212}{n}"),
            };
            let counts = format!("{added}{removed}");
            let name = &file.path[dir.len()..];
            let room = width.saturating_sub(indent + text::str_width(&counts) + 2);
            let mut line = Line::default();
            pad_to(&mut line, indent);
            push(&mut line, shorten_left(name, room), theme.text(), width);
            pad_to(&mut line, width.saturating_sub(text::str_width(&counts)));
            // Green and red from the terminal's palette.
            push(&mut line, added, theme.ok(), width);
            push(&mut line, removed, theme.error(), width);
            out.files.push((out.lines.len(), file.path.clone()));
            out.lines.push(line);
        }
    }
    out
}

/// The paths in the list's order: by directory, root files first.
pub fn ordered(files: &[FileLine]) -> Vec<String> {
    grouped(files)
        .into_iter()
        .flat_map(|(_, files)| files)
        .map(|file| file.path.clone())
        .collect()
}

/// The changed files by directory, sorted by path: files at the root first,
/// under no directory, then each directory with its files. A directory is
/// named with its trailing slash.
fn grouped(files: &[FileLine]) -> Vec<(&str, Vec<&FileLine>)> {
    let mut sorted: Vec<&FileLine> = files.iter().collect();
    sorted.sort_by(|a, b| {
        let dir = |f: &FileLine| f.path.rfind('/').map_or(0, |at| at + 1);
        f_key(a, dir(a)).cmp(&f_key(b, dir(b)))
    });
    let mut groups: Vec<(&str, Vec<&FileLine>)> = Vec::new();
    for file in sorted {
        let dir = &file.path[..file.path.rfind('/').map_or(0, |at| at + 1)];
        match groups.last_mut() {
            Some((last, members)) if *last == dir => members.push(file),
            _ => groups.push((dir, vec![file])),
        }
    }
    groups
}

/// Root files first, then by directory, then by name.
fn f_key(file: &FileLine, dir: usize) -> (bool, &str, &str) {
    (dir > 0, &file.path[..dir], &file.path[dir..])
}

/// A directory in at most `max` columns, cut in the middle at directories
/// so its first and last parts survive: "apps/…/Sources/Chat/".
fn shorten_middle(dir: &str, max: usize) -> String {
    if text::str_width(dir) <= max {
        return dir.to_owned();
    }
    let parts: Vec<&str> = dir.trim_end_matches('/').split('/').collect();
    if let Some((first, rest)) = parts.split_first() {
        for keep in (1..rest.len()).rev() {
            let tail = rest[rest.len() - keep..].join("/");
            let shown = format!("{first}/…/{tail}/");
            if text::str_width(&shown) <= max {
                return shown;
            }
        }
    }
    shorten_left(dir, max)
}

/// `path` in at most `max` columns, cut from the left at a directory so the
/// file's name survives: "…/src/session.rs"; a name too long on its own
/// keeps its start and extension.
pub fn shorten_left(path: &str, max: usize) -> String {
    if text::str_width(path) <= max {
        return path.to_owned();
    }
    let mut at = 0;
    while let Some(slash) = path[at..].find('/') {
        at += slash + 1;
        let tail = &path[at..];
        if text::str_width(tail) + 2 <= max {
            return format!("…/{tail}");
        }
    }
    // Only the name is left: keep its start and its extension, which tell
    // names apart better than their ends ("Composer….swift").
    let name = &path[at..];
    let ext = name
        .rfind('.')
        .filter(|dot| *dot > 0)
        .map_or("", |dot| &name[dot..]);
    let head = max.saturating_sub(text::str_width(ext) + 1);
    if head >= 3 {
        let start: String = name.chars().take(head).collect();
        return format!("{start}…{ext}");
    }
    let keep = max.saturating_sub(1);
    let skip = name.chars().count().saturating_sub(keep);
    format!("…{}", name.chars().skip(skip).collect::<String>())
}

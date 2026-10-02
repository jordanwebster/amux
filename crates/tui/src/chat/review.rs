//! The review page: a full-screen reading of the agent's working-tree diff,
//! frozen when the page opened, with the person's comments on its lines.
//!
//! The document is ui-view's [`review_doc`]; this module owns the cursor,
//! the scroll and the comment editor. Comments become one Review
//! attachment in the chat's draft, which the chat keeps in step with the
//! page as comments are saved and deleted.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_view::{FileStatus, LineKind, ReviewDoc, review_doc};
use wire::{Attachment, Diff, Review, ReviewComment, attachment};

use super::composer::editor_lines;
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// A place a comment can go: a file as a whole, or one line of a hunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    File(usize),
    Line {
        file: usize,
        hunk: usize,
        line: usize,
    },
}

/// What a key on the page asks of the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    None,
    /// Back to the chat; the page stays behind its token.
    Close,
    /// The comments changed: bring the draft's token up to date.
    Comments,
}

/// A comment being written or edited under its target.
#[derive(Debug)]
struct Composing {
    target: Target,
    /// The comment being edited, by its index in the page's comments.
    editing: Option<usize>,
    editor: Editor,
}

#[derive(Debug)]
pub struct ReviewPage {
    diff: Diff,
    patch: String,
    comments: Vec<ReviewComment>,
    doc: ReviewDoc,
    targets: Vec<Target>,
    /// The selected target.
    at: usize,
    /// The first document line on screen.
    scroll: usize,
    composing: Option<Composing>,
    /// The body's height at the last draw, for paging.
    height: usize,
    /// Whose changes these are, for the header: the agent's name and where
    /// it works, as the chat's header says them.
    owner: (String, String),
}

impl ReviewPage {
    pub fn new(diff: Diff, patch: String) -> ReviewPage {
        let doc = review_doc(&diff, &patch, &[]);
        let targets = targets(&doc);
        // Start on the first changed line: that is what the page is for.
        let at = targets
            .iter()
            .position(|target| {
                matches!(target, Target::Line { .. })
                    && line_of(&doc, *target).is_some_and(|line| line.kind != LineKind::Context)
            })
            .unwrap_or(0);
        ReviewPage {
            diff,
            patch,
            comments: Vec::new(),
            doc,
            targets,
            at,
            scroll: 0,
            composing: None,
            height: 0,
            owner: (String::new(), String::new()),
        }
    }

    pub fn comments(&self) -> &[ReviewComment] {
        &self.comments
    }

    /// The draft's token for this review, carrying the frozen diff and
    /// every comment.
    pub fn attachment(&self) -> Attachment {
        Attachment {
            of: Some(attachment::Of::Review(Review {
                diff: Some(self.diff.clone()),
                comments: self.comments.clone(),
            })),
        }
    }

    /// Whether `attachment` is this page's token: a review of the same
    /// patch.
    pub fn owns(&self, attachment: &Attachment) -> bool {
        let Some(attachment::Of::Review(review)) = &attachment.of else {
            return false;
        };
        let theirs = review.diff.as_ref().and_then(|diff| diff.patch.as_ref());
        theirs.map(|patch| &patch.hash) == self.diff.patch.as_ref().map(|patch| &patch.hash)
    }

    /// Whether a comment editor has the keys and holds text, for Ctrl+C.
    pub fn editing(&self) -> bool {
        self.composing
            .as_ref()
            .is_some_and(|composing| !composing.editor.is_empty())
    }

    /// Ctrl+C in the comment editor clears it as a kill.
    pub fn kill_field(&mut self) -> bool {
        self.composing
            .as_mut()
            .is_some_and(|composing| composing.editor.kill_all())
    }

    /// A bracketed paste types into the comment being written.
    pub fn paste(&mut self, text: &str) {
        if let Some(composing) = &mut self.composing {
            composing.editor.insert_str(text);
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> ReviewAction {
        if self.composing.is_some() {
            return self.composing_key(key);
        }
        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
            return ReviewAction::Close;
        }
        if self.targets.is_empty() {
            return ReviewAction::None;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select(self.at + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.at.saturating_sub(1)),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.select(self.at + self.height.max(1).saturating_sub(2).max(1))
            }
            KeyCode::PageUp => self.select(
                self.at
                    .saturating_sub(self.height.max(1).saturating_sub(2).max(1)),
            ),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('J') => self.jump(true, starts_hunk),
            KeyCode::Char('K') => self.jump(false, starts_hunk),
            KeyCode::Char(']') => self.jump(true, |t| matches!(t, Target::File(_))),
            KeyCode::Char('[') => self.jump(false, |t| matches!(t, Target::File(_))),
            KeyCode::Char('c') => self.compose(None),
            KeyCode::Enter if !shift => {
                let newest = self.comments_at(self.target()).last().copied();
                self.compose(newest);
            }
            KeyCode::Char('d') => {
                if let Some(newest) = self.comments_at(self.target()).last().copied() {
                    self.comments.remove(newest);
                    self.refresh();
                    return ReviewAction::Comments;
                }
            }
            _ => {}
        }
        ReviewAction::None
    }

    /// Selects the header of the file at `path`, when the diff has it.
    pub fn show_file(&mut self, path: &str) {
        let found = self.targets.iter().position(|target| {
            matches!(target, Target::File(file) if self.doc.files.get(*file).is_some_and(|f| f.path == path))
        });
        if let Some(at) = found {
            self.select(at);
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.select(self.at.saturating_add_signed(delta));
    }

    fn composing_key(&mut self, key: KeyEvent) -> ReviewAction {
        let Some(composing) = &mut self.composing else {
            return ReviewAction::None;
        };
        match key.code {
            KeyCode::Esc => {
                self.composing = None;
                ReviewAction::None
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                let words = composing.editor.text().trim().to_owned();
                let target = composing.target;
                let editing = composing.editing;
                self.composing = None;
                match (editing, words.is_empty()) {
                    (None, true) => return ReviewAction::None,
                    (Some(index), true) => {
                        self.comments.remove(index);
                    }
                    (Some(index), false) => self.comments[index].text = words,
                    (None, false) => {
                        let comment = self.comment_for(target, words);
                        self.comments.push(comment);
                    }
                }
                self.refresh();
                ReviewAction::Comments
            }
            _ => {
                composing.editor.key(key);
                ReviewAction::None
            }
        }
    }

    fn target(&self) -> Target {
        self.targets[self.at.min(self.targets.len().saturating_sub(1))]
    }

    fn select(&mut self, at: usize) {
        self.at = at.min(self.targets.len().saturating_sub(1));
    }

    /// Moves to the next or previous target where a group starts: a file
    /// header, or a hunk's first line.
    fn jump(&mut self, forward: bool, starts: impl Fn(Target) -> bool) {
        let found = if forward {
            (self.at + 1..self.targets.len()).find(|&i| starts(self.targets[i]))
        } else {
            (0..self.at).rev().find(|&i| starts(self.targets[i]))
        };
        if let Some(at) = found {
            self.at = at;
        }
    }

    fn compose(&mut self, editing: Option<usize>) {
        let mut editor = Editor::default();
        if let Some(index) = editing {
            editor.set(&self.comments[index].text, Vec::new());
        }
        self.composing = Some(Composing {
            target: self.target(),
            editing,
            editor,
        });
    }

    fn comment_for(&self, target: Target, text: String) -> ReviewComment {
        let (file, line) = match target {
            Target::File(file) => (file, None),
            Target::Line { file, .. } => (file, line_of(&self.doc, target)),
        };
        let path = self.doc.files[file].path.clone();
        let (line, old_line) = match line {
            Some(line) => match (line.new_line, line.old_line) {
                (Some(new), _) => (new, 0),
                (None, Some(old)) => (0, old),
                (None, None) => (0, 0),
            },
            None => (0, 0),
        };
        ReviewComment {
            path,
            line,
            old_line,
            text,
        }
    }

    /// Indices of the comments on `target`, oldest first.
    fn comments_at(&self, target: Target) -> Vec<usize> {
        let probe = self.comment_for(target, String::new());
        self.comments
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.path == probe.path && c.line == probe.line && c.old_line == probe.old_line
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Whose changes these are: the agent's name and its place, for the
    /// header.
    pub fn set_owner(&mut self, name: &str, place: &str) {
        self.owner = (name.to_owned(), place.to_owned());
    }

    fn refresh(&mut self) {
        self.doc = review_doc(&self.diff, &self.patch, &self.comments);
    }

    /// Paints the page over the whole of `area`. `footer` replaces the key
    /// hint when the app has something to say.
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        footer: Option<Line<'static>>,
        theme: Theme,
    ) {
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        let (body, selected, cursor) = self.body(width, theme);
        // A blank above the header keeps it off the terminal's edge, as in
        // the chat; hairlines set the file list apart.
        let rule = || Line::from(Span::styled("─".repeat(width), theme.hairline()));
        let mut top = vec![Line::default(), self.title(width, theme), Line::default()];
        top.push(rule());
        top.extend(self.file_list(width, theme));
        top.push(rule());
        let hint = footer.unwrap_or_else(|| {
            let keys: &[(&str, &str)] = if self.composing.is_some() {
                &[("enter", "save"), ("ctrl+j", "newline"), ("esc", "cancel")]
            } else {
                &[
                    ("j/k", "move"),
                    ("J/K", "hunk"),
                    ("]/[", "file"),
                    ("c", "comment"),
                    ("enter", "edit"),
                    ("d", "delete"),
                    ("q", "back"),
                ]
            };
            // The key reads first, its action recedes, as on every hint
            // line.
            let mut line = Line::from(Span::raw("  "));
            for (i, (key, action)) in keys.iter().enumerate() {
                if i > 0 {
                    push(&mut line, "   ", theme.faint(), width);
                }
                push(&mut line, *key, theme.emphasis(), width);
                push(&mut line, format!(" {action}"), theme.faint(), width);
            }
            line
        });
        // The file list gives way to the diff on a short screen.
        let room = height.saturating_sub(1);
        if top.len() + 4 > room {
            top.truncate(room.saturating_sub(4).max(1));
        }
        let body_height = room.saturating_sub(top.len());
        self.height = body_height;
        let (first, last) = selected;
        if first < self.scroll {
            self.scroll = first;
        } else if last + 1 > self.scroll + body_height {
            self.scroll = (last + 1).saturating_sub(body_height);
        }
        self.scroll = self.scroll.min(body.len().saturating_sub(body_height));
        let mut lines = top;
        let body_top = lines.len();
        lines.extend(body.into_iter().skip(self.scroll).take(body_height));
        while lines.len() < room {
            lines.push(Line::default());
        }
        lines.push(hint);
        paint.render_widget(Paragraph::new(lines), area);
        if let Some((row, col)) = cursor
            && row >= self.scroll
            && row < self.scroll + body_height
        {
            let y = area.y as usize + body_top + row - self.scroll;
            let x = area.x as usize + col.min(width.saturating_sub(1));
            paint.set_cursor_position(Position::new(x as u16, y as u16));
        }
    }

    /// Like the chat's header: whose changes at the left (`name │ path`),
    /// what they are against and their counts at the right ("review ·
    /// working tree at 3f2a1c9 │ 2 files · +12 −4 · 1 comment").
    fn title(&self, width: usize, theme: Theme) -> Line<'static> {
        let bar = || Span::styled(" │ ", theme.faint());
        let base = if self.doc.base.is_empty() {
            "working tree".to_owned()
        } else {
            format!("since {}", self.doc.base)
        };
        let head: String = self.doc.head.chars().take(7).collect();
        let mut about = format!("review · {base}");
        if !head.is_empty() {
            about.push_str(&format!(" at {head}"));
        }
        let files = self.doc.files.len();
        let mut counts = format!(
            "{files} file{} · +{} −{}",
            if files == 1 { "" } else { "s" },
            self.doc.added,
            self.doc.removed
        );
        match self.comments.len() {
            0 => {}
            1 => counts.push_str(" · 1 comment"),
            n => counts.push_str(&format!(" · {n} comments")),
        }
        let right = vec![
            Span::styled(about, theme.faint()),
            bar(),
            Span::styled(counts, theme.muted()),
        ];
        let right_width: usize = right.iter().map(|s| text::str_width(&s.content)).sum();
        let right_at = width.saturating_sub(2 + right_width);
        let mut line = Line::from(Span::raw("  "));
        let room = right_at.saturating_sub(2);
        let (name, place) = &self.owner;
        let name = if name.is_empty() { "Review" } else { name };
        push(&mut line, name, theme.bright(), room);
        if !place.is_empty() && text::line_width(&line) + 6 < room {
            line.spans.push(bar());
            push(&mut line, place.clone(), theme.faint(), room);
        }
        if text::line_width(&line) + 2 <= right_at {
            text::pad_to(&mut line, right_at);
            line.spans.extend(right);
        }
        line
    }

    /// One line per file: its status, path, counts and comments.
    fn file_list(&self, width: usize, theme: Theme) -> Vec<Line<'static>> {
        let path_room = self
            .doc
            .files
            .iter()
            .map(|file| text::str_width(&file_name(file)))
            .max()
            .unwrap_or(0)
            .min(width.saturating_sub(24));
        let here = (!self.targets.is_empty()).then(|| file_of(self.target()));
        self.doc
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let mut line = Line::from(Span::raw("  "));
                let (mark, style) = match file.status {
                    FileStatus::Modified => ("M", theme.muted()),
                    FileStatus::Added => ("A", theme.ok()),
                    FileStatus::Deleted => ("D", theme.error()),
                    FileStatus::Renamed => ("R", theme.muted()),
                };
                push(&mut line, format!("{mark}  "), style, width);
                let name = text::ellipsize(&file_name(file), path_room);
                let pad = path_room.saturating_sub(text::str_width(&name));
                let name_style = if Some(i) == here {
                    theme.emphasis()
                } else {
                    theme.text()
                };
                push(&mut line, name, name_style, width);
                push(&mut line, " ".repeat(pad + 2), theme.text(), width);
                if file.binary {
                    push(&mut line, "binary", theme.muted(), width);
                } else {
                    push(&mut line, format!("+{}", file.added), theme.ok(), width);
                    push(
                        &mut line,
                        format!(" −{}", file.removed),
                        theme.error(),
                        width,
                    );
                }
                let comments = file.comments.len()
                    + file
                        .hunks
                        .iter()
                        .flat_map(|hunk| &hunk.lines)
                        .map(|line| line.comments.len())
                        .sum::<usize>();
                if comments > 0 {
                    push(
                        &mut line,
                        format!(
                            " · {comments} comment{}",
                            if comments == 1 { "" } else { "s" }
                        ),
                        theme.accent(),
                        width,
                    );
                }
                line
            })
            .collect()
    }

    /// Every file's section as lines, with the selected target's first and
    /// last line and the comment editor's cursor.
    #[allow(clippy::type_complexity)]
    fn body(
        &self,
        width: usize,
        theme: Theme,
    ) -> (Vec<Line<'static>>, (usize, usize), Option<(usize, usize)>) {
        let digits = self
            .doc
            .files
            .iter()
            .flat_map(|file| file.hunks.iter().flat_map(|hunk| &hunk.lines))
            .flat_map(|line| [line.old_line, line.new_line])
            .flatten()
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(3);
        // "▌ " + old + " " + new + " " + sign + " "
        let gutter = 2 + digits * 2 + 4;
        let selected_target = self.target();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut selected = (0, 0);
        let mut cursor = None;
        for (t, target) in self.targets.iter().enumerate() {
            let is_selected = t == self.at.min(self.targets.len().saturating_sub(1));
            let bar = if is_selected {
                Span::styled("▌ ", theme.focus_bar())
            } else {
                Span::raw("  ")
            };
            let start = lines.len();
            let comments: &[String] = match *target {
                Target::File(f) => {
                    let file = &self.doc.files[f];
                    if f > 0 {
                        lines.push(Line::default());
                    }
                    let mut line = Line::from(bar);
                    push(&mut line, file_name(file), theme.emphasis(), width);
                    if file.binary {
                        push(&mut line, "  binary file", theme.muted(), width);
                    } else {
                        push(&mut line, format!("  +{}", file.added), theme.ok(), width);
                        push(
                            &mut line,
                            format!(" −{}", file.removed),
                            theme.error(),
                            width,
                        );
                    }
                    lines.push(line);
                    &file.comments
                }
                Target::Line { file, hunk, line } => {
                    let h = &self.doc.files[file].hunks[hunk];
                    if line == 0 {
                        let mut header = Line::from(Span::raw("  "));
                        push(&mut header, h.header.clone(), theme.diff_meta(), width);
                        lines.push(header);
                    }
                    let diff_line = &h.lines[line];
                    let number = |n: Option<u32>| match n {
                        Some(n) => format!("{n:>digits$}"),
                        None => " ".repeat(digits),
                    };
                    let (sign, style) = match diff_line.kind {
                        LineKind::Added => ('+', theme.diff_added()),
                        LineKind::Removed => ('-', theme.diff_removed()),
                        LineKind::Context => (' ', theme.diff_context()),
                    };
                    let mut row = Line::from(bar);
                    push(
                        &mut row,
                        format!(
                            "{} {} ",
                            number(diff_line.old_line),
                            number(diff_line.new_line)
                        ),
                        theme.muted(),
                        width,
                    );
                    let body = format!("{sign} {}", diff_line.text.replace('\t', "    "));
                    let room = width.saturating_sub(text::line_width(&row));
                    push(&mut row, text::clip_to_width(&body, room), style, width);
                    if diff_line.kind != LineKind::Context {
                        text::fill(&mut row, style, width);
                    }
                    lines.push(row);
                    &diff_line.comments
                }
            };
            let indent = match target {
                Target::File(_) => 2,
                Target::Line { .. } => gutter,
            };
            for comment in comments {
                for (i, part) in text::wrap(comment, width.saturating_sub(indent + 2).max(1))
                    .into_iter()
                    .enumerate()
                {
                    let mut line = Line::from(Span::raw(" ".repeat(indent)));
                    let lead = if i == 0 { "│ " } else { "  " };
                    push(&mut line, lead, theme.accent(), width);
                    push(&mut line, part, theme.text(), width);
                    lines.push(line);
                }
            }
            if let Some(composing) = self.composing.as_ref().filter(|c| c.target == *target) {
                let placeholder = match composing.editing {
                    Some(_) => "edit the comment; empty removes it",
                    None => "a comment for the agent on this line",
                };
                let room = width.saturating_sub(indent).max(4);
                let (editor, at) = editor_lines(&composing.editor, placeholder, room, theme);
                cursor = Some((lines.len() + at.0, indent + at.1));
                for line in editor {
                    let mut spans = vec![Span::raw(" ".repeat(indent))];
                    spans.extend(line.spans);
                    lines.push(Line::from(spans));
                }
            }
            if *target == selected_target && is_selected {
                selected = (start, lines.len().saturating_sub(1));
            }
        }
        if self.targets.is_empty() {
            let mut line = Line::from(Span::raw("  "));
            push(
                &mut line,
                "No changes in the working tree.",
                theme.muted(),
                width,
            );
            lines.push(line);
        }
        (lines, selected, cursor)
    }
}

fn targets(doc: &ReviewDoc) -> Vec<Target> {
    let mut out = Vec::new();
    for (f, file) in doc.files.iter().enumerate() {
        out.push(Target::File(f));
        for (h, hunk) in file.hunks.iter().enumerate() {
            for l in 0..hunk.lines.len() {
                out.push(Target::Line {
                    file: f,
                    hunk: h,
                    line: l,
                });
            }
        }
    }
    out
}

fn line_of(doc: &ReviewDoc, target: Target) -> Option<&ui_view::DiffLine> {
    match target {
        Target::Line { file, hunk, line } => doc.files.get(file)?.hunks.get(hunk)?.lines.get(line),
        Target::File(_) => None,
    }
}

fn file_of(target: Target) -> usize {
    match target {
        Target::File(file) | Target::Line { file, .. } => file,
    }
}

fn starts_hunk(target: Target) -> bool {
    matches!(target, Target::Line { line: 0, .. })
}

fn file_name(file: &ui_view::ReviewFile) -> String {
    match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    }
}

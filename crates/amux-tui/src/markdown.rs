//! Terminal markdown for assistant messages (`docs/CHAT.md` B2 scope):
//! headings bold keeping their `#`, fenced code blocks, lists, inline
//! emphasis/code, tables as preformatted blocks, URL-aware wrapping that
//! never splits a link.
//!
//! Pure formatting: markdown source in, styled span rows out. Nothing here
//! interprets content — the Model already decided everything (G2: views
//! format, never decide). Wrapping is greedy by word; a word is never
//! split unless it is longer than the whole line (URLs included — the
//! unavoidable case), and code/table lines hard-wrap verbatim so their
//! internal spacing survives.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::render::{Theme, clip_to_width, str_width};

/// One styled run of text (pre-wrap).
type Run = (String, Style);

/// Render one markdown source into rows of spans, wrapped at `width`.
/// Rows are relative to the caller's indent column; blank rows separate
/// blocks exactly as blank source lines do.
pub(crate) fn markdown_rows(source: &str, width: usize, theme: Theme) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut in_fence = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            // Fence markers stay visible, muted — the same keep-the-source
            // honesty as headings keeping their `#`.
            in_fence = !in_fence;
            rows.extend(hard_wrap(line, width, theme.muted()));
            continue;
        }
        if in_fence {
            rows.extend(hard_wrap(line, width, theme.code()));
            continue;
        }
        if line.trim().is_empty() {
            rows.push(Vec::new());
            continue;
        }
        if trimmed.starts_with('|') {
            // Markdown tables render as preformatted blocks in V1 (B2).
            rows.extend(hard_wrap(line, width, theme.text()));
            continue;
        }
        if trimmed.starts_with('#') {
            // Headings bold, keeping their `#`.
            rows.extend(wrap_runs(&[(line.to_string(), theme.emphasis())], width, 0));
            continue;
        }
        if let Some((marker, rest)) = split_list_marker(line) {
            // List items wrap with a hanging indent under their text.
            let mut runs = vec![(marker.clone(), theme.text())];
            runs.extend(inline_runs(rest, theme));
            rows.extend(wrap_runs(&runs, width, marker.chars().count()));
            continue;
        }
        rows.extend(wrap_runs(&inline_runs(line, theme), width, 0));
    }
    rows
}

/// A paragraph of plain text (prompts, notices): newline-respecting,
/// word-wrapped in one style.
pub(crate) fn plain_rows(text: &str, width: usize, style: Style) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            rows.push(Vec::new());
        } else {
            rows.extend(wrap_runs(&[(line.to_string(), style)], width, 0));
        }
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

/// `- ` / `* ` / `+ ` / `12. ` list markers, keeping the source's own
/// leading indent.
fn split_list_marker(line: &str) -> Option<(String, &str)> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    for bullet in ["- ", "* ", "+ "] {
        if let Some(text) = rest.strip_prefix(bullet) {
            return Some((format!("{indent}{bullet}"), text));
        }
    }
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty()
        && let Some(text) = rest[digits.len()..].strip_prefix(". ")
    {
        return Some((format!("{indent}{digits}. "), text));
    }
    None
}

/// Inline `**bold**`, `*italic*`, and `` `code` `` runs; unmatched markers
/// render literally. Deliberately single-level — B2's scope, nothing more.
fn inline_runs(text: &str, theme: Theme) -> Vec<Run> {
    let chars: Vec<char> = text.chars().collect();
    let mut runs: Vec<Run> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    let flush = |plain: &mut String, runs: &mut Vec<Run>| {
        if !plain.is_empty() {
            runs.push((std::mem::take(plain), theme.text()));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '`'
            && let Some(close) = find(&chars, i + 1, '`')
        {
            flush(&mut plain, &mut runs);
            runs.push((chars[i + 1..close].iter().collect(), theme.code()));
            i = close + 1;
            continue;
        }
        if c == '*' {
            let double = chars.get(i + 1) == Some(&'*');
            if double {
                if let Some(close) = find_pair(&chars, i + 2) {
                    flush(&mut plain, &mut runs);
                    runs.push((chars[i + 2..close].iter().collect(), theme.emphasis()));
                    i = close + 2;
                    continue;
                }
            } else if let Some(close) = find(&chars, i + 1, '*') {
                flush(&mut plain, &mut runs);
                runs.push((chars[i + 1..close].iter().collect(), theme.italic()));
                i = close + 1;
                continue;
            }
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut runs);
    runs
}

fn find(chars: &[char], from: usize, wanted: char) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == wanted)
}

fn find_pair(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&i| chars[i] == '*' && chars[i + 1] == '*')
}

/// Greedy word wrap over styled runs. Words never split unless longer than
/// a whole line — the URL-aware rule (B2): a link that fits any line is
/// never broken. `hang` indents continuation rows (list items).
fn wrap_runs(runs: &[Run], width: usize, hang: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let hang = hang.min(width.saturating_sub(1));
    struct Word {
        text: String,
        style: Style,
        /// Preceded by whitespace in the source (first word of a run glues
        /// to the previous run's tail — inline styles split mid-word).
        spaced: bool,
    }
    /// Close the current row and open its continuation (hang-indented);
    /// returns the continuation row's width.
    fn break_row(
        rows: &mut Vec<Vec<Span<'static>>>,
        row: &mut Vec<Span<'static>>,
        width: usize,
        hang: usize,
    ) -> usize {
        rows.push(std::mem::take(row));
        if hang > 0 {
            row.push(Span::raw(" ".repeat(hang)));
        }
        width.saturating_sub(hang).max(1)
    }
    let mut words: Vec<Word> = Vec::new();
    // A run boundary carries a space when either side of it does — inline
    // styles split source text mid-sentence, and `split_whitespace` eats
    // the boundary whitespace.
    let mut prev_trailing = false;
    for (text, style) in runs {
        let leading = text.starts_with(char::is_whitespace);
        let mut first = true;
        for word in text.split_whitespace() {
            words.push(Word {
                text: word.to_string(),
                style: *style,
                spaced: !first || prev_trailing || leading,
            });
            first = false;
        }
        if !text.is_empty() {
            prev_trailing = text.ends_with(char::is_whitespace);
        }
    }

    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut row_width = width;
    for word in words {
        let len = str_width(&word.text);
        let sep = usize::from(word.spaced && used > 0);
        if used > 0 && used + sep + len > row_width {
            row_width = break_row(&mut rows, &mut row, width, hang);
            used = 0;
        } else if sep > 0 {
            row.push(Span::styled(" ".to_string(), word.style));
            used += 1;
        }
        // A word longer than the whole line hard-splits (the unavoidable
        // case, URLs included) — by display cells, at grapheme boundaries.
        let mut remaining = word.text.as_str();
        while str_width(remaining) > row_width.saturating_sub(used) {
            let take = row_width - used;
            let head = clip_to_width(remaining, take);
            if head.is_empty() {
                if used == 0 {
                    // A single grapheme wider than the whole row: take it
                    // anyway (finish_line clips defensively) rather than
                    // loop forever.
                    break;
                }
                row_width = break_row(&mut rows, &mut row, width, hang);
                used = 0;
                continue;
            }
            used += str_width(head);
            row.push(Span::styled(head.to_string(), word.style));
            remaining = &remaining[head.len()..];
        }
        if !remaining.is_empty() {
            used += str_width(remaining);
            row.push(Span::styled(remaining.to_string(), word.style));
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// Verbatim hard wrap (code, tables): internal spacing survives, overlong
/// rows split at the display width, at grapheme boundaries.
fn hard_wrap(line: &str, width: usize, style: Style) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![vec![Span::styled(String::new(), style)]];
    }
    let mut rows = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let head = clip_to_width(rest, width);
        if head.is_empty() {
            // A single grapheme wider than the row: emit it whole rather
            // than loop (finish_line clips defensively).
            use unicode_segmentation::UnicodeSegmentation;
            let grapheme = rest.graphemes(true).next().expect("non-empty rest");
            rows.push(vec![Span::styled(grapheme.to_string(), style)]);
            rest = &rest[grapheme.len()..];
            continue;
        }
        rows.push(vec![Span::styled(head.to_string(), style)]);
        rest = &rest[head.len()..];
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(rows: &[Vec<Span<'static>>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn headings_stay_bold_and_keep_their_hashes() {
        let rows = markdown_rows("## Approach", 40, Theme::default());
        assert_eq!(text_of(&rows), vec!["## Approach"]);
        assert_eq!(rows[0][0].style, Theme::default().emphasis());
    }

    #[test]
    fn fenced_code_renders_verbatim_between_visible_fences() {
        let source = "```rust\nlet x  =  1;\n```";
        let rows = markdown_rows(source, 40, Theme::default());
        assert_eq!(text_of(&rows), vec!["```rust", "let x  =  1;", "```"]);
        assert_eq!(rows[1][0].style, Theme::default().code());
        assert_eq!(rows[0][0].style, Theme::default().muted());
    }

    #[test]
    fn lists_wrap_with_a_hanging_indent() {
        let rows = markdown_rows("- alpha beta gamma delta", 12, Theme::default());
        assert_eq!(text_of(&rows), vec!["- alpha beta", "  gamma", "  delta"]);
    }

    #[test]
    fn inline_emphasis_and_code_style_their_runs() {
        let rows = markdown_rows("mind the **gap** and `code` here", 60, Theme::default());
        assert_eq!(text_of(&rows), vec!["mind the gap and code here"]);
        let styles: Vec<Style> = rows[0].iter().map(|span| span.style).collect();
        assert!(styles.contains(&Theme::default().emphasis()));
        assert!(styles.contains(&Theme::default().code()));
    }

    #[test]
    fn unmatched_markers_render_literally() {
        let rows = markdown_rows("2 * 3 is six", 60, Theme::default());
        assert_eq!(text_of(&rows), vec!["2 * 3 is six"]);
    }

    #[test]
    fn tables_are_preformatted() {
        let source = "| a | b |\n|---|---|\n| 1 | 2 |";
        let rows = markdown_rows(source, 40, Theme::default());
        assert_eq!(text_of(&rows), vec!["| a | b |", "|---|---|", "| 1 | 2 |"]);
    }

    #[test]
    fn urls_never_split_when_they_fit_a_line() {
        let rows = markdown_rows(
            "see https://example.com/a/longer here",
            30,
            Theme::default(),
        );
        assert_eq!(
            text_of(&rows),
            vec!["see", "https://example.com/a/longer", "here"]
        );
    }

    #[test]
    fn overlong_words_hard_split_as_the_last_resort() {
        let rows = markdown_rows("abcdefghij", 4, Theme::default());
        assert_eq!(text_of(&rows), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn plain_rows_respect_newlines() {
        let rows = plain_rows("one\n\ntwo three", 6, Style::default());
        assert_eq!(text_of(&rows), vec!["one", "", "two", "three"]);
    }
}

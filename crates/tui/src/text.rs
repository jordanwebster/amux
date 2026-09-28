//! Cell arithmetic: widths, clipping, wrapping and line assembly. Every
//! measurement in the crate goes through here, with the unicode-width
//! version ratatui renders with, so layout and paint never disagree.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

/// Display width in terminal cells: CJK and emoji two, combining marks zero.
pub(crate) fn str_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// The longest prefix of `text` fitting `max` cells, cut at a grapheme
/// boundary so a wide grapheme never straddles the cut.
pub(crate) fn clip_to_width(text: &str, max: usize) -> &str {
    let mut used = 0usize;
    let mut end = 0usize;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let width = str_width(grapheme);
        if used + width > max {
            break;
        }
        used += width;
        end = offset + grapheme.len();
    }
    &text[..end]
}

/// `text` clipped to `max` cells with an ellipsis when anything was cut.
pub(crate) fn ellipsize(text: &str, max: usize) -> String {
    if str_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}…", clip_to_width(text, max - 1))
}

pub(crate) fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| str_width(&span.content)).sum()
}

/// Appends `text` to `line`, clipped so the line stays within `width`.
pub(crate) fn push(line: &mut Line<'static>, text: impl AsRef<str>, style: Style, width: usize) {
    let room = width.saturating_sub(line_width(line));
    let text = ellipsize(text.as_ref(), room);
    if !text.is_empty() {
        line.spans.push(Span::styled(text, style));
    }
}

/// Puts `text` at the right edge when it fits after what the line holds,
/// with at least two cells between them; otherwise leaves the line alone.
pub(crate) fn push_right(line: &mut Line<'static>, text: &str, style: Style, width: usize) {
    let used = line_width(line);
    let need = str_width(text);
    if text.is_empty() || used + 2 + need > width {
        return;
    }
    line.spans.push(Span::raw(" ".repeat(width - used - need)));
    line.spans.push(Span::styled(text.to_owned(), style));
}

/// Pads the line with blanks up to column `col`.
pub(crate) fn pad_to(line: &mut Line<'static>, col: usize) {
    let used = line_width(line);
    if used < col {
        line.spans.push(Span::raw(" ".repeat(col - used)));
    }
}

/// Pads the line with `style` to `width`, so a surface colour fills it.
pub(crate) fn fill(line: &mut Line<'static>, style: Style, width: usize) {
    let used = line_width(line);
    if used < width {
        line.spans
            .push(Span::styled(" ".repeat(width - used), style));
    }
}

/// Word-wraps one paragraph at `width` cells; a word longer than the line
/// is split. Newlines in `text` start new lines; an empty line stays one.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0usize;
        for word in paragraph.split_word_bounds() {
            let w = str_width(word);
            if used + w <= width {
                line.push_str(word);
                used += w;
                continue;
            }
            if word.trim().is_empty() {
                out.push(std::mem::take(&mut line));
                used = 0;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line).trim_end().to_owned());
            }
            let mut rest = word;
            while str_width(rest) > width {
                let head = clip_to_width(rest, width);
                let head = if head.is_empty() {
                    rest.graphemes(true).next().unwrap_or(rest)
                } else {
                    head
                };
                out.push(head.to_owned());
                rest = &rest[head.len()..];
            }
            line.push_str(rest);
            used = str_width(rest);
        }
        out.push(line.trim_end().to_owned());
    }
    out
}

/// The first line of `text`, for a row that opens to the rest.
pub(crate) fn first_line(text: &str) -> &str {
    text.trim_start().lines().next().unwrap_or_default()
}

/// "1m 42s", "8s", "340ms".
pub(crate) fn duration(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let secs = ms / 1_000;
    if secs < 60 {
        if ms < 10_000 && ms % 1_000 != 0 {
            return format!("{:.1}s", ms as f64 / 1_000.0);
        }
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m {}s", secs % 60);
    }
    format!("{}h {}m", mins / 60, mins % 60)
}

/// "148k", "1.2M", "812".
pub(crate) fn tokens(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// "12 KB", "3.4 MB", "900 B".
pub(crate) fn bytes(size: u64) -> String {
    match size {
        0..1_024 => format!("{size} B"),
        1_024..1_048_576 => format!("{} KB", size / 1_024),
        _ => format!("{:.1} MB", size as f64 / 1_048_576.0),
    }
}

/// "3m ago", "2h ago", "just now": the fleet's last-activity column.
/// A fleet cell's age: "now" under a minute, then minutes, hours, days.
pub(crate) fn age(now_ms: i64, then_ms: i64) -> String {
    if then_ms <= 0 {
        return String::new();
    }
    let secs = (now_ms - then_ms).max(0) / 1_000;
    match secs {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_at_words_and_splits_only_overlong_ones() {
        assert_eq!(wrap("alpha beta gamma", 10), vec!["alpha beta", "gamma"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("one\n\ntwo", 10), vec!["one", "", "two"]);
    }

    #[test]
    fn wide_graphemes_never_straddle_a_clip() {
        assert_eq!(clip_to_width("日本語", 5), "日本");
        assert_eq!(ellipsize("日本語", 5), "日本…");
    }

    #[test]
    fn durations_read_the_way_the_vocabulary_writes_them() {
        assert_eq!(duration(102_000), "1m 42s");
        assert_eq!(duration(8_000), "8s");
        assert_eq!(duration(4_200), "4.2s");
        assert_eq!(tokens(148_000), "148k");
    }

    #[test]
    fn right_meta_is_dropped_rather_than_colliding() {
        let mut line = Line::from("left");
        push_right(&mut line, "meta", Style::default(), 9);
        assert_eq!(line_width(&line), 4);
        push_right(&mut line, "meta", Style::default(), 12);
        assert_eq!(line_width(&line), 12);
    }
}

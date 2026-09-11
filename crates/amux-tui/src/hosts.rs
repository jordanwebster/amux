//! Host captions and the fleet's host inventory overlay.

use amux_ui::{HostEntry, HostTrustStatus, HostVia, Model};
use ratatui::text::{Line, Span};

use crate::render::{clip_to_width, push_span};
use crate::theme::Theme;

const LABEL_COL: usize = 4;
const ROUTE_COL: usize = 32;

/// The route wording shared by fleet rows and the host inventory.
pub(crate) fn host_caption(model: &Model, entry: &HostEntry) -> String {
    let route = route_label(model, entry);
    let binding = if matches!(route, "offline" | "relay") && entry.signed_in == Some(false) {
        ", not signed in"
    } else {
        ""
    };
    format!("{} ·{route}{binding}", entry.name)
}

fn route_label<'a>(model: &Model, entry: &'a HostEntry) -> &'a str {
    match entry.via {
        HostVia::Direct => "direct",
        HostVia::Relay if model.host_is_away(entry.id) && entry.signed_in != Some(false) => "away",
        HostVia::Relay => "relay",
        HostVia::Ssh => "ssh",
        HostVia::Offline => "offline",
    }
}

/// Every trusted host and every currently visible pairing candidate.
pub(crate) fn hosts_overlay_lines(model: &Model, width: u16, theme: Theme) -> Vec<Line<'static>> {
    let mut hosts = model.hosts().map(|state| &state.entry).collect::<Vec<_>>();
    hosts.sort_unstable_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.id.cmp(&right.id))
    });

    let mut lines = vec![line_at(
        LABEL_COL,
        "hosts".to_string(),
        theme.emphasis(),
        theme,
    )];
    lines.push(Line::from(Span::styled("│", theme.muted())));
    if hosts.is_empty() {
        lines.push(line_at(
            LABEL_COL,
            "no hosts known".to_string(),
            theme.muted(),
            theme,
        ));
        return lines;
    }

    let content_width = usize::from(width).saturating_sub(ROUTE_COL + 2);
    for entry in hosts {
        let mut line = Line::from(Span::styled("│", theme.muted()));
        push_span(
            &mut line,
            LABEL_COL,
            clip_to_width(&entry.name, ROUTE_COL - LABEL_COL - 2).to_string(),
            match entry.trust_status {
                HostTrustStatus::Trusted => theme.text(),
                HostTrustStatus::UntrustedButOnline => theme.muted(),
            },
        );
        let detail = match entry.trust_status {
            HostTrustStatus::Trusted => {
                let caption = host_caption(model, entry);
                caption
                    .strip_prefix(&entry.name)
                    .unwrap_or(&caption)
                    .trim_start()
                    .to_string()
            }
            HostTrustStatus::UntrustedButOnline => format!(
                "{} · run amux pair {}",
                match entry.via {
                    HostVia::Direct => "found",
                    HostVia::Relay => "seen through relay",
                    HostVia::Ssh => "seen over SSH",
                    HostVia::Offline => "offline",
                },
                shell_target(&entry.name)
            ),
        };
        push_span(
            &mut line,
            ROUTE_COL,
            clip_to_width(&detail, content_width).to_string(),
            theme.muted(),
        );
        lines.push(line);
    }
    lines
}

fn line_at(col: usize, text: String, style: ratatui::style::Style, theme: Theme) -> Line<'static> {
    let mut line = Line::from(Span::styled("│", theme.muted()));
    push_span(&mut line, col, text, style);
    line
}

fn shell_target(target: &str) -> String {
    if target
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        target.to_string()
    } else {
        format!("'{}'", target.replace('\'', "'\\''"))
    }
}

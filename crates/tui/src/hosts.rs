//! The hosts overlay: every trusted host whatever its presence, and every
//! candidate discovery currently sees, straight from the inventory.

use ratatui::text::{Line, Span};
use ui_state::FleetState;
use wire::{HostEntry, HostVia, Presence, Trust};

use crate::text::push;
use crate::theme::Theme;

const ROUTE_COL: usize = 28;

fn route(entry: &HostEntry) -> &'static str {
    match entry.via() {
        HostVia::Direct => "direct",
        HostVia::Relay => "relay",
        HostVia::Ssh => "ssh",
        HostVia::Unspecified => "",
    }
}

/// How a trusted host is reached now, in words.
pub fn caption(entry: &HostEntry) -> String {
    let mut parts = vec![match entry.presence() {
        Presence::Online => "online".to_owned(),
        Presence::Away => "away".to_owned(),
        Presence::Offline => "offline".to_owned(),
        Presence::Unspecified => "unknown".to_owned(),
    }];
    let via = route(entry);
    if entry.presence() == Presence::Online && !via.is_empty() {
        parts.push(via.to_owned());
    }
    // Signing in matters only for reaching a host through the relay.
    let relay = entry.presence() != Presence::Online || entry.via() == HostVia::Relay;
    if relay && entry.signed_in == Some(false) {
        parts.push("not signed in".into());
    }
    if let Some(error) = entry.last_dial_error.as_ref().filter(|e| !e.is_empty()) {
        parts.push(error.clone());
    }
    parts.join(" · ")
}

fn shell_target(target: &str) -> String {
    if target
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        target.to_owned()
    } else {
        format!("'{}'", target.replace('\'', "'\\''"))
    }
}

pub fn overlay_lines(fleet: &FleetState, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut hosts: Vec<&HostEntry> = fleet.hosts().collect();
    hosts.sort_by(|a, b| {
        (a.trust() != Trust::Trusted)
            .cmp(&(b.trust() != Trust::Trusted))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.host_id.cmp(&b.host_id))
    });
    let mut lines = vec![
        Line::from(Span::styled("  Hosts", theme.emphasis())),
        Line::default(),
    ];
    if hosts.is_empty() {
        lines.push(Line::from(Span::styled(
            "    No hosts known",
            theme.muted(),
        )));
    }
    let mut candidates = false;
    for entry in hosts {
        let trusted = entry.trust() == Trust::Trusted;
        if !trusted && !candidates {
            candidates = true;
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "  Found nearby, not paired",
                theme.muted(),
            )));
        }
        let mut line = Line::from(Span::raw("    "));
        let glyph = match (trusted, entry.presence()) {
            (true, Presence::Online) => ("● ", theme.ok()),
            (true, _) => ("○ ", theme.muted()),
            (false, _) => ("+ ", theme.muted()),
        };
        push(&mut line, glyph.0, glyph.1, width);
        let name = if entry.name.is_empty() {
            "unnamed host"
        } else {
            &entry.name
        };
        push(
            &mut line,
            name,
            if trusted { theme.text() } else { theme.muted() },
            ROUTE_COL,
        );
        let used = crate::text::line_width(&line);
        if used < ROUTE_COL {
            line.spans.push(Span::raw(" ".repeat(ROUTE_COL - used)));
        } else {
            line.spans.push(Span::raw("  "));
        }
        let detail = if trusted {
            caption(entry)
        } else {
            let mut words = String::from("found");
            if let Some(platform) = entry.platform.as_ref().filter(|p| !p.is_empty()) {
                words.push_str(&format!(" · {platform}"));
            }
            words.push_str(&format!(" · amux pair {}", shell_target(name)));
            words
        };
        push(&mut line, detail, theme.muted(), width);
        lines.push(line);
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("  esc close", theme.muted())));
    lines
}

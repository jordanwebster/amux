//! Host words shared by the fleet rows and the hosts overlay: how each host
//! is reached, every trusted host whatever its presence and every candidate
//! discovery currently sees, and the one banner line a reachability problem
//! earns.

use ratatui::text::{Line, Span};
use ui_state::FleetState;
use wire::{HostEntry, HostVia, Presence, Trust};

use crate::text::{self, pad_to};
use crate::theme::Theme;

/// How a trusted host is reached now, in one word: the route while it is
/// online, else its presence. This machine's own entry has no route.
pub fn route(entry: &HostEntry, local_host: &[u8]) -> &'static str {
    if entry.revoked == Some(true) {
        return "offline";
    }
    match entry.presence() {
        Presence::Offline => "offline",
        Presence::Away => "away",
        Presence::Online | Presence::Unspecified => match entry.via() {
            HostVia::Direct => "direct",
            HostVia::Relay => "relay",
            HostVia::Ssh => "ssh",
            HostVia::Unspecified if entry.host_id == local_host => "local",
            HostVia::Unspecified => "online",
        },
    }
}

/// The overlay's words for a trusted host: its route, then what stands in
/// its way. A host that said it no longer trusts this machine says that.
/// While this machine is signed out, a host that is not online is away for
/// that reason as far as anyone here can say, and the words name this
/// machine rather than the host.
pub fn caption(entry: &HostEntry, local: Option<&HostEntry>) -> String {
    let local_host = local.map_or(&[][..], |local| &local.host_id[..]);
    let here_signed_out = local.is_some_and(|local| local.signed_in == Some(false));
    let here = local.is_some_and(|local| local.host_id == entry.host_id);
    let mut parts = vec![format!("·{}", route(entry, local_host))];
    let online = entry.presence() == Presence::Online && entry.revoked != Some(true);
    // Signing in matters only for reaching a host through the relay.
    let relay = !online || entry.via() == HostVia::Relay;
    if entry.revoked == Some(true) {
        parts.push("no longer trusts this machine".into());
    } else if here_signed_out && (here || !online) {
        parts.push("this machine is signed out".into());
    } else if relay && entry.signed_in == Some(false) {
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

/// The trusted hosts, then what discovery found, each by name.
pub fn listed(fleet: &FleetState) -> Vec<&HostEntry> {
    let mut hosts: Vec<&HostEntry> = fleet.hosts().collect();
    hosts.sort_by(|a, b| {
        (a.trust() != Trust::Trusted)
            .cmp(&(b.trust() != Trust::Trusted))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.host_id.cmp(&b.host_id))
    });
    hosts
}

/// The hosts modal's rows: one line per host, its name (this machine said
/// so), then how it is reached ("direct", "relay") or why not ("away · not
/// signed in"); what discovery found, faint, with the command that pairs
/// it; and a faint footer on pairing another.
pub fn modal_rows(fleet: &FleetState, local_host: &[u8], theme: Theme) -> Vec<Line<'static>> {
    let local = fleet.host(local_host);
    let hosts = listed(fleet);
    let names: Vec<String> = hosts
        .iter()
        .map(|entry| {
            if entry.name.is_empty() {
                "unnamed host".to_owned()
            } else {
                entry.name.clone()
            }
        })
        .collect();
    let column = names
        .iter()
        .map(|name| text::str_width(name))
        .max()
        .unwrap_or(0)
        + 3;
    let mut rows = vec![Line::default()];
    if hosts.is_empty() {
        rows.push(Line::from(Span::styled("No hosts known", theme.muted())));
    }
    for (entry, name) in hosts.iter().zip(names) {
        let trusted = entry.trust() == Trust::Trusted;
        let mut line = Line::default();
        let ink = if trusted { theme.text() } else { theme.faint() };
        line.spans.push(Span::styled(name.clone(), ink));
        pad_to(&mut line, column);
        let words = if entry.host_id == local_host {
            "this machine".to_owned()
        } else if trusted {
            caption(entry, local).trim_start_matches('·').to_owned()
        } else {
            format!("found nearby · amux pair {}", shell_target(&name))
        };
        line.spans.push(Span::styled(
            words,
            if trusted {
                theme.muted()
            } else {
                theme.faint()
            },
        ));
        rows.push(line);
    }
    rows.push(Line::default());
    rows.push(Line::from(vec![
        Span::styled("Pair another machine with ", theme.faint()),
        Span::styled("amux pair", theme.code()),
    ]));
    rows.push(Line::from(vec![
        Span::styled("esc", theme.muted()),
        Span::styled(" close", theme.faint()),
    ]));
    rows
}

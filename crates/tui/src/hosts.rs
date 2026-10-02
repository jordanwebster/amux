//! Host words shared by the fleet rows and the hosts overlay: how each host
//! is reached, every trusted host whatever its presence and every candidate
//! discovery currently sees, and the one banner line a reachability problem
//! earns.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ui_state::FleetState;
use wire::{HostEntry, HostVia, Presence, Trust};

use crate::text::{self, pad_to};
use crate::theme::Theme;

/// Overlay columns, inside the fleet's frame.
const NAME_COL: usize = 3;
const ROUTE_COL: usize = 31;

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

/// A fleet row's host cell: the name and how it is reached.
pub fn host_cell(entry: &HostEntry, local_host: &[u8]) -> String {
    format!("{} ·{}", entry.name, route(entry, local_host))
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

/// The overlay's rows, `width` cells wide, for the fleet's frame to hold.
pub fn overlay_lines(
    fleet: &FleetState,
    local_host: &[u8],
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let local = fleet.host(local_host);
    let hosts = listed(fleet);
    let mut heading = Line::default();
    pad_to(&mut heading, NAME_COL);
    heading.spans.push(Span::styled("hosts", theme.emphasis()));
    let mut lines = vec![heading, Line::default()];
    if hosts.is_empty() {
        let mut line = Line::default();
        pad_to(&mut line, NAME_COL);
        line.spans
            .push(Span::styled("no hosts known", theme.muted()));
        lines.push(line);
    }
    for entry in hosts {
        let trusted = entry.trust() == Trust::Trusted;
        let name = if entry.name.is_empty() {
            "unnamed host"
        } else {
            &entry.name
        };
        let mut line = Line::default();
        pad_to(&mut line, NAME_COL);
        line.spans.push(Span::styled(
            text::ellipsize(name, ROUTE_COL - NAME_COL - 2),
            if trusted { theme.text() } else { theme.muted() },
        ));
        pad_to(&mut line, ROUTE_COL);
        let detail = if trusted {
            caption(entry, local)
        } else {
            let mut words = String::from("found");
            if let Some(platform) = entry.platform.as_ref().filter(|p| !p.is_empty()) {
                words.push_str(&format!(" · {platform}"));
            }
            words.push_str(&format!(" · run amux pair {}", shell_target(name)));
            words
        };
        text::push(&mut line, detail, theme.muted(), width);
        lines.push(line);
    }
    lines
}

/// The one reachability problem worth a line of its own, loudest first: a
/// daemon running a newer build than this client, a host that no longer
/// trusts this machine, a host that is away, or this machine signed out
/// while a host is out of reach. A host that is merely offline says so on
/// its own rows.
pub fn banner(
    fleet: &FleetState,
    local_host: &[u8],
    version: &str,
    theme: Theme,
) -> Option<(String, Style)> {
    // A daemon that restarted into a newer build keeps serving this older
    // client; only the person can restart it.
    if let Some(running) = fleet
        .host(local_host)
        .and_then(|host| host.version.as_deref())
        .filter(|running| !version.is_empty() && *running != version)
    {
        return Some((
            format!("⚠ amux {running} is running · restart to update"),
            theme.warning(),
        ));
    }
    let hosts = listed(fleet);
    let trusted = || {
        hosts
            .iter()
            .filter(|host| host.trust() == Trust::Trusted && host.host_id != local_host)
    };
    if let Some(host) = trusted().find(|host| host.revoked == Some(true)) {
        return Some((
            format!(
                "⚠ {} no longer trusts this machine · run amux pair {} to pair again",
                host.name,
                shell_target(&host.name)
            ),
            theme.warning(),
        ));
    }
    if let Some(host) = trusted().find(|host| host.presence() == Presence::Away) {
        return Some((
            format!("⚠ {} is away · its agents are as it last said", host.name),
            theme.warning(),
        ));
    }
    let signed_out = ui_view::signed_out(fleet, local_host);
    if signed_out && trusted().any(|host| host.presence() != Presence::Online) {
        return Some((
            "sign in to reach your agents from anywhere · amux login".to_owned(),
            theme.warning(),
        ));
    }
    None
}

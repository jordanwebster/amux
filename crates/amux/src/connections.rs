//! Pure formatting and parsing for the CLI's device-connection surfaces.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use qrcode::QrCode;
use qrcode::render::unicode;

pub const INSTALL_LINK: &str = "https://amux.sh/get";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairTarget {
    Found(String),
    Addr(SocketAddr),
    Ssh(String),
}

pub fn parse_pair_target(target: &str) -> Result<PairTarget> {
    let target = target.trim();
    if target.is_empty() {
        return Err(anyhow!("pairing target cannot be empty"));
    }
    if let Ok(addr) = target.parse::<SocketAddr>() {
        return Ok(PairTarget::Addr(addr));
    }
    if target.contains('@') {
        return Ok(PairTarget::Ssh(target.to_string()));
    }
    Ok(PairTarget::Found(target.to_string()))
}

pub fn resolve_pairing_candidate(
    candidates: &[node::PairingCandidate],
    target: &str,
) -> Result<node::PairingCandidate> {
    if let Ok(id) = uuid::Uuid::parse_str(target)
        && let Some(candidate) = candidates.iter().find(|candidate| candidate.host.id == id)
    {
        return Ok(candidate.clone());
    }

    let matches = candidates
        .iter()
        .filter(|candidate| candidate.host.name == target)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [candidate] => Ok((*candidate).clone()),
        [] => Err(anyhow!(
            "no pairing candidate named `{target}`. Run `amux peer list` to list hosts that can be paired."
        )),
        _ => Err(anyhow!(
            "multiple pairing candidates are named `{target}`; use the host ID shown by `amux peer list`"
        )),
    }
}

pub fn terminal_qr_code(payload: &str) -> Result<String> {
    let code = QrCode::new(payload.as_bytes()).context("failed to encode terminal QR")?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}

pub fn onramp_lines(host_name: &str, code: &str, ttl: Duration, install_url: &str) -> Vec<String> {
    let code = format_pairing_code(code);
    vec![
        terminal_qr_code(install_url).expect("the fixed install URL must encode as a QR code"),
        format!(
            "Pairing code: {code} · valid for {}",
            format_pairing_ttl(ttl.as_secs())
        ),
        format!("On your phone, install amux, and it will find {host_name} on this network."),
        "Run `amux pair` for a fresh code.".to_string(),
        "amux is free on your network. Sign in to reach your agents from anywhere: amux login"
            .to_string(),
    ]
}

pub fn format_peer_list(
    peers: &[node::PeerEntry],
    hosts: &[node::HostEntry],
    candidates: &[node::PairingCandidate],
    tier: Option<node::Tier>,
) -> String {
    let mut rows = peers
        .iter()
        .map(|peer| {
            let host = hosts.iter().find(|host| host.id == peer.host_id);
            vec![
                peer.name.clone(),
                peer.host_id.to_string(),
                trusted_peer_via(host, tier).to_string(),
                "yes".to_string(),
            ]
        })
        .collect::<Vec<_>>();
    rows.sort_unstable_by(|left, right| left[0].cmp(&right[0]).then(left[1].cmp(&right[1])));

    let trusted = peers
        .iter()
        .map(|peer| peer.host_id)
        .collect::<HashSet<_>>();
    let mut found = candidates
        .iter()
        .filter(|candidate| !trusted.contains(&candidate.host.id))
        .map(|candidate| {
            vec![
                candidate.host.name.clone(),
                candidate.host.id.to_string(),
                candidate_via(candidate.via).to_string(),
                format!(
                    "pair with: amux pair {}",
                    shell_target(&candidate.host.name)
                ),
            ]
        })
        .collect::<Vec<_>>();
    found.sort_unstable_by(|left, right| left[0].cmp(&right[0]).then(left[1].cmp(&right[1])));
    rows.extend(found);

    format_table(["HOST", "ID", "VIA", "PAIRED"], &rows)
}

pub fn pairing_identity_lines(pending: &node::PendingPeer) -> [String; 2] {
    [
        format!("Host: {} ({})", pending.name, pending.host_id),
        format!("Fingerprint: {}", pending.fingerprint),
    ]
}

pub fn pairing_success_line(peer: &node::PeerEntry, via: node::PeerVia) -> String {
    let route = match via {
        node::PeerVia::Direct => "on this network",
        node::PeerVia::Relay => "through the relay",
        node::PeerVia::Ssh => "over SSH",
    };
    format!("Paired with {} ({}) {route}", peer.name, peer.host_id)
}

fn format_pairing_code(code: &str) -> String {
    let digits = code
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    if digits.len() == 6 {
        format!("{} {}", &digits[..3], &digits[3..])
    } else {
        code.to_string()
    }
}

fn format_pairing_ttl(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 && s >= 86_400 => plural(s / 86_400, "day"),
        s if s % 3_600 == 0 && s >= 3_600 => plural(s / 3_600, "hour"),
        s if s % 60 == 0 && s >= 60 => plural(s / 60, "minute"),
        s => plural(s, "second"),
    }
}

fn plural(value: u64, unit: &str) -> String {
    format!("{value} {unit}{}", if value == 1 { "" } else { "s" })
}

fn trusted_peer_via(host: Option<&node::HostEntry>, tier: Option<node::Tier>) -> &'static str {
    let Some(host) = host else {
        return "offline";
    };
    match host.via {
        node::HostVia::Direct => "direct",
        node::HostVia::Relay if tier == Some(node::Tier::Free) && host.signed_in != Some(false) => {
            "away"
        }
        node::HostVia::Relay => "relay",
        node::HostVia::Ssh => "ssh",
        node::HostVia::Offline if host.signed_in == Some(false) => "offline, not signed in",
        node::HostVia::Offline => "offline",
    }
}

fn candidate_via(via: node::PeerVia) -> &'static str {
    match via {
        node::PeerVia::Direct => "found · on this network",
        node::PeerVia::Relay => "seen · through the relay",
        node::PeerVia::Ssh => "seen · over SSH",
    }
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

fn format_table(headers: [&str; 4], rows: &[Vec<String>]) -> String {
    let mut widths = headers.map(display_width);
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(display_width(value));
        }
    }

    let mut output = String::new();
    append_row(&mut output, &headers.map(str::to_string), &widths);
    for row in rows {
        append_row(&mut output, row, &widths);
    }
    output
}

fn append_row(output: &mut String, values: &[String], widths: &[usize; 4]) {
    for (index, value) in values.iter().enumerate() {
        output.push_str(value);
        if index < values.len() - 1 {
            let padding = widths[index] - display_width(value) + 2;
            output.extend(std::iter::repeat_n(' ', padding));
        }
    }
    output.push('\n');
}

fn display_width(value: &str) -> usize {
    value.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onramp_formats_install_qr_code_and_copy() {
        let lines = onramp_lines("nova", "481923", Duration::from_secs(15 * 60), INSTALL_LINK);
        assert!(lines[0].lines().count() > 4);
        assert_eq!(
            &lines[1..],
            [
                "Pairing code: 481 923 · valid for 15 minutes",
                "On your phone, install amux, and it will find nova on this network.",
                "Run `amux pair` for a fresh code.",
                "amux is free on your network. Sign in to reach your agents from anywhere: amux login",
            ]
        );
    }

    #[test]
    fn onramp_target_parser_distinguishes_found_address_and_ssh() {
        assert_eq!(
            parse_pair_target("nova").unwrap(),
            PairTarget::Found("nova".into())
        );
        assert_eq!(
            parse_pair_target("127.0.0.1:4242").unwrap(),
            PairTarget::Addr("127.0.0.1:4242".parse().unwrap())
        );
        assert_eq!(
            parse_pair_target("me@nova").unwrap(),
            PairTarget::Ssh("me@nova".into())
        );
        assert!(parse_pair_target("  ").is_err());
    }

    #[test]
    fn onramp_peer_table_combines_trusted_and_found_hosts() {
        let direct = peer(1, "desktop", direct_route());
        let relay = peer(2, "relay-host", node::PeerReachability::Cloud);
        let hosts = vec![host(1, "desktop", true), host(2, "relay-host", false)];
        let candidates = vec![candidate(3, "phone", node::PeerVia::Direct)];
        let output = format_peer_list(&[direct, relay], &hosts, &candidates, None);

        assert!(output.starts_with("HOST"));
        assert!(output.contains("ID"));
        assert!(output.contains("VIA"));
        assert!(output.contains("PAIRED"));
        assert!(output.contains("desktop"));
        assert!(output.contains("direct"));
        assert!(output.contains("relay-host"));
        assert!(output.contains("offline"));
        assert!(output.contains("found · on this network"));
        assert!(output.contains("pair with: amux pair phone"));
    }

    #[test]
    fn onramp_peer_table_formats_relay_candidates_and_empty_tables() {
        let output = format_peer_list(
            &[],
            &[],
            &[candidate(4, "away", node::PeerVia::Relay)],
            None,
        );
        assert!(output.contains("seen · through the relay"));
        assert_eq!(
            format_peer_list(&[], &[], &[], None),
            "HOST  ID  VIA  PAIRED\n"
        );
        let spaced = format_peer_list(
            &[],
            &[],
            &[candidate(7, "My Mac", node::PeerVia::Direct)],
            None,
        );
        assert!(spaced.contains("pair with: amux pair 'My Mac'"));
    }

    #[test]
    fn peer_table_distinguishes_free_relay_presence_and_unsigned_offline_hosts() {
        let away_peer = peer(8, "away-host", node::PeerReachability::Cloud);
        let unsigned_peer = peer(9, "unsigned-host", node::PeerReachability::Cloud);
        let mut away = host(8, "away-host", true);
        away.via = node::HostVia::Relay;
        let mut unsigned = host(9, "unsigned-host", false);
        unsigned.signed_in = Some(false);

        let output = format_peer_list(
            &[away_peer, unsigned_peer],
            &[away, unsigned],
            &[],
            Some(node::Tier::Free),
        );
        assert!(output.contains("away-host"));
        assert!(output.contains("away"));
        assert!(output.contains("offline, not signed in"));
    }

    #[test]
    fn onramp_candidate_resolution_accepts_id_and_rejects_ambiguous_names() {
        let first = candidate(5, "phone", node::PeerVia::Direct);
        let second = candidate(6, "phone", node::PeerVia::Relay);
        assert_eq!(
            resolve_pairing_candidate(std::slice::from_ref(&first), "phone")
                .unwrap()
                .host
                .id,
            first.host.id
        );
        assert_eq!(
            resolve_pairing_candidate(std::slice::from_ref(&first), &first.host.id.to_string())
                .unwrap()
                .host
                .id,
            first.host.id
        );
        assert!(resolve_pairing_candidate(&[first, second], "phone").is_err());
        assert!(resolve_pairing_candidate(&[], "missing").is_err());
    }

    fn host(id: u128, name: &str, online: bool) -> node::HostEntry {
        node::HostEntry {
            id: uuid::Uuid::from_u128(id),
            name: name.into(),
            online,
            version: Some("test".into()),
            capabilities: Some(node::Capabilities::default()),
            trust_status: node::HostTrustStatus::Trusted,
            last_dial_error: None,
            via: if online {
                node::HostVia::Direct
            } else {
                node::HostVia::Offline
            },
            signed_in: Some(true),
            platform: None,
        }
    }

    fn peer(id: u128, name: &str, reachability: node::PeerReachability) -> node::PeerEntry {
        node::PeerEntry {
            host_id: uuid::Uuid::from_u128(id),
            name: name.into(),
            pubkey: vec![7; 32],
            fingerprint: "aa:bb".into(),
            paired_at: chrono::DateTime::from_timestamp(200, 0).unwrap(),
            reachabilities: vec![reachability],
        }
    }

    fn candidate(id: u128, name: &str, via: node::PeerVia) -> node::PairingCandidate {
        let mut host = host(id, name, true);
        host.trust_status = node::HostTrustStatus::UntrustedButOnline;
        node::PairingCandidate {
            host,
            via,
            addrs: Vec::new(),
        }
    }

    fn direct_route() -> node::PeerReachability {
        node::PeerReachability::Direct {
            addrs: vec!["127.0.0.1:4242".parse().unwrap()],
        }
    }
}

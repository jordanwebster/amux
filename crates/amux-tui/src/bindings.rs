//! The one named binding table (`docs/CHAT.md` §Keybindings).
//!
//! Dispatch stays in the key handlers; everything DISCOVERABLE derives
//! from here — the fleet help overlay, the chat `?` overlay, and the
//! tier-gated hints — so hints can never advertise a dead key (P10).
//! Three tiers: plain (guaranteed ANSI, always hintable), ext (standard
//! CSI, convenience only, marked terminal-dependent in the overlay),
//! kitty (feature-detected sugar, hidden when the terminal cannot
//! deliver it). The shape is palette-ready data — sections of typed
//! rows — which is a data-shape concern, not a feature.

use crate::view::OpenMode;

/// Delivery tier of a binding (`docs/CHAT.md` §Keybindings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Guaranteed ANSI bytes: always hintable.
    Plain,
    /// Standard CSI most emulators deliver: convenience only, never the
    /// sole path to an action; the overlay marks it terminal-dependent.
    Ext,
    /// Kitty-keyboard-protocol sugar: shown only when the probe said the
    /// terminal delivers it.
    Kitty,
}

/// One row of the table: a key chord, what it does, its tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub keys: String,
    pub action: String,
    pub tier: Tier,
}

/// A titled group of bindings (one focus context).
#[derive(Clone, Debug)]
pub struct Section {
    pub title: &'static str,
    pub bindings: Vec<Binding>,
}

/// The terminal/config facts the effective table depends on.
#[derive(Clone, Debug)]
pub struct Effective {
    /// Whether the kitty probe succeeded — kitty-tier rows exist iff so.
    pub kitty: bool,
    /// The configured leader's label ("C-a"), substituted into chords.
    pub leader_label: String,
}

impl Effective {
    /// Build from the probed kitty fact and the configured leader
    /// character — the `C-<leader>` label rule lives here, once.
    pub fn new(kitty: bool, leader: char) -> Self {
        Self {
            kitty,
            leader_label: format!("C-{leader}"),
        }
    }
}

fn row(keys: impl Into<String>, action: impl Into<String>, tier: Tier) -> Binding {
    Binding {
        keys: keys.into(),
        action: action.into(),
        tier,
    }
}

fn mode_name(mode: OpenMode) -> &'static str {
    match mode {
        OpenMode::RawAttach => "raw attach",
        OpenMode::Chat => "chat",
    }
}

/// The fleet's effective bindings (kitty rows already filtered); the
/// entry rows name the effective modes from the A1 default.
pub fn fleet_sections(eff: &Effective, default_open_mode: OpenMode) -> Vec<Section> {
    let default = mode_name(default_open_mode);
    let other = mode_name(default_open_mode.other());
    let mut fleet = vec![
        row("j/k or ↑/↓", "move", Tier::Plain),
        row("gg / G", "top / bottom", Tier::Plain),
        row("/ or i", "filter", Tier::Plain),
        row("enter", format!("open in {default}"), Tier::Plain),
    ];
    if eff.kitty {
        fleet.push(row("ctrl+enter", format!("open in {other}"), Tier::Kitty));
    }
    fleet.extend([
        row("o", format!("open in {other}"), Tier::Plain),
        row("n", "new agent", Tier::Plain),
        row("r", "rename selected", Tier::Plain),
        row("d", "delete selected", Tier::Plain),
        row("C-g", "debug dump", Tier::Plain),
        row("q", "quit", Tier::Plain),
        row("ctrl+c ctrl+c", "quit (guarded: two presses)", Tier::Plain),
    ]);
    let attach = vec![
        row(
            format!("{} d", eff.leader_label),
            "detach to shell",
            Tier::Plain,
        ),
        row(
            format!("{} s", eff.leader_label),
            "back to fleet",
            Tier::Plain,
        ),
    ];
    vec![
        Section {
            title: "fleet",
            bindings: fleet,
        },
        Section {
            title: "attached",
            bindings: attach,
        },
    ]
}

/// The chat's effective bindings, grouped by focus context (the full key
/// list the `?` overlay renders; kitty rows already filtered).
pub fn chat_sections(eff: &Effective) -> Vec<Section> {
    let chat = vec![
        row("ctrl+x", "interrupt the agent", Tier::Plain),
        row("esc", "back one stage (never answers)", Tier::Plain),
        row("pgup/pgdn", "scroll the feed", Tier::Plain),
        row("ctrl+home/end", "feed oldest / newest", Tier::Ext),
        row("ctrl+t", "read accepted plans (←/→ steps)", Tier::Plain),
        row(
            "ctrl+c",
            "clear field; empty: press twice to quit",
            Tier::Plain,
        ),
        row(
            format!("{} s", eff.leader_label),
            "back to fleet",
            Tier::Plain,
        ),
        row(
            format!("{} d", eff.leader_label),
            "detach to shell",
            Tier::Plain,
        ),
    ];
    let mut composer = vec![
        row("enter", "send", Tier::Plain),
        row("ctrl+j", "newline", Tier::Plain),
    ];
    if eff.kitty {
        composer.push(row("shift+enter", "newline", Tier::Kitty));
    }
    composer.extend([
        row("shift+tab", "cycle permission mode", Tier::Plain),
        row("home/end · ctrl+b/f/p/n", "motion", Tier::Plain),
        row("ctrl+←/→", "word motion", Tier::Ext),
        row("ctrl+w/u/k", "kill word / to line start / end", Tier::Plain),
        row("ctrl+d", "delete forward", Tier::Plain),
        row("ctrl+y", "yank the last kill", Tier::Plain),
        row("?", "this help (empty draft)", Tier::Plain),
    ]);
    let asks = vec![
        row("1-9", "select (never submits)", Tier::Plain),
        row("↑/↓", "move selection", Tier::Plain),
        row("space", "toggle (multi-select)", Tier::Plain),
        row("tab/shift+tab ←/→", "cycle question tabs", Tier::Plain),
        row("enter", "confirm / advance / submit", Tier::Plain),
        row("f", "full diff / plan in the reader", Tier::Plain),
    ];
    let reader = vec![
        row("↑/↓ j/k pgup/pgdn", "scroll", Tier::Plain),
        row("home/end · g/G", "top / bottom", Tier::Plain),
        row("←/→", "step between accepted plans", Tier::Plain),
        row("q", "close", Tier::Plain),
    ];
    let readonly = vec![
        row("↑/↓ j/k pgup/pgdn g/G", "scroll", Tier::Plain),
        row("f", "read the diff / plan", Tier::Plain),
        row("q", "back to fleet", Tier::Plain),
    ];
    vec![
        Section {
            title: "chat",
            bindings: chat,
        },
        Section {
            title: "composer",
            bindings: composer,
        },
        Section {
            title: "ask panels",
            bindings: asks,
        },
        Section {
            title: "reader",
            bindings: reader,
        },
        Section {
            title: "read-only chat",
            bindings: readonly,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(kitty: bool) -> Effective {
        Effective {
            kitty,
            leader_label: "C-b".to_string(),
        }
    }

    /// Kitty-tier rows exist iff the probe succeeded — hints and the
    /// overlay can never advertise a chord the terminal cannot deliver
    /// (P10), and a kitty terminal sees the sugar.
    #[test]
    fn kitty_rows_are_hidden_without_the_probe_and_shown_with_it() {
        for sections in [
            fleet_sections(&eff(false), OpenMode::RawAttach),
            chat_sections(&eff(false)),
        ] {
            assert!(
                sections
                    .iter()
                    .flat_map(|section| &section.bindings)
                    .all(|binding| binding.tier != Tier::Kitty),
                "no kitty rows without the probe"
            );
        }
        let fleet = fleet_sections(&eff(true), OpenMode::RawAttach);
        assert!(
            fleet
                .iter()
                .flat_map(|section| &section.bindings)
                .any(|binding| binding.keys == "ctrl+enter" && binding.tier == Tier::Kitty)
        );
        let chat = chat_sections(&eff(true));
        assert!(
            chat.iter()
                .flat_map(|section| &section.bindings)
                .any(|binding| binding.keys == "shift+enter" && binding.tier == Tier::Kitty)
        );
    }

    /// The Enter row names the CONFIGURED default; `o` (and ctrl+enter)
    /// name the other mode — the table derives, never hardcodes (A1).
    #[test]
    fn entry_rows_name_the_effective_modes() {
        let sections = fleet_sections(&eff(true), OpenMode::Chat);
        let fleet = &sections[0].bindings;
        let enter = fleet.iter().find(|b| b.keys == "enter").expect("enter row");
        assert_eq!(enter.action, "open in chat");
        let o = fleet.iter().find(|b| b.keys == "o").expect("o row");
        assert_eq!(o.action, "open in raw attach");
    }

    /// The configured leader substitutes into every chord row (P10).
    #[test]
    fn the_leader_label_substitutes_into_chords() {
        let sections = chat_sections(&eff(false));
        let chat = &sections[0].bindings;
        assert!(chat.iter().any(|b| b.keys == "C-b s"));
        assert!(chat.iter().any(|b| b.keys == "C-b d"));
    }

    /// Ext-tier rows exist regardless of the probe (standard CSI), and
    /// carry the tier so the overlay can mark them terminal-dependent.
    #[test]
    fn ext_rows_carry_their_tier() {
        let sections = chat_sections(&eff(false));
        let ext: Vec<_> = sections
            .iter()
            .flat_map(|section| &section.bindings)
            .filter(|binding| binding.tier == Tier::Ext)
            .collect();
        assert!(ext.iter().any(|b| b.keys == "ctrl+home/end"));
        assert!(ext.iter().any(|b| b.keys == "ctrl+←/→"));
    }
}

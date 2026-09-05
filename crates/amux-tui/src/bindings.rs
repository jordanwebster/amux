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

    /// Move the block focus older or newer.
    pub fn focus_chord(&self) -> String {
        format!("{} k/j", self.leader_label)
    }

    /// Copy the focused block — the newest one when nothing is focused.
    pub fn copy_chord(&self) -> String {
        format!("{} y", self.leader_label)
    }

    /// Open or shut the focused folded run.
    pub fn fold_chord(&self) -> String {
        format!("{} o", self.leader_label)
    }

    /// What the feed offers a reader who has stopped following: the two
    /// acts that need a block under the focus bar. Written here so the
    /// footer cannot name a chord the overlay does not list.
    pub fn feed_hint(&self) -> String {
        format!("{} focus · {} copy", self.focus_chord(), self.copy_chord())
    }

    /// What a folded run says will open it, and what an open one says
    /// will shut it again.
    pub fn fold_hint(&self, expanded: bool) -> String {
        format!(
            "{} {}",
            self.fold_chord(),
            if expanded { "close" } else { "expand" }
        )
    }
}

/// The rule the frame draws under a feed that is no longer following.
/// Every chord it names is leaderless, so unlike the hints it needs no
/// `Effective` — but it lives beside the table it must agree with.
pub const PAUSED_RULE: &str =
    "↓ scrolled back · wheel or pgdn to catch up · ctrl+end for the newest";

/// Esc walks visible chat state back by one layer. A native overlay closes
/// before shared block focus; without one, the native ask and feed stages
/// follow focus clearing.
const ESC_BACK_ACTION: &str = "close reader / clear focus / back ask / follow";

/// The screen facts the family chords depend on. Each of the three
/// exists only where it would do something: `<leader> n` needs somewhere
/// else in the family to go, `<leader> m` needs a completion with a body
/// behind it, `<leader> a` needs a child's ask this chat could host.
/// Hints never advertise a dead key (P10), and an overlay is a hint.
#[derive(Clone, Copy, Debug, Default)]
pub struct FamilyKeys {
    pub cycle: bool,
    pub reports: bool,
    pub answer: bool,
}

/// The family chords, identical in both native chats because they are
/// the same act in both — written once so they cannot drift apart.
fn family_rows(eff: &Effective, family: FamilyKeys) -> Vec<Binding> {
    let mut rows = Vec::new();
    if family.cycle {
        rows.push(row(
            format!("{} n", eff.leader_label),
            "next agent in this family",
            Tier::Plain,
        ));
    }
    if family.reports {
        rows.push(row(
            format!("{} m", eff.leader_label),
            "open / close completions",
            Tier::Plain,
        ));
    }
    if family.answer {
        rows.push(row(
            format!("{} a", eff.leader_label),
            "answer the waiting child here",
            Tier::Plain,
        ));
    }
    rows
}

/// Shared feed rows: how the reader moves through the history and takes
/// something out of it. `shift+drag` is listed although amux binds
/// nothing — mouse capture would otherwise look as though it had taken
/// selection away, and the reader needs to know their terminal still has
/// it.
fn feed_focus_rows(eff: &Effective) -> Vec<Binding> {
    vec![
        row("wheel", "scroll the feed", Tier::Plain),
        row(
            "shift+drag",
            "select text (your terminal's own)",
            Tier::Plain,
        ),
        row(
            eff.copy_chord(),
            "copy the focused block (the newest if none)",
            Tier::Plain,
        ),
        row(eff.focus_chord(), "focus older / newer block", Tier::Plain),
        row("ctrl+↑/↓", "focus older / newer block", Tier::Ext),
        row(
            eff.fold_chord(),
            "open / close the focused run",
            Tier::Plain,
        ),
        row("esc", ESC_BACK_ACTION, Tier::Plain),
    ]
}

/// The capture key's row, present only where the key itself is. The
/// binding is a debug-build affordance — a release binary has nothing to
/// capture into a report — and every list of keys derives from this one
/// answer so no overlay can offer a key that does nothing.
pub(crate) fn report_key_row() -> Option<Binding> {
    report_key_row_for(cfg!(debug_assertions))
}

/// The gate itself, so both answers are reachable from a test binary
/// (which is always built with debug assertions on).
pub(crate) fn report_key_row_for(debug_build: bool) -> Option<Binding> {
    debug_build.then(|| row("C-g", "capture a report", Tier::Plain))
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
pub fn fleet_sections(
    eff: &Effective,
    default_open_mode: OpenMode,
    families: bool,
) -> Vec<Section> {
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
    ]);
    // Nothing on this fleet has children, so nothing folds.
    if families {
        fleet.push(row("z", "open/shut a family", Tier::Plain));
    }
    fleet.extend([
        row("r", "rename selected", Tier::Plain),
        row("d", "delete selected", Tier::Plain),
        row("q", "quit", Tier::Plain),
        row("ctrl+c ctrl+c", "quit (guarded: two presses)", Tier::Plain),
    ]);
    fleet.extend(report_key_row());
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
pub fn chat_sections(eff: &Effective, family: FamilyKeys) -> Vec<Section> {
    let mut chat = vec![
        row("ctrl+x", "interrupt the agent", Tier::Plain),
        row("pgup/pgdn", "scroll the feed", Tier::Plain),
        row("ctrl+home/end", "feed oldest / newest", Tier::Ext),
    ];
    // The feed rows sit with the scroll rows they belong to, not at the
    // end of the section, because they are all one act to the reader.
    chat.extend(feed_focus_rows(eff));
    chat.extend([
        row("ctrl+t", "read accepted plans (←/→ steps)", Tier::Plain),
        row(
            format!("{} r", eff.leader_label),
            "review the agent's diff (resumes a draft one)",
            Tier::Plain,
        ),
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
    ]);
    chat.extend(family_rows(eff, family));
    chat.extend(report_key_row());
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
        row("ctrl+v", "attach clipboard image or file", Tier::Plain),
        row("?", "this help (empty draft)", Tier::Plain),
    ]);
    let asks = vec![
        row("1-9", "select (never submits)", Tier::Plain),
        row("↑/↓", "move selection", Tier::Plain),
        row("space", "toggle (multi-select)", Tier::Plain),
        row("tab/shift+tab ←/→", "cycle question tabs", Tier::Plain),
        row("enter", "confirm / advance / submit", Tier::Plain),
        row("f", "open document in the reader", Tier::Plain),
    ];
    let reader = vec![
        row("↑/↓ j/k pgup/pgdn", "scroll", Tier::Plain),
        row("home/end · g/G", "top / bottom", Tier::Plain),
        row("←/→", "step between accepted plans", Tier::Plain),
        row("q", "close", Tier::Plain),
    ];
    let readonly = vec![
        row("↑/↓ j/k pgup/pgdn g/G", "scroll", Tier::Plain),
        row("f", "read document", Tier::Plain),
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

/// Claude SDK chat bindings: the shared Claude chat rows plus the one
/// key only a session driven over stream-JSON has. The breakdown costs a
/// round trip to the provider, so the row appears only where the session
/// could answer it — a hint never names a dead key.
pub fn claude_sdk_chat_sections(
    eff: &Effective,
    family: FamilyKeys,
    context_breakdown: bool,
) -> Vec<Section> {
    let mut sections = chat_sections(eff, family);
    if context_breakdown && let Some(chat) = sections.first_mut() {
        chat.bindings.push(row(
            format!("{} c", eff.leader_label),
            "context breakdown (again refreshes)",
            Tier::Plain,
        ));
    }
    sections
}

/// Codex chat bindings. Chrome, composer, and pager conventions are shared;
/// approval and steering rows name Codex-native actions instead of borrowing
/// Claude's question/plan vocabulary.
pub fn codex_chat_sections(eff: &Effective, family: FamilyKeys) -> Vec<Section> {
    let mut chat = vec![
        row("ctrl+x", "interrupt the active turn", Tier::Plain),
        row("pgup/pgdn", "scroll the feed", Tier::Plain),
        row("ctrl+home/end", "feed oldest / newest", Tier::Ext),
    ];
    chat.extend(feed_focus_rows(eff));
    chat.extend([
        row(
            "ctrl+c",
            "clear draft; empty: press twice to quit",
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
    ]);
    chat.extend(family_rows(eff, family));
    chat.extend(report_key_row());
    let mut composer = vec![
        row("enter", "send or steer", Tier::Plain),
        row("ctrl+j", "newline", Tier::Plain),
    ];
    if eff.kitty {
        composer.push(row("shift+enter", "newline", Tier::Kitty));
    }
    composer.extend([
        row("home/end · ctrl+b/f/p/n", "motion", Tier::Plain),
        row("ctrl+←/→", "word motion", Tier::Ext),
        row("ctrl+w/u/k", "kill word / line", Tier::Plain),
        row("ctrl+d / ctrl+y", "delete forward / yank", Tier::Plain),
        row("ctrl+v", "attach clipboard image or file", Tier::Plain),
        row("?", "this help (empty draft)", Tier::Plain),
    ]);
    let approvals = vec![
        row("1-9 or ↑/↓", "select offered decision", Tier::Plain),
        row("enter", "confirm enabled decision", Tier::Plain),
        row("ctrl+x", "interrupt instead", Tier::Plain),
    ];
    let readonly = vec![
        row("↑/↓ j/k pgup/pgdn g/G", "scroll", Tier::Plain),
        row("q", "back to fleet", Tier::Plain),
    ];
    vec![
        Section {
            title: "codex chat",
            bindings: chat,
        },
        Section {
            title: "composer",
            bindings: composer,
        },
        Section {
            title: "approvals",
            bindings: approvals,
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

    /// Every family chord live, so the tests below see the whole table.
    fn all_family() -> FamilyKeys {
        FamilyKeys {
            cycle: true,
            reports: true,
            answer: true,
        }
    }

    fn eff(kitty: bool) -> Effective {
        Effective {
            kitty,
            leader_label: "C-b".to_string(),
        }
    }

    #[test]
    fn both_chat_tables_state_escs_back_one_stage_behavior() {
        for sections in [
            chat_sections(&eff(false), all_family()),
            codex_chat_sections(&eff(false), all_family()),
        ] {
            let esc = sections
                .iter()
                .flat_map(|section| &section.bindings)
                .find(|binding| binding.keys == "esc")
                .expect("chat bindings carry Esc");
            assert_eq!(esc.action, ESC_BACK_ACTION);
            for meaning in ["close reader", "clear focus", "back ask", "follow"] {
                assert!(
                    esc.action.contains(meaning),
                    "Esc row omits {meaning:?}: {}",
                    esc.action
                );
            }
        }
    }

    /// Kitty-tier rows exist iff the probe succeeded — hints and the
    /// overlay can never advertise a chord the terminal cannot deliver
    /// (P10), and a kitty terminal sees the sugar.
    #[test]
    fn kitty_rows_are_hidden_without_the_probe_and_shown_with_it() {
        for sections in [
            fleet_sections(&eff(false), OpenMode::RawAttach, true),
            chat_sections(&eff(false), all_family()),
        ] {
            assert!(
                sections
                    .iter()
                    .flat_map(|section| &section.bindings)
                    .all(|binding| binding.tier != Tier::Kitty),
                "no kitty rows without the probe"
            );
        }
        let fleet = fleet_sections(&eff(true), OpenMode::RawAttach, true);
        assert!(
            fleet
                .iter()
                .flat_map(|section| &section.bindings)
                .any(|binding| binding.keys == "ctrl+enter" && binding.tier == Tier::Kitty)
        );
        let chat = chat_sections(&eff(true), all_family());
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
        let sections = fleet_sections(&eff(true), OpenMode::Chat, true);
        let fleet = &sections[0].bindings;
        let enter = fleet.iter().find(|b| b.keys == "enter").expect("enter row");
        assert_eq!(enter.action, "open in chat");
        let o = fleet.iter().find(|b| b.keys == "o").expect("o row");
        assert_eq!(o.action, "open in raw attach");
    }

    /// The configured leader substitutes into every chord row (P10).
    #[test]
    fn the_leader_label_substitutes_into_chords() {
        let sections = chat_sections(&eff(false), all_family());
        let chat = &sections[0].bindings;
        assert!(chat.iter().any(|b| b.keys == "C-b s"));
        assert!(chat.iter().any(|b| b.keys == "C-b d"));
        assert!(chat.iter().any(|b| b.keys == "C-b r"), "the review chord");
    }

    /// Reviewing the agent's diff is a Claude-chat act; the Codex chat has
    /// no page to open yet, so its table must not offer the chord.
    #[test]
    fn only_the_claude_chat_lists_the_review_chord() {
        let claude = chat_sections(&eff(false), FamilyKeys::default());
        let review = claude[0]
            .bindings
            .iter()
            .find(|binding| binding.keys == "C-b r")
            .expect("the review chord");
        assert!(review.action.contains("review"));
        assert_eq!(review.tier, Tier::Plain);
        let codex = codex_chat_sections(&eff(false), FamilyKeys::default());
        assert!(!codex[0].bindings.iter().any(|b| b.keys == "C-b r"));
    }

    /// Everything the feed offers a reader is listed in both chats:
    /// the wheel, the terminal's own selection, copy, focus motion in
    /// both tiers, the fold and the way out of focus.
    #[test]
    fn feed_bindings_are_identical_in_both_chats() {
        for sections in [
            chat_sections(&eff(false), FamilyKeys::default()),
            codex_chat_sections(&eff(false), FamilyKeys::default()),
        ] {
            let chat = &sections[0].bindings;
            let plain = |keys: &str| {
                chat.iter()
                    .any(|binding| binding.keys == keys && binding.tier == Tier::Plain)
            };
            assert!(plain("wheel"));
            assert!(plain("shift+drag"));
            assert!(plain("C-b y"));
            assert!(plain("C-b k/j"));
            assert!(
                chat.iter()
                    .any(|binding| { binding.keys == "ctrl+↑/↓" && binding.tier == Tier::Ext })
            );
            assert!(plain("C-b o"));
            assert!(plain("esc"));
        }
    }

    /// A folded run offers to open; the same run, open, offers to shut.
    /// The hint follows the state so it never invites a second expand.
    #[test]
    fn the_fold_hint_names_the_act_that_is_available() {
        assert_eq!(eff(false).fold_hint(false), "C-b o expand");
        assert_eq!(eff(false).fold_hint(true), "C-b o close");
    }

    /// The family chords are listed exactly where they would do
    /// something. A chat with no family below it, no completion to open
    /// and no child waiting has none of the three rows; the same table
    /// with all three facts true has all three.
    #[test]
    fn a2a_bindings_list_a_family_chord_only_where_it_works() {
        for sections in [
            chat_sections(&eff(false), FamilyKeys::default()),
            codex_chat_sections(&eff(false), FamilyKeys::default()),
        ] {
            let keys: Vec<&str> = sections
                .iter()
                .flat_map(|section| &section.bindings)
                .map(|binding| binding.keys.as_str())
                .collect();
            for chord in ["C-b n", "C-b m", "C-b a"] {
                assert!(!keys.contains(&chord), "{chord} is inert here: {keys:?}");
            }
        }
        for sections in [
            chat_sections(&eff(false), all_family()),
            codex_chat_sections(&eff(false), all_family()),
        ] {
            let keys: Vec<&str> = sections
                .iter()
                .flat_map(|section| &section.bindings)
                .map(|binding| binding.keys.as_str())
                .collect();
            for chord in ["C-b n", "C-b m", "C-b a"] {
                assert!(keys.contains(&chord), "{chord} works here: {keys:?}");
            }
        }
    }

    /// One chord at a time: each fact turns on its own row and nobody
    /// else's, so a chat that can only cycle does not offer to answer.
    #[test]
    fn a2a_bindings_gate_each_family_chord_on_its_own_fact() {
        let cases = [
            (
                FamilyKeys {
                    cycle: true,
                    ..FamilyKeys::default()
                },
                "C-b n",
            ),
            (
                FamilyKeys {
                    reports: true,
                    ..FamilyKeys::default()
                },
                "C-b m",
            ),
            (
                FamilyKeys {
                    answer: true,
                    ..FamilyKeys::default()
                },
                "C-b a",
            ),
        ];
        for (family, expected) in cases {
            let sections = chat_sections(&eff(false), family);
            let chords: Vec<&str> = sections
                .iter()
                .flat_map(|section| &section.bindings)
                .map(|binding| binding.keys.as_str())
                .filter(|keys| ["C-b n", "C-b m", "C-b a"].contains(keys))
                .collect();
            assert_eq!(chords, vec![expected]);
        }
    }

    /// The two chats are the same chrome, so they name the same chords in
    /// the same words — the Codex overlay used to be missing them
    /// outright, which taught the human that its chat had no family keys.
    #[test]
    fn a2a_bindings_read_the_same_in_both_chats() {
        let family_rows = |sections: Vec<Section>| -> Vec<(String, String)> {
            sections
                .iter()
                .flat_map(|section| &section.bindings)
                .filter(|binding| ["C-b n", "C-b m", "C-b a"].contains(&binding.keys.as_str()))
                .map(|binding| (binding.keys.clone(), binding.action.clone()))
                .collect()
        };
        assert_eq!(
            family_rows(chat_sections(&eff(false), all_family())),
            family_rows(codex_chat_sections(&eff(false), all_family())),
        );
    }

    /// `z` is a fleet key, and a fleet with nobody's children in it has
    /// nothing to fold.
    #[test]
    fn a2a_bindings_list_the_fold_key_only_with_a_family_on_screen() {
        let has = |families: bool| {
            fleet_sections(&eff(false), OpenMode::RawAttach, families)
                .iter()
                .flat_map(|section| &section.bindings)
                .any(|binding| binding.keys == "z")
        };
        assert!(has(true));
        assert!(!has(false));
    }

    /// Ext-tier rows exist regardless of the probe (standard CSI), and
    /// carry the tier so the overlay can mark them terminal-dependent.
    #[test]
    fn ext_rows_carry_their_tier() {
        let sections = chat_sections(&eff(false), all_family());
        let ext: Vec<_> = sections
            .iter()
            .flat_map(|section| &section.bindings)
            .filter(|binding| binding.tier == Tier::Ext)
            .collect();
        assert!(ext.iter().any(|b| b.keys == "ctrl+home/end"));
        assert!(ext.iter().any(|b| b.keys == "ctrl+←/→"));
    }
}

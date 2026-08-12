//! Renderer-local state and key handling for the fleet screen.
//!
//! ViewState is exactly what `docs/UI.md` allows a renderer to keep: focus,
//! scroll, drafts, navigation. Everything domain-shaped stays in the Model.

use amux_ui::{AgentId, Command, FleetItem, Model};
use chrono::{DateTime, TimeDelta, Utc};

/// The chrome-wide guarded Ctrl+C (`docs/CHAT.md` §Keybindings) — ONE
/// rule for the whole TUI: with a focused non-empty text field the press
/// clears that field (and never arms); otherwise the first press arms
/// this guard — the footer hint line becomes `press ctrl+c again to
/// quit` in warning color — and a second press within the window quits.
/// Any other key or the timeout disarms. The invariant, teachable in one
/// line: a single Ctrl+C never quits, never interrupts, and never loses
/// text it didn't visibly kill.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuitGuard {
    armed_at: Option<DateTime<Utc>>,
}

impl QuitGuard {
    /// The arm window: long enough to read the message, short enough that
    /// no armed state lurks (keybindings derivation §5.5, accepted).
    pub const WINDOW_SECS: i64 = 3;

    fn window() -> TimeDelta {
        TimeDelta::seconds(Self::WINDOW_SECS)
    }

    /// A Ctrl+C press with nothing to clear: quits when armed and fresh,
    /// arms otherwise. A stale arm re-arms instead of quitting — the
    /// rendered message may never have been read.
    pub fn press(&mut self, now: DateTime<Utc>) -> bool {
        match self.armed_at {
            Some(at) if now - at <= Self::window() => {
                self.armed_at = None;
                true
            }
            _ => {
                self.armed_at = Some(now);
                false
            }
        }
    }

    /// A Ctrl+C press that cleared a focused field: the clearing press
    /// never arms (and never quits) — the branch is taken per press by
    /// buffer state.
    pub fn note_clear(&mut self) {
        self.armed_at = None;
    }

    /// Any key that is not Ctrl+C disarms.
    pub fn disarm(&mut self) {
        self.armed_at = None;
    }

    /// Whether the armed footer renders. Expiry is the tick's job
    /// ([`QuitGuard::expire`]); a render between the deadline and the
    /// next tick may show the message up to a second late — tolerated.
    pub fn is_armed(&self) -> bool {
        self.armed_at.is_some()
    }

    /// The tick check, gated on being armed (the event-driven rule's
    /// extension): disarms a stale arm; true when state changed and a
    /// repaint is owed.
    pub fn expire(&mut self, now: DateTime<Utc>) -> bool {
        match self.armed_at {
            Some(at) if now - at > Self::window() => {
                self.armed_at = None;
                true
            }
            _ => false,
        }
    }
}

/// Interaction mode of the fleet screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Navigation (`j/k`, `gg/G`); the filter line is dormant.
    Normal,
    /// Insert mode: typing narrows the fleet.
    Filter,
    /// Inline rename of the selected agent.
    Rename { agent: AgentId, draft: String },
    /// Delete confirmation living in the status line.
    ConfirmDelete { agent: AgentId, name: String },
    /// Key help overlay.
    Help,
}

/// Renderer-local state. Never serialized, never authoritative.
#[derive(Clone, Debug)]
pub struct ViewState {
    pub mode: Mode,
    pub filter: String,
    /// Selection index into the *visible* (filtered) rows.
    pub selected: usize,
    pub scroll: usize,
    /// `gg` chord progress.
    pub pending_g: bool,
    /// Status-line op failures with `seq <= dismissed` stay hidden
    /// (dismissal is view state; the Model only reports).
    pub dismissed_error_seq: u64,
    /// Transient view-side notice (e.g. refusing to attach to an offline
    /// host); cleared on the next keypress.
    pub notice: Option<String>,
    /// Human label of the leader key ("C-a"), shown in help. View-config,
    /// set once at startup.
    pub leader_label: String,
    /// Whether the terminal answered the kitty keyboard-enhancement probe
    /// (view-config, set when the chrome session enters). Gates the
    /// kitty-tier bindings in hints and the `?` overlay — hints advertise
    /// only what works; dispatch trusts delivered events.
    pub kitty: bool,
    /// The chrome-wide two-press quit guard (fleet side; an open chat
    /// carries its own instance — same type, same rule).
    pub quit_guard: QuitGuard,
    /// The chat screen, when open: it replaces the fleet inside the same
    /// chrome (`docs/CHAT.md`). Opening from the fleet is Phase 6's
    /// binding work; [`ViewState::open_chat`] is the seam it invokes.
    pub chat: Option<crate::chat::ChatView>,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            filter: String::new(),
            selected: 0,
            scroll: 0,
            pending_g: false,
            dismissed_error_seq: 0,
            notice: None,
            leader_label: "C-a".to_string(),
            kitty: false,
            quit_guard: QuitGuard::default(),
            chat: None,
        }
    }
}

impl ViewState {
    /// Enter the chat screen for an agent — the entry seam the Phase 6
    /// fleet bindings (Enter/Ctrl+Enter/`o` per A1) will invoke.
    pub fn open_chat(&mut self, agent: AgentId) {
        self.chat = Some(crate::chat::ChatView::open(agent));
    }

    /// Leave the chat screen back to the fleet (chrome navigation — a
    /// pending ask survives leaving; reopening re-derives everything from
    /// the Model).
    pub fn close_chat(&mut self) {
        self.chat = None;
    }
}

/// One visible row after filtering.
pub enum VisibleRow<'a> {
    Agent(&'a amux_ui::AgentCard),
    PendingCreate {
        name: &'a str,
        agent_type: &'a amux_ui::AgentType,
        host: Option<amux_ui::HostId>,
    },
}

impl VisibleRow<'_> {
    pub fn display_name(&self) -> String {
        match self {
            VisibleRow::Agent(card) => card.display_name(),
            VisibleRow::PendingCreate { name, .. } => (*name).to_string(),
        }
    }
}

/// The fleet after the view's filter: ranking comes from the Model, the
/// filter is presentation.
pub fn visible_rows<'a>(model: &'a Model, view: &ViewState) -> Vec<VisibleRow<'a>> {
    model
        .fleet()
        .into_iter()
        .filter_map(|item| {
            let row = match item {
                FleetItem::Agent(card) => VisibleRow::Agent(card),
                FleetItem::PendingCreate {
                    name,
                    agent_type,
                    host,
                    ..
                } => VisibleRow::PendingCreate {
                    name,
                    agent_type,
                    host,
                },
            };
            fuzzy_matches(&view.filter, &row.display_name()).then_some(row)
        })
        .collect()
}

/// Case-insensitive subsequence match — cheap, predictable fuzzy.
pub fn fuzzy_matches(filter: &str, candidate: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let candidate = candidate.to_lowercase();
    let mut chars = candidate.chars();
    'outer: for wanted in filter.to_lowercase().chars() {
        for have in chars.by_ref() {
            if have == wanted {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// Generated create name: `claude-N`, first N not taken by a current
/// display name or pending create. Deterministic from the Model.
pub fn next_agent_name(model: &Model) -> String {
    let taken: Vec<String> = model
        .fleet()
        .iter()
        .map(|item| match item {
            FleetItem::Agent(card) => card.display_name(),
            FleetItem::PendingCreate { name, .. } => (*name).to_string(),
        })
        .collect();
    (1..)
        .map(|n| format!("claude-{n}"))
        .find(|candidate| !taken.iter().any(|name| name == candidate))
        .expect("unbounded name space")
}

/// What a keypress asks the shell to do.
#[derive(Debug, PartialEq)]
pub enum UiAction {
    Quit,
    Attach(AgentId),
    Dispatch(Command),
    /// Leave the chat back to the fleet (read-only chats' `q`, F1; the
    /// writable chat leaves via the chrome leader — Phase 6).
    CloseChat,
    /// Create on this host (name and working dir are filled in by the
    /// runtime edge, which owns id/name generation).
    Create {
        host: Option<amux_ui::HostId>,
    },
    /// Dump the recorder ring (`C-g`).
    DebugDump,
}

#[cfg(test)]
mod quit_guard_tests {
    use super::*;

    fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_755_000_000 + seconds, 0).expect("epoch")
    }

    #[test]
    fn the_first_press_arms_and_a_fresh_second_press_quits() {
        let mut guard = QuitGuard::default();
        assert!(!guard.press(t(0)), "a single Ctrl+C never quits");
        assert!(guard.is_armed());
        assert!(guard.press(t(2)), "second press within the window quits");
        assert!(!guard.is_armed(), "the quitting press consumes the arm");
    }

    #[test]
    fn a_stale_arm_rearms_instead_of_quitting() {
        let mut guard = QuitGuard::default();
        guard.press(t(0));
        assert!(
            !guard.press(t(QuitGuard::WINDOW_SECS + 1)),
            "past the window the press arms again"
        );
        assert!(guard.is_armed());
    }

    #[test]
    fn any_other_key_and_the_clearing_press_disarm() {
        let mut guard = QuitGuard::default();
        guard.press(t(0));
        guard.disarm();
        assert!(!guard.is_armed());
        guard.press(t(1));
        guard.note_clear();
        assert!(!guard.is_armed(), "the clearing press never arms");
    }

    #[test]
    fn the_tick_expires_a_stale_arm_exactly_once() {
        let mut guard = QuitGuard::default();
        assert!(!guard.expire(t(0)), "nothing to expire while disarmed");
        guard.press(t(0));
        assert!(!guard.expire(t(QuitGuard::WINDOW_SECS)), "still fresh");
        assert!(guard.expire(t(QuitGuard::WINDOW_SECS + 1)), "stale: disarm");
        assert!(!guard.is_armed());
        assert!(!guard.expire(t(10)), "already disarmed: no repaint owed");
    }
}

impl ViewState {
    pub fn clamp_selection(&mut self, visible: usize, list_rows: usize) {
        if visible == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = self.selected.min(visible - 1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if list_rows > 0 && self.selected >= self.scroll + list_rows {
            self.scroll = self.selected + 1 - list_rows;
        }
    }
}

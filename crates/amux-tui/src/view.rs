//! Renderer-local state and key handling for the fleet screen.
//!
//! ViewState is exactly what `docs/UI.md` allows a renderer to keep: focus,
//! scroll, drafts, navigation. Everything domain-shaped stays in the Model.

use std::collections::BTreeSet;

use amux_ui::{AgentId, Attention, Command, FleetItem, Model};
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

    /// The armed footer text — every surface (fleet status line, chat
    /// footer, panel hints, reader tail) renders this one message in
    /// warning color while the guard is armed.
    pub(crate) const HINT: &'static str = "press ctrl+c again to quit";

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

/// Which mode an entry key opens a Claude agent in (`docs/CHAT.md` A1):
/// raw attach (the byte passthrough) or the structured chat. The default
/// comes from the amux config (`ui.default_open_mode`, shipped `raw`);
/// the non-default mode opens via Ctrl+Enter (kitty tier) or `o`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenMode {
    #[default]
    RawAttach,
    Chat,
}

impl OpenMode {
    /// The non-default mode ("open in the other mode").
    pub fn other(self) -> Self {
        match self {
            OpenMode::RawAttach => OpenMode::Chat,
            OpenMode::Chat => OpenMode::RawAttach,
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
    /// The configured leader character (`a` for ctrl+a). View-config, set
    /// once at startup; the chat's leader chords compose against it and
    /// the help overlays derive their `C-<leader>` labels from it.
    pub leader: char,
    /// The mode the fleet's Enter opens (A1); view-config from the amux
    /// config's `ui.default_open_mode`.
    pub default_open_mode: OpenMode,
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
    /// Which families are unfolded, by the id of the agent heading them.
    /// Navigation state, so it lives here: a family the Model no longer
    /// reports simply stops being consulted, and nothing has to be
    /// cleaned up when one disappears.
    pub expanded: BTreeSet<AgentId>,
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
            leader: 'a',
            default_open_mode: OpenMode::default(),
            kitty: false,
            quit_guard: QuitGuard::default(),
            chat: None,
            expanded: BTreeSet::new(),
        }
    }
}

impl ViewState {
    /// Enter the chat screen for an agent — invoked by the fleet's entry
    /// bindings (Enter/Ctrl+Enter/`o` per A1) through
    /// [`UiAction::OpenChat`]; the run loop notes the attach so the
    /// subscription policy widens.
    pub fn open_chat(&mut self, model: &Model, agent: AgentId) {
        self.chat = crate::chat::ChatView::open(model, agent, self.leader, self.kitty);
    }

    /// Leave the chat screen back to the fleet (chrome navigation — a
    /// pending ask survives leaving; reopening re-derives everything from
    /// the Model).
    pub fn close_chat(&mut self) {
        self.chat = None;
    }

    /// Open or shut the family the given row belongs to. Shutting one from
    /// a row that is about to disappear leaves the selection on the row
    /// that swallowed it — the top row — so the cursor never lands
    /// somewhere the keypress did not point at.
    pub fn toggle_fold(&mut self, model: &Model, row: usize) -> bool {
        let rows = visible_rows(model, self);
        let Some(VisibleRow::Agent(agent)) = rows.get(row) else {
            return false;
        };
        let Some(family) = agent.family else {
            return false;
        };
        if !self.expanded.remove(&family) {
            self.expanded.insert(family);
            return true;
        }
        // The family is shut now: everything below its top row is gone.
        self.selected = visible_rows(model, self)
            .iter()
            .position(|row| row.card().is_some_and(|card| card.agent.id == family))
            .unwrap_or(self.selected);
        true
    }
}

/// What a folded family row stands in for: the agents it hides and the
/// loudest attention among them, so one glance at a shut family says
/// whether anything inside wants a person.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Folded {
    pub hidden: usize,
    pub attention: Attention,
}

/// One agent row of the fleet list, placed in its family.
pub struct AgentRow<'a> {
    pub card: &'a amux_ui::AgentCard,
    /// The agent heading this row's family, when the row is in one at all
    /// — its own id on a family's top row. `None` for an agent that has
    /// neither a parent nor children: there is nothing to fold.
    pub family: Option<AgentId>,
    /// Generations below the family's top row; 0 on the top row itself.
    pub depth: usize,
    /// Set while this row's family is folded shut.
    pub folded: Option<Folded>,
}

/// One visible row after filtering.
pub enum VisibleRow<'a> {
    Agent(AgentRow<'a>),
    PendingCreate {
        name: &'a str,
        agent_type: &'a amux_ui::AgentType,
        host: Option<amux_ui::HostId>,
    },
}

impl VisibleRow<'_> {
    /// The name the filter matches and the row leads with — the plain
    /// agent name, never the folded row's `⋯N` marker, so typing a name
    /// finds the agent whether or not its family is shut.
    pub fn display_name(&self) -> String {
        match self {
            VisibleRow::Agent(row) => row.card.display_name(),
            VisibleRow::PendingCreate { name, .. } => (*name).to_string(),
        }
    }

    pub fn card(&self) -> Option<&amux_ui::AgentCard> {
        match self {
            VisibleRow::Agent(row) => Some(row.card),
            VisibleRow::PendingCreate { .. } => None,
        }
    }
}

/// The fleet after the view's filter: ranking and family structure come
/// from the Model, the fold and the filter are presentation.
pub fn visible_rows<'a>(model: &'a Model, view: &ViewState) -> Vec<VisibleRow<'a>> {
    model
        .fleet()
        .into_iter()
        .flat_map(|item| fleet_item_rows(item, view))
        .filter(|row| fuzzy_matches(&view.filter, &row.display_name()))
        .collect()
}

/// The rows one ranked fleet item contributes. A family is one row while
/// it is folded and its parent plus every descendant while it is open —
/// and a filter opens every family, because a name typed into the filter
/// must never miss an agent hiding behind a fold.
fn fleet_item_rows<'a>(item: FleetItem<'a>, view: &ViewState) -> Vec<VisibleRow<'a>> {
    match item {
        FleetItem::Agent(card) => vec![VisibleRow::Agent(AgentRow {
            card,
            family: None,
            depth: 0,
            folded: None,
        })],
        FleetItem::Family {
            parent,
            children,
            child_count,
            highest_attention,
        } => {
            let top = parent.agent.id;
            if !view.filter.is_empty() || view.expanded.contains(&top) {
                std::iter::once(VisibleRow::Agent(AgentRow {
                    card: parent,
                    family: Some(top),
                    depth: 0,
                    folded: None,
                }))
                .chain(children.into_iter().map(|member| {
                    VisibleRow::Agent(AgentRow {
                        card: member.card,
                        family: Some(top),
                        depth: member.depth,
                        folded: None,
                    })
                }))
                .collect()
            } else {
                vec![VisibleRow::Agent(AgentRow {
                    card: parent,
                    family: Some(top),
                    depth: 0,
                    folded: Some(Folded {
                        hidden: child_count,
                        attention: highest_attention,
                    }),
                })]
            }
        }
        FleetItem::PendingCreate {
            name,
            agent_type,
            host,
            ..
        } => vec![VisibleRow::PendingCreate {
            name,
            agent_type,
            host,
        }],
    }
}

/// Every agent name the fleet holds, fold or no fold — the name generator
/// must not hand out a name an unfolded family is already using.
fn all_agent_names(model: &Model) -> Vec<String> {
    model
        .fleet()
        .into_iter()
        .flat_map(|item| match item {
            FleetItem::Agent(card) => vec![card.display_name()],
            FleetItem::Family {
                parent, children, ..
            } => std::iter::once(parent.display_name())
                .chain(children.iter().map(|member| member.card.display_name()))
                .collect(),
            FleetItem::PendingCreate { name, .. } => vec![name.to_string()],
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

/// Generated create name per type (`claude-N`, `codex-N`, ...), first N
/// not taken by a current display name or pending create. Deterministic
/// from the Model.
pub fn next_agent_name(model: &Model, agent_type: &amux_ui::AgentType) -> String {
    let taken = all_agent_names(model);
    (1..)
        .map(|n| format!("{}-{n}", amux_ui::agent_type_label(agent_type)))
        .find(|candidate| !taken.iter().any(|name| name == candidate))
        .expect("unbounded name space")
}

/// What a keypress asks the shell to do.
#[derive(Debug, PartialEq)]
pub enum UiAction {
    Quit,
    Attach(AgentId),
    /// Ask the terminal boundary to publish this exact block text through
    /// OSC 52. Key handlers never write terminal control sequences directly.
    CopyToClipboard(String),
    /// Open the chat screen for an agent (A1/A3): stays inside the
    /// chrome — no terminal handoff — and notes the attach for the
    /// subscription policy.
    OpenChat(AgentId),
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

    #[test]
    fn generated_names_are_scoped_to_the_agent_type() {
        let model = Model::default();
        assert_eq!(
            next_agent_name(
                &model,
                &amux_ui::AgentType::Claude {
                    driver: amux_ui::ClaudeDriver::Pty,
                },
            ),
            "claude-1"
        );
        assert_eq!(
            next_agent_name(
                &model,
                &amux_ui::AgentType::Codex {
                    model: None,
                    approval_policy: None,
                    sandbox_policy: None,
                    resume_thread_id: None,
                },
            ),
            "codex-1"
        );
    }
}

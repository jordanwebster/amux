//! Renderer-local state and key handling for the fleet screen.
//!
//! ViewState is exactly what `docs/UI.md` allows a renderer to keep: focus,
//! scroll, drafts, navigation. Everything domain-shaped stays in the Model.

use amux_ui::{AgentId, Command, FleetItem, Model};

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
        }
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

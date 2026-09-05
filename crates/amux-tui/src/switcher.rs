//! The profile switcher: which account the fleet is showing.
//!
//! One installation can hold several accounts, each a complete device with
//! its own agents, trust and cloud link. The switcher is the door between
//! them: it lists what the installation has and hands a selection back to
//! the shell, which rebinds the runtime. Nothing here reaches a daemon —
//! the rows arrive from the shell, already read from the front door.

use amux_ui::ProfileEntry;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

/// The open switcher: the accounts on this installation and the row the
/// cursor is on.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SwitcherState {
    pub entries: Vec<ProfileEntry>,
    /// Index into `entries`; always in range while any entry exists.
    pub selected: usize,
}

/// What a keypress in the switcher decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitcherOutcome {
    /// Rebind the shell to this profile.
    Switch(ProfileEntry),
    /// Leave the account on screen alone.
    Close,
}

impl SwitcherState {
    /// Open on `current` when the installation still lists it, so the first
    /// thing the person sees is where they are, not where the list starts.
    pub fn open(entries: Vec<ProfileEntry>, current: Option<&std::path::Path>) -> Self {
        let selected = current
            .and_then(|socket| {
                entries
                    .iter()
                    .position(|entry| entry.socket.as_path() == socket)
            })
            .unwrap_or(0);
        Self { entries, selected }
    }

    /// The row under the cursor, if the installation listed any profile.
    pub fn selected(&self) -> Option<&ProfileEntry> {
        self.entries.get(self.selected)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// Apply one keypress. `None` means the switcher stays open and the
    /// shell has nothing to do.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SwitcherOutcome> {
        // A control chord is never a switcher key: the chrome-wide Ctrl+C
        // guard and the leader are handled before this is reached, and the
        // rest belong to whatever else has them.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
                None
            }
            // Enter on an empty list closes rather than selecting nothing:
            // an installation with no other account has nothing to switch to.
            KeyCode::Enter => Some(
                self.selected()
                    .cloned()
                    .map_or(SwitcherOutcome::Close, SwitcherOutcome::Switch),
            ),
            KeyCode::Esc | KeyCode::Char('q') => Some(SwitcherOutcome::Close),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use amux_ui::ProfileId;
    use uuid::Uuid;

    use super::*;

    fn entry(label: &str, index: u128) -> ProfileEntry {
        ProfileEntry {
            id: ProfileId(Uuid::from_u128(index)),
            label: label.to_string(),
            email: Some(format!("{label}@example.com")),
            status: "connected".to_string(),
            socket: PathBuf::from(format!("/tmp/amux/{label}.sock")),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_switcher_opens_on_the_account_already_showing() {
        let entries = vec![entry("personal", 1), entry("work", 2)];
        let socket = entries[1].socket.clone();
        let state = SwitcherState::open(entries, Some(&socket));
        assert_eq!(state.selected().map(|entry| entry.label.as_str()), Some("work"));
    }

    #[test]
    fn switcher_movement_stops_at_both_ends() {
        let mut state = SwitcherState::open(vec![entry("personal", 1), entry("work", 2)], None);
        assert_eq!(state.handle_key(key(KeyCode::Char('k'))), None);
        assert_eq!(state.selected, 0, "k at the top stays put");
        state.handle_key(key(KeyCode::Char('j')));
        state.handle_key(key(KeyCode::Down));
        assert_eq!(state.selected, 1, "j past the bottom stays put");
        state.handle_key(key(KeyCode::Up));
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn switcher_enter_selects_and_esc_closes() {
        let mut state = SwitcherState::open(vec![entry("personal", 1), entry("work", 2)], None);
        state.handle_key(key(KeyCode::Char('j')));
        let outcome = state.handle_key(key(KeyCode::Enter));
        assert_eq!(
            outcome,
            Some(SwitcherOutcome::Switch(entry("work", 2))),
            "enter switches to the selected account"
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Esc)),
            Some(SwitcherOutcome::Close)
        );
    }

    /// An installation the front door listed as empty is a list with no
    /// selection: enter must not fabricate one.
    #[test]
    fn the_switcher_closes_on_enter_with_nothing_to_switch_to() {
        let mut state = SwitcherState::open(Vec::new(), None);
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            Some(SwitcherOutcome::Close)
        );
    }
}

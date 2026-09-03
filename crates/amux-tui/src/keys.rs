//! Key handling for the fleet screen: keys mutate ViewState and produce
//! `UiAction`s; all domain writes leave as Commands through the runtime.

use amux_ui::{Command, Model};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::view::{Mode, Notice, OpenMode, UiAction, ViewState, VisibleRow, visible_rows};

pub fn handle_key(
    view: &mut ViewState,
    model: &Model,
    key: KeyEvent,
    list_rows: usize,
    now: DateTime<Utc>,
) -> Option<UiAction> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    // Any keypress dismisses transient status-line messages (dismissal is
    // view state; the Model keeps the outcome).
    view.notice = None;
    if let Some(failure) = model.latest_op_failure() {
        view.dismissed_error_seq = view.dismissed_error_seq.max(failure.seq);
    }
    let pending_g = std::mem::take(&mut view.pending_g);

    // Ctrl+C is the chrome-wide guarded abandon key (`docs/CHAT.md`
    // §Keybindings), ONE rule for the whole TUI: a focused non-empty text
    // field (here: the filter, the rename draft) is cleared — the
    // clearing press never arms — and otherwise the first press arms the
    // quit guard, a second within the window quits. A single Ctrl+C never
    // quits; the arm renders in the status line.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        match &mut view.mode {
            Mode::Filter if !view.filter.is_empty() => {
                view.filter.clear();
                view.selected = 0;
                view.scroll = 0;
                view.quit_guard.note_clear();
            }
            Mode::Rename { draft, .. } if !draft.is_empty() => {
                draft.clear();
                view.quit_guard.note_clear();
            }
            _ => {
                if view.quit_guard.press(now) {
                    return Some(UiAction::Quit);
                }
            }
        }
        return None;
    }
    // Any other key disarms the quit guard.
    view.quit_guard.disarm();

    match view.mode.clone() {
        Mode::Help => {
            view.mode = Mode::Normal;
            None
        }
        Mode::ConfirmDelete { agent, .. } => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    view.mode = Mode::Normal;
                    return Some(UiAction::Dispatch(Command::DeleteAgent { agent }));
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    view.mode = Mode::Normal;
                }
                _ => {}
            }
            None
        }
        Mode::Rename { agent, mut draft } => {
            match key.code {
                KeyCode::Enter => {
                    view.mode = Mode::Normal;
                    if !draft.trim().is_empty() {
                        return Some(UiAction::Dispatch(Command::RenameAgent {
                            agent,
                            name: draft.trim().to_string(),
                        }));
                    }
                }
                KeyCode::Esc => view.mode = Mode::Normal,
                KeyCode::Backspace => {
                    draft.pop();
                    view.mode = Mode::Rename { agent, draft };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    draft.push(c);
                    view.mode = Mode::Rename { agent, draft };
                }
                _ => view.mode = Mode::Rename { agent, draft },
            }
            None
        }
        Mode::Filter => match key.code {
            KeyCode::Esc => {
                view.mode = Mode::Normal;
                None
            }
            // Ctrl+Enter (kitty tier — only a kitty-probed terminal can
            // deliver it) opens the non-default mode; plain Enter the
            // default. The `o` fallback is Normal-mode only — here it is
            // a printable (P2).
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                open_selected(view, model, true)
            }
            KeyCode::Enter => open_selected(view, model, false),
            KeyCode::Down => {
                move_selection(view, model, 1, list_rows);
                None
            }
            KeyCode::Up => {
                move_selection(view, model, -1, list_rows);
                None
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                move_selection(view, model, 1, list_rows);
                None
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                move_selection(view, model, -1, list_rows);
                None
            }
            KeyCode::Backspace => {
                view.filter.pop();
                view.selected = 0;
                view.scroll = 0;
                None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                view.filter.push(c);
                view.selected = 0;
                view.scroll = 0;
                None
            }
            _ => None,
        },
        Mode::Normal => match key.code {
            // `q` keeps its single-press quit where the guarded Ctrl+C
            // takes two: a deliberately typed letter is not a reflex —
            // the guard exists for the shell's ^C muscle memory, and
            // Normal mode has no text field for `q` to type into.
            KeyCode::Char('q') => Some(UiAction::Quit),
            KeyCode::Char('j') | KeyCode::Down => {
                move_selection(view, model, 1, list_rows);
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                move_selection(view, model, -1, list_rows);
                None
            }
            KeyCode::Char('g') => {
                if pending_g {
                    view.selected = 0;
                    view.scroll = 0;
                } else {
                    view.pending_g = true;
                }
                None
            }
            KeyCode::Char('G') => {
                let visible = visible_rows(model, view).len();
                view.selected = visible.saturating_sub(1);
                view.clamp_selection(visible, list_rows);
                None
            }
            KeyCode::Char('/') | KeyCode::Char('i') => {
                view.mode = Mode::Filter;
                None
            }
            // Entry (A1): Enter opens the settings-default mode;
            // Ctrl+Enter (kitty tier) and the guaranteed plain fallback
            // `o` open the other one.
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                open_selected(view, model, true)
            }
            KeyCode::Enter => open_selected(view, model, false),
            KeyCode::Char('o') => open_selected(view, model, true),
            KeyCode::Char('n') => {
                let host = selected_agent_host(view, model);
                Some(UiAction::Create { host })
            }
            KeyCode::Char('r') => {
                if let Some(card) = selected_agent(view, model) {
                    view.mode = Mode::Rename {
                        agent: card.agent.id,
                        draft: card.display_name(),
                    };
                }
                None
            }
            KeyCode::Char('d') => {
                if let Some(card) = selected_agent(view, model) {
                    view.mode = Mode::ConfirmDelete {
                        agent: card.agent.id,
                        name: card.display_name(),
                    };
                }
                None
            }
            // `z` is vim's fold key: it opens or shuts the family the
            // selected row belongs to. On a row that is in no family
            // there is nothing to fold and the press does nothing — the
            // help overlay says so.
            KeyCode::Char('z') => {
                if view.toggle_fold(model, view.selected) {
                    let visible = visible_rows(model, view).len();
                    view.clamp_selection(visible, list_rows);
                }
                None
            }
            KeyCode::Char('?') => {
                view.mode = Mode::Help;
                None
            }
            _ => None,
        },
    }
}

fn move_selection(view: &mut ViewState, model: &Model, delta: isize, list_rows: usize) {
    let visible = visible_rows(model, view).len();
    if visible == 0 {
        return;
    }
    let selected = view.selected as isize + delta;
    view.selected = selected.clamp(0, visible as isize - 1) as usize;
    view.clamp_selection(visible, list_rows);
}

fn selected_agent<'a>(view: &ViewState, model: &'a Model) -> Option<&'a amux_ui::AgentCard> {
    match visible_rows(model, view).into_iter().nth(view.selected) {
        Some(VisibleRow::Agent(row)) => Some(row.card),
        _ => None,
    }
}

/// Create targets the selected row's host (falling back to the local host
/// via `host: None`, which lets the daemon pick itself).
fn selected_agent_host(view: &ViewState, model: &Model) -> Option<amux_ui::HostId> {
    selected_agent(view, model).map(|card| card.agent.host_id)
}

/// An entry key on a row: open the agent in the resolved mode (A1) —
/// unless the host is known-offline, when the status line carries the
/// daemon's `last_dial_error` instead (both modes need the host: attach
/// for the PTY, chat for the stream subscription).
fn open_selected(view: &mut ViewState, model: &Model, other_mode: bool) -> Option<UiAction> {
    let card = selected_agent(view, model)?;
    let host_id = card.agent.host_id;
    if !model.host_online(host_id) {
        let host = model.host_name(host_id).unwrap_or("host");
        let detail = model
            .host(host_id)
            .and_then(|state| state.entry.last_dial_error.clone())
            .unwrap_or_else(|| "no route".to_string());
        view.notice = Some(Notice::problem(format!("{host} is offline: {detail}")));
        return None;
    }
    let mut mode = view.default_open_mode;
    if other_mode {
        mode = mode.other();
    }
    // Read-only agents and kinds without terminal_v1 open in chat only —
    // raw attach is absent, not disabled, so every entry key opens the one
    // mode that exists. For an SDK-driven Claude this is the deliberate
    // unsupported placeholder; opening it must not ask the daemon for a PTY.
    if card.agent.readonly || !card.agent.kind.exposes(amux_ui::Protocol::TerminalV1) {
        mode = OpenMode::Chat;
    }
    Some(match mode {
        OpenMode::RawAttach => UiAction::Attach(card.agent.id),
        OpenMode::Chat => UiAction::OpenChat(card.agent.id),
    })
}

#[cfg(test)]
mod tests {
    use amux_ui::Model;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::view::{Mode, UiAction, ViewState};

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_755_000_000 + seconds, 0).expect("epoch")
    }

    /// The chrome-wide guarded Ctrl+C: with no text field to clear, the
    /// first press arms (a single Ctrl+C never quits) and a fresh second
    /// press quits — from every field-less mode.
    #[test]
    fn ctrl_c_arms_then_a_second_press_quits_from_any_fieldless_mode() {
        let model = Model::default();
        for mode in [Mode::Normal, Mode::Filter, Mode::Help] {
            let mut view = ViewState {
                mode,
                ..ViewState::default()
            };
            let first = handle_key(&mut view, &model, ctrl_c(), 10, t(0));
            assert_eq!(first, None, "the first press arms, never quits");
            assert!(view.quit_guard.is_armed(), "armed state renders");
            let second = handle_key(&mut view, &model, ctrl_c(), 10, t(2));
            assert!(matches!(second, Some(UiAction::Quit)), "second quits");
        }
    }

    #[test]
    fn a_stale_arm_rearms_instead_of_quitting() {
        let model = Model::default();
        let mut view = ViewState::default();
        handle_key(&mut view, &model, ctrl_c(), 10, t(0));
        let late = handle_key(&mut view, &model, ctrl_c(), 10, t(10));
        assert_eq!(late, None, "past the window the press only re-arms");
        assert!(view.quit_guard.is_armed());
    }

    #[test]
    fn any_other_key_disarms() {
        let model = Model::default();
        let mut view = ViewState::default();
        handle_key(&mut view, &model, ctrl_c(), 10, t(0));
        handle_key(
            &mut view,
            &model,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            10,
            t(1),
        );
        assert!(!view.quit_guard.is_armed(), "another key disarms");
        let press = handle_key(&mut view, &model, ctrl_c(), 10, t(2));
        assert_eq!(press, None, "disarmed: the press arms again, no quit");
    }

    /// A non-empty focused text field makes Ctrl+C a clear — the clearing
    /// press never arms; the NEXT press (field now empty) arms; a third
    /// quits. Quit-from-a-full-field is three deliberate presses.
    #[test]
    fn ctrl_c_clears_the_filter_and_never_arms() {
        let model = Model::default();
        let mut view = ViewState {
            mode: Mode::Filter,
            filter: "auth".to_string(),
            selected: 2,
            ..ViewState::default()
        };
        assert_eq!(handle_key(&mut view, &model, ctrl_c(), 10, t(0)), None);
        assert!(view.filter.is_empty(), "the press cleared the filter");
        assert_eq!(view.selected, 0, "clearing resets the selection");
        assert!(!view.quit_guard.is_armed(), "the clearing press never arms");
        assert_eq!(handle_key(&mut view, &model, ctrl_c(), 10, t(1)), None);
        assert!(view.quit_guard.is_armed(), "empty now: the press arms");
        assert!(matches!(
            handle_key(&mut view, &model, ctrl_c(), 10, t(2)),
            Some(UiAction::Quit)
        ));
    }

    #[test]
    fn ctrl_c_clears_the_rename_draft_and_never_arms() {
        let model = Model::default();
        let mut view = ViewState {
            mode: Mode::Rename {
                agent: uuid::Uuid::from_u128(7),
                draft: "half-typed".to_string(),
            },
            ..ViewState::default()
        };
        assert_eq!(handle_key(&mut view, &model, ctrl_c(), 10, t(0)), None);
        let Mode::Rename { draft, .. } = &view.mode else {
            panic!("still renaming — Ctrl+C cleared, it did not cancel");
        };
        assert!(draft.is_empty());
        assert!(!view.quit_guard.is_armed());
    }

    // --- entry-mode resolution (A1/A3) ------------------------------------

    use amux_ui::{Msg, ServerMsg, update};
    use uuid::Uuid;

    fn agent_id() -> amux_ui::AgentId {
        Uuid::from_u128(7)
    }

    /// One synced host with one agent; `readonly` and `online` shape the
    /// A3 and offline cases.
    fn entry_model_with_kind(readonly: bool, online: bool, kind: amux_ui::AgentKind) -> Model {
        let mut model = Model::default();
        let host = amux_ui::HostEntry {
            id: Uuid::from_u128(1),
            name: "mbp".to_string(),
            online,
            version: None,
            capabilities: None,
            trust_status: amux_ui::HostTrustStatus::Trusted,
            last_dial_error: (!online).then(|| "connection refused".to_string()),
        };
        let agent = amux_ui::Agent {
            id: agent_id(),
            host_id: Uuid::from_u128(1),
            name: Some("fix-auth".to_string()),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            kind,
            readonly,
            args: Vec::new(),
            created_at: chrono::DateTime::from_timestamp(1_755_000_000, 0).expect("epoch"),
            parent: None,
            working_on: None,
        };
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(Uuid::from_u128(1)),
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
            Msg::Server(ServerMsg::AgentUpserted { agent }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
        ] {
            update(&mut model, msg);
        }
        model
    }

    fn entry_model(readonly: bool, online: bool) -> Model {
        entry_model_with_kind(
            readonly,
            online,
            amux_ui::AgentKind::Claude {
                driver: amux_ui::ClaudeDriver::Pty,
            },
        )
    }

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn ctrl_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
    }

    fn o_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE)
    }

    /// A1 with the shipped default (raw attach): Enter attaches;
    /// Ctrl+Enter (kitty-delivered) and the plain fallback `o` open the
    /// chat.
    #[test]
    fn enter_opens_the_default_mode_and_o_the_other() {
        let model = entry_model(false, true);
        let mut view = ViewState::default();
        assert_eq!(
            handle_key(&mut view, &model, enter(), 10, t(0)),
            Some(UiAction::Attach(agent_id()))
        );
        assert_eq!(
            handle_key(&mut view, &model, ctrl_enter(), 10, t(0)),
            Some(UiAction::OpenChat(agent_id()))
        );
        assert_eq!(
            handle_key(&mut view, &model, o_key(), 10, t(0)),
            Some(UiAction::OpenChat(agent_id()))
        );
    }

    /// Flipping `ui.default_open_mode` to chat swaps the pair — a
    /// settings change, not a migration (A1).
    #[test]
    fn a_chat_default_swaps_the_entry_pair() {
        let model = entry_model(false, true);
        let mut view = ViewState {
            default_open_mode: crate::view::OpenMode::Chat,
            ..ViewState::default()
        };
        assert_eq!(
            handle_key(&mut view, &model, enter(), 10, t(0)),
            Some(UiAction::OpenChat(agent_id()))
        );
        assert_eq!(
            handle_key(&mut view, &model, o_key(), 10, t(0)),
            Some(UiAction::Attach(agent_id()))
        );
    }

    /// A3: read-only agents open in chat only — raw attach is absent,
    /// not disabled, so every entry key opens the one mode that exists.
    #[test]
    fn readonly_agents_open_in_chat_from_every_entry_key() {
        let model = entry_model(true, true);
        let mut view = ViewState::default();
        for key in [enter(), ctrl_enter(), o_key()] {
            assert_eq!(
                handle_key(&mut view, &model, key, 10, t(0)),
                Some(UiAction::OpenChat(agent_id()))
            );
        }
    }

    /// Claude's SDK driver exposes only claude_sdk_v1. Even when raw attach
    /// is configured as the default, every entry key opens the typed SDK
    /// placeholder instead of handing terminal_v1 to the attach callback.
    #[test]
    fn sdk_agents_open_in_chat_from_every_entry_key() {
        let model = entry_model_with_kind(
            false,
            true,
            amux_ui::AgentKind::Claude {
                driver: amux_ui::ClaudeDriver::Sdk,
            },
        );

        for default_open_mode in [OpenMode::RawAttach, OpenMode::Chat] {
            let mut view = ViewState {
                default_open_mode,
                ..ViewState::default()
            };
            for key in [enter(), ctrl_enter(), o_key()] {
                assert_eq!(
                    handle_key(&mut view, &model, key, 10, t(0)),
                    Some(UiAction::OpenChat(agent_id()))
                );
            }
        }
    }

    /// Enter in Filter mode opens the default mode too (Ctrl+Enter the
    /// other); `o` stays a printable there (P2).
    #[test]
    fn filter_mode_enter_opens_and_o_types() {
        let model = entry_model(false, true);
        let mut view = ViewState {
            mode: Mode::Filter,
            ..ViewState::default()
        };
        assert_eq!(
            handle_key(&mut view, &model, o_key(), 10, t(0)),
            None,
            "`o` narrows the filter, never opens"
        );
        assert_eq!(view.filter, "o");
        view.filter.clear();
        assert_eq!(
            handle_key(&mut view, &model, enter(), 10, t(0)),
            Some(UiAction::Attach(agent_id()))
        );
        assert_eq!(
            handle_key(&mut view, &model, ctrl_enter(), 10, t(0)),
            Some(UiAction::OpenChat(agent_id()))
        );
    }

    /// An offline host refuses both modes with the dial error in the
    /// status line — chat needs the host's stream as much as attach
    /// needs its PTY.
    #[test]
    fn an_offline_host_refuses_entry_with_the_dial_error() {
        let model = entry_model(false, false);
        let mut view = ViewState::default();
        for key in [enter(), o_key()] {
            assert_eq!(handle_key(&mut view, &model, key, 10, t(0)), None);
            assert_eq!(
                view.notice.as_ref().map(|notice| notice.text.as_str()),
                Some("mbp is offline: connection refused")
            );
        }
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let model = Model::default();
        let mut view = ViewState::default();
        let action = handle_key(
            &mut view,
            &model,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            10,
            t(0),
        );
        assert!(matches!(action, Some(UiAction::Quit)));
    }
}

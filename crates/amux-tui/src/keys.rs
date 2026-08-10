//! Key handling for the fleet screen: keys mutate ViewState and produce
//! `UiAction`s; all domain writes leave as Commands through the runtime.

use amux_ui::{Command, Model};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::view::{Mode, UiAction, ViewState, VisibleRow, visible_rows};

pub fn handle_key(
    view: &mut ViewState,
    model: &Model,
    key: KeyEvent,
    list_rows: usize,
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

    // Ctrl-C quits from ANY mode: the chrome is a stateless viewer and must
    // never feel like it traps the terminal. `esc` cancels modes; `C-c`
    // (like `q` in normal mode) exits.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(UiAction::Quit);
    }

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
            KeyCode::Enter => attach_selected(view, model),
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
            KeyCode::Char('q') => Some(UiAction::Quit),
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(UiAction::DebugDump)
            }
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
            KeyCode::Enter => attach_selected(view, model),
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
        Some(VisibleRow::Agent(card)) => Some(card),
        _ => None,
    }
}

/// Create targets the selected row's host (falling back to the local host
/// via `host: None`, which lets the daemon pick itself).
fn selected_agent_host(view: &ViewState, model: &Model) -> Option<amux_ui::HostId> {
    selected_agent(view, model).map(|card| card.agent.host_id)
}

/// Enter on a row: attach, unless the host is known-offline — then the
/// status line carries the daemon's `last_dial_error` instead.
fn attach_selected(view: &mut ViewState, model: &Model) -> Option<UiAction> {
    let card = selected_agent(view, model)?;
    let host_id = card.agent.host_id;
    if !model.host_online(host_id) {
        let host = model.host_name(host_id).unwrap_or("host");
        let detail = model
            .host(host_id)
            .and_then(|state| state.entry.last_dial_error.clone())
            .unwrap_or_else(|| "no route".to_string());
        view.notice = Some(format!("{host} is offline: {detail}"));
        return None;
    }
    Some(UiAction::Attach(card.agent.id))
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

    /// Ctrl-C exits from every mode — including filter/rename, where a bare
    /// `c` would otherwise be text input.
    #[test]
    fn ctrl_c_quits_from_any_mode() {
        let model = Model::default();
        for mode in [
            Mode::Normal,
            Mode::Filter,
            Mode::Rename {
                agent: uuid::Uuid::from_u128(7),
                draft: "half-typed".to_string(),
            },
            Mode::Help,
        ] {
            let mut view = ViewState {
                mode,
                ..ViewState::default()
            };
            let action = handle_key(&mut view, &model, ctrl_c(), 10);
            assert!(matches!(action, Some(UiAction::Quit)), "mode should quit");
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
        );
        assert!(matches!(action, Some(UiAction::Quit)));
    }
}

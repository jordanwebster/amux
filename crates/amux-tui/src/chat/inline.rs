//! A child's ask, answered where the human found it (U2).
//!
//! The parent's chat decides *that* a panel is drawn and where; the
//! child's own layer decides what it looks like and what answering it
//! means. Nothing is copied between the two: the panel reads the ask out
//! of the child's layer under the child's id, and an answer leaves as
//! the child's own `Command` addressed to the child. Opening the child's
//! own chat and answering there is the identical act — one path, no
//! shared state to keep in step, and an ask answered on another device
//! takes this panel away by the same re-derivation that took the banner.
//!
//! The panel docks where the composer is, exactly as a chat's own ask
//! panel does: one ask, one cursor, one place to look. It is therefore
//! offered only while the parent's own bottom block *is* the composer —
//! an agent's own obligations come before its children's.

use amux_ui::claude::AskState;
use amux_ui::claude::encoding::{self, AskAnswer};
use amux_ui::{AgentId, ClaudeCommand, CodexCommand, Command, Model, StructuredProtocol};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::chat::claude::ask_ui::{AskKeyOutcome, AskUi};
use crate::chat::claude::panel;
use crate::chat::codex::render::{ApprovalView, approval_panel};
use crate::composer::Composer;
use crate::render::{Theme, finish_line, new_line, str_width};

/// A child's ask docked in its parent's chat: whose it is, and the
/// child's layer's own panel state for it.
#[derive(Clone, Debug)]
pub(crate) struct InlineAsk {
    child: AgentId,
    ui: Ui,
}

/// Panel state, one variant per layer that knows how to draw an ask. It
/// is the child's layer's own type in both cases — the Claude stage
/// machine and the Codex decision cursor — so an inline panel behaves
/// exactly like the same panel in the child's own chat.
#[derive(Clone, Debug)]
enum Ui {
    Claude(AskUi),
    Codex { cursor: usize },
}

/// What a keypress inside the docked child panel asked for.
pub(crate) enum InlineOutcome {
    /// Consumed; panel state may have changed.
    Handled,
    /// Leave the child's ask; the parent's composer comes back.
    Close,
    /// Dispatch this command — always addressed to the child.
    Dispatch(Command),
    /// Not a panel key: the parent may route it to the feed.
    NotHandled,
}

/// Whether the loudest need can actually be docked here — the fact the
/// banner's chord is advertised on, so the hint can never name a key
/// that would do nothing (P10).
///
/// Three ways it cannot: the parent's own bottom block is already taken
/// (its own ask, or a read-only chat), the child has no ask to draw (a
/// finished child needs a person, but not an answer), or the child is
/// read-only, where answering is absent rather than disabled.
pub(crate) fn can_open(model: &Model, parent: AgentId, child: AgentId) -> bool {
    if bottom_is_taken(model, parent) {
        return false;
    }
    let Some(card) = model.agent(child) else {
        return false;
    };
    if card.agent.readonly {
        return false;
    }
    match card.structured_protocol() {
        Some(StructuredProtocol::Claude) => model
            .claude(child)
            .and_then(|layer| layer.ask_head())
            .is_some(),
        Some(StructuredProtocol::Codex) => model
            .codex(child)
            .and_then(|layer| layer.ask_head())
            .is_some(),
        // A layer nothing folds has no ask to dock.
        Some(StructuredProtocol::ClaudeSdk) | None => false,
    }
}

/// The parent's own bottom block is something other than the composer,
/// so there is nowhere to dock a guest.
fn bottom_is_taken(model: &Model, parent: AgentId) -> bool {
    let Some(card) = model.agent(parent) else {
        return true;
    };
    if card.agent.readonly {
        return true;
    }
    match card.structured_protocol() {
        Some(StructuredProtocol::Claude) => model
            .claude(parent)
            .and_then(|layer| layer.ask_head())
            .is_some(),
        Some(StructuredProtocol::Codex) => model
            .codex(parent)
            .and_then(|layer| layer.ask_head())
            .is_some(),
        // The placeholder frame has no composer to give up.
        Some(StructuredProtocol::ClaudeSdk) | None => true,
    }
}

impl InlineAsk {
    /// Dock the child's ask, taking its panel state from the child's
    /// layer. `None` when there is nothing to dock — the caller has
    /// usually asked [`can_open`] first, but a stale banner is possible
    /// and answering it with nothing is the right outcome.
    pub(crate) fn open(model: &Model, child: AgentId) -> Option<Self> {
        let ui = match model.agent(child)?.structured_protocol()? {
            StructuredProtocol::Claude => {
                Ui::Claude(AskUi::for_ask(model.claude(child)?.ask_head()?))
            }
            StructuredProtocol::Codex => {
                model.codex(child)?.ask_head()?;
                Ui::Codex { cursor: 0 }
            }
            StructuredProtocol::ClaudeSdk => return None,
        };
        Some(Self { child, ui })
    }

    /// The text field the child's panel has open, if any — the paste
    /// target and the surface the guarded Ctrl+C clears, derived here so
    /// the parent's chat does not have to know which layer it is hosting.
    pub(crate) fn active_field(&mut self) -> Option<&mut Composer> {
        match &mut self.ui {
            Ui::Claude(ui) => ui.active_field(),
            Ui::Codex { .. } => None,
        }
    }
}

/// Re-derive the docked panel against the Model, dropping it when the
/// reason for it is gone: the ask was answered (here, in the child's own
/// chat, or on another device), the child left, or the parent's own
/// business took the bottom block back. A Claude ask that was replaced
/// by the next one in the child's queue gets a fresh panel rather than
/// the previous ask's typed state.
pub(crate) fn reconcile(model: &Model, parent: AgentId, slot: &mut Option<InlineAsk>) {
    let Some(inline) = slot else { return };
    if bottom_is_taken(model, parent) {
        *slot = None;
        return;
    }
    let child = inline.child;
    match &mut inline.ui {
        Ui::Claude(ui) => match model.claude(child).and_then(|layer| layer.ask_head()) {
            Some(ask) if ask.id == ui.ask_id => {}
            Some(ask) => *ui = AskUi::for_ask(ask),
            None => *slot = None,
        },
        Ui::Codex { cursor } => match model.codex(child).and_then(|layer| layer.ask_head()) {
            Some(ask) => *cursor = (*cursor).min(ask.actions.len().saturating_sub(1)),
            None => *slot = None,
        },
    }
}

/// The docked child panel: the child's layer's own rows, under a rule
/// that says whose they are and how to leave. Without that rule the
/// human would be looking at an ask panel in a chat and have every
/// reason to read it as this agent's.
pub(crate) fn panel_lines(
    model: &Model,
    inline: &InlineAsk,
    width: usize,
    theme: Theme,
    quit_guard_armed: bool,
) -> Vec<Line<'static>> {
    let child = inline.child;
    let mut lines = match &inline.ui {
        Ui::Claude(ui) => {
            let Some(ask) = model.claude(child).and_then(|layer| layer.ask_head()) else {
                return Vec::new();
            };
            let count = model
                .claude(child)
                .map(|layer| layer.ask_count())
                .unwrap_or(1);
            panel::panel_lines(ask, count, Some(ui), None, width, theme, quit_guard_armed)
        }
        Ui::Codex { cursor } => {
            let Some(ask) = model.codex(child).and_then(|layer| layer.ask_head()) else {
                return Vec::new();
            };
            approval_panel(
                model,
                ApprovalView {
                    agent: child,
                    cursor: *cursor,
                    failure: None,
                },
                ask,
                width,
                theme,
            )
        }
    };
    // Both layers open their panel with the same plain takeover rule
    // (C1). Replacing it costs no rows and puts the attribution exactly
    // where the boundary already is.
    let name = model
        .agent(child)
        .map(|card| card.display_name())
        .unwrap_or_else(|| "a subagent".to_string());
    let attribution = attribution_rule(&name, width, theme);
    match lines.first_mut() {
        Some(first) => *first = attribution,
        None => lines.push(attribution),
    }
    lines
}

/// `─ answering test-runner ────────────────── esc back ─`
///
/// Saturating, because the width is not always a width a frame could be
/// drawn at: `layout` asks the bottom block how many rows it wants at
/// whatever viewport it was handed, and that question is asked before the
/// too-small notice takes over.
fn attribution_rule(name: &str, width: usize, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    let right = " esc back ─";
    let mut text = format!("─ answering {name} ");
    while 1 + str_width(&text) + str_width(right) < width.saturating_sub(1) {
        text.push('─');
    }
    text.push_str(right);
    line.spans.push(Span::styled(text, theme.muted()));
    finish_line(&mut line, width);
    line
}

/// One keypress inside the docked child panel. The stage machine and the
/// decision cursor are the child layer's own; everything this function
/// adds is addressing the resulting command to the child.
pub(crate) fn handle_key(model: &Model, inline: &mut InlineAsk, key: &KeyEvent) -> InlineOutcome {
    let child = inline.child;
    // Ctrl+X interrupts the agent whose ask is on screen — the same rule
    // as in that agent's own chat. While a guest panel is docked that is
    // the child; Esc gives the key back to the parent.
    if key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return interrupt(model, inline);
    }
    match &mut inline.ui {
        Ui::Claude(ui) => {
            let Some(ask) = model.claude(child).and_then(|layer| layer.ask_head()) else {
                return InlineOutcome::Close;
            };
            // Esc steps back through the child's own stages first and
            // leaves the panel only from its floor, exactly as it does in
            // the child's chat — a half-typed denial is not lost to one
            // keystroke.
            if key.code == KeyCode::Esc {
                return if ui.step_back() {
                    InlineOutcome::Handled
                } else {
                    InlineOutcome::Close
                };
            }
            // The two states the child's own chat routes without a stage
            // machine: an answer already in flight has nothing to select
            // (C5), and an unverified menu shape has no actions to offer
            // (C2). Both stay on screen and consume nothing.
            if !matches!(ask.state, AskState::Pending | AskState::SendFailed { .. })
                || encoding::menu_shape_refusal(&ask.kind).is_some()
            {
                return InlineOutcome::NotHandled;
            }
            match ui.handle_key(ask, key, true) {
                AskKeyOutcome::Answer(answer) => {
                    InlineOutcome::Dispatch(answer_command(child, ask.id, answer))
                }
                AskKeyOutcome::Handled => InlineOutcome::Handled,
                // The fullscreen reader belongs to the chat whose agent
                // the artifact is from; a guest panel does not take the
                // parent's whole frame over. The key is consumed rather
                // than leaked into the feed.
                AskKeyOutcome::OpenReader => InlineOutcome::Handled,
                AskKeyOutcome::NotHandled => InlineOutcome::NotHandled,
            }
        }
        Ui::Codex { cursor } => {
            let Some(ask) = model.codex(child).and_then(|layer| layer.ask_head()) else {
                return InlineOutcome::Close;
            };
            if key.code == KeyCode::Esc {
                return InlineOutcome::Close;
            }
            let count = ask.actions.len();
            let allows_answer = amux_ui::codex::allows_answer(model, child);
            match key.code {
                KeyCode::Char(digit @ '1'..='9') if allows_answer => {
                    let index = digit as usize - '1' as usize;
                    if index < count {
                        *cursor = index;
                    }
                    InlineOutcome::Handled
                }
                KeyCode::Up if allows_answer => {
                    *cursor = cursor.saturating_sub(1);
                    InlineOutcome::Handled
                }
                KeyCode::Down if allows_answer => {
                    *cursor = (*cursor + 1).min(count.saturating_sub(1));
                    InlineOutcome::Handled
                }
                KeyCode::Enter => {
                    if !allows_answer {
                        return InlineOutcome::Handled;
                    }
                    match ask.actions.get(*cursor).and_then(|action| action.decision) {
                        Some(decision) => {
                            InlineOutcome::Dispatch(Command::Codex(CodexCommand::Answer {
                                agent: child,
                                request_id: ask.request_id.clone(),
                                decision,
                            }))
                        }
                        None => InlineOutcome::Handled,
                    }
                }
                _ => InlineOutcome::NotHandled,
            }
        }
    }
}

fn interrupt(model: &Model, inline: &InlineAsk) -> InlineOutcome {
    let child = inline.child;
    match &inline.ui {
        Ui::Claude(_) => {
            InlineOutcome::Dispatch(Command::Claude(ClaudeCommand::Interrupt { agent: child }))
        }
        Ui::Codex { .. } if amux_ui::codex::allows_interrupt(model, child) => {
            InlineOutcome::Dispatch(Command::Codex(CodexCommand::Interrupt { agent: child }))
        }
        Ui::Codex { .. } => InlineOutcome::Handled,
    }
}

fn answer_command(child: AgentId, ask: u64, answer: AskAnswer) -> Command {
    Command::Claude(ClaudeCommand::AnswerAsk {
        agent: child,
        ask,
        answer,
    })
}

/// Text pasted while a child panel is docked belongs to whatever field
/// that panel has open, and is dropped when it has none — printables
/// never reach the parent's draft from behind a guest panel (P2).
pub(crate) fn handle_paste(inline: &mut InlineAsk, text: &str) {
    if let Some(field) = inline.active_field() {
        field.paste(text);
    }
}

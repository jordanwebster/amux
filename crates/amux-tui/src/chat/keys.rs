//! Chat key handling: keys mutate ChatView and produce `UiAction`s; all
//! domain writes leave as Commands through the runtime (never bytes —
//! encodings live in amux-ui's C6 module).
//!
//! The binding set is `docs/CHAT.md` §Keybindings' plain tier, derived in
//! `notes/chat-v1/keybindings.md`: readline is law inside the composer
//! (P6), reflex keys stay harmless (P4), interrupt shares a key with
//! nothing (P5). Kitty-tier sugar (Shift+Enter newline) is absent until
//! the chrome feature-detects kitty — Phase 6; hints never advertise it.

use amux_ui::{Command, Model};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::chat::{ChatView, FeedScroll, entry_watermark, render};
use crate::view::UiAction;

pub fn handle_chat_key(
    chat: &mut ChatView,
    model: &Model,
    key: KeyEvent,
    viewport: (u16, u16),
) -> Option<UiAction> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    // Any keypress dismisses a stated send failure (dismissal is view
    // state; the Model keeps the outcome).
    chat.send_failure = None;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        // D3: interrupt is the one deliberate binding that works in every
        // focus state, even while send is gated. Never on Esc, never on
        // Ctrl+C. The reducer dispatches it ungated.
        KeyCode::Char('x') if ctrl => {
            Some(UiAction::Dispatch(Command::Interrupt { agent: chat.agent }))
        }
        // Ctrl+C on a non-empty draft: abandon the whole draft as a kill
        // (yankable — a single ^C never loses text it didn't visibly
        // kill). On an empty draft this arms the chrome-wide two-press
        // quit guard — Phase 6's chrome integration; until it lands the
        // empty branch is a deliberate no-op (a single ^C must never
        // quit).
        KeyCode::Char('c') if ctrl => {
            if !chat.composer.is_empty() {
                chat.composer.kill_all();
            }
            None
        }
        // The settled view-only Esc chain: never answers, never
        // interrupts.
        KeyCode::Esc => {
            esc_chain(chat);
            None
        }
        KeyCode::Enter => send(chat, model),
        // Ctrl+J: the guaranteed newline in any terminal (Shift+Enter is
        // kitty sugar, Phase 6).
        KeyCode::Char('j') if ctrl => {
            chat.composer.insert_newline();
            None
        }
        // D4: Shift+Tab cycles the permission mode; the current mode
        // renders in the footer from hook facts. Gated exactly where the
        // injected CSI Z would not reach claude's composer.
        KeyCode::BackTab => {
            if model.claude_mode_cycle_gate(chat.agent).is_none() {
                Some(UiAction::Dispatch(Command::CyclePermissionMode {
                    agent: chat.agent,
                }))
            } else {
                None
            }
        }
        // Tab is reserved for the future queueing door (D2) — a no-op
        // until that lands deliberately.
        KeyCode::Tab => None,
        KeyCode::PageUp => {
            page_up(chat, model, viewport);
            None
        }
        KeyCode::PageDown => {
            page_down(chat, model, viewport);
            None
        }
        // Ctrl+Home / Ctrl+End: feed oldest / newest (ext tier —
        // convenience, never the sole path; PgUp/PgDn are guaranteed).
        KeyCode::Home if ctrl => {
            let (_, feed_h) = scroll_metrics(chat, model, viewport);
            let total = render::feed_line_count(model, chat, viewport.0 as usize);
            if total > feed_h {
                pause_at(chat, model, 0);
            }
            None
        }
        KeyCode::End if ctrl => {
            chat.scroll = FeedScroll::Following;
            None
        }
        // The readline set (P6). Ctrl+A is the chrome leader and is never
        // shadowed here; Home and Ctrl+E serve line motion.
        KeyCode::Home => {
            chat.composer.home();
            None
        }
        KeyCode::End => {
            chat.composer.end();
            None
        }
        KeyCode::Left if ctrl => {
            chat.composer.word_left();
            None
        }
        KeyCode::Right if ctrl => {
            chat.composer.word_right();
            None
        }
        KeyCode::Left => {
            chat.composer.left();
            None
        }
        KeyCode::Right => {
            chat.composer.right();
            None
        }
        KeyCode::Up => {
            chat.composer.up();
            None
        }
        KeyCode::Down => {
            chat.composer.down();
            None
        }
        KeyCode::Backspace => {
            chat.composer.backspace();
            None
        }
        KeyCode::Delete => {
            chat.composer.delete_forward();
            None
        }
        KeyCode::Char(c) if ctrl => {
            match c {
                'b' => chat.composer.left(),
                'f' => chat.composer.right(),
                'p' => chat.composer.up(),
                'n' => chat.composer.down(),
                'e' => chat.composer.end(),
                'w' => chat.composer.kill_word_back(),
                'u' => chat.composer.kill_to_line_start(),
                'k' => chat.composer.kill_to_line_end(),
                'd' => chat.composer.delete_forward(),
                'y' => chat.composer.yank(),
                // Deliberately unbound, each an act of restraint: Ctrl+A
                // (chrome leader), Ctrl+G (emacs abort reflex — must never
                // fire agent actions), Ctrl+R (reserved: history search),
                // Ctrl+L (shell redraw reflex), Ctrl+T (plan reader,
                // Phase 5), Ctrl+V (bracketed paste owns pasting), and the
                // byte-aliases Ctrl+H/I/M.
                _ => {}
            }
            None
        }
        // Printables belong to the draft (P2) — including `?`: the help
        // overlay on an empty draft is Phase 6's chrome work; until it
        // exists, `?` types.
        KeyCode::Char(c) => {
            chat.composer.insert(c);
            None
        }
        _ => None,
    }
}

/// Enter: send, gated on phase by the same derivation the footer states
/// (D2) — while gated, Enter is a no-op and the draft is kept.
fn send(chat: &mut ChatView, model: &Model) -> Option<UiAction> {
    if chat.composer.is_empty() {
        return None;
    }
    if model.claude_send_gate(chat.agent).refusal().is_some() {
        return None;
    }
    let text = chat.composer.text();
    chat.composer.clear_for_send();
    Some(UiAction::Dispatch(Command::SendPrompt {
        agent: chat.agent,
        text,
    }))
}

/// The deterministic view-only Esc chain (`docs/CHAT.md` §State
/// transitions), checked in order — first hit wins. Esc never answers an
/// ask and never interrupts.
fn esc_chain(chat: &mut ChatView) {
    // Stage 1 (Phase 5): close the reader — a plan-review reader drops to
    // its docked panel.
    // Stage 2 (Phase 5): step back ask stages, flooring at the menu stage
    // — the panel is never dismissed while its ask pends.
    // Stage 3: reset feed scroll to following — empty draft only.
    if matches!(chat.scroll, FeedScroll::Paused { .. }) && chat.composer.is_empty() {
        chat.scroll = FeedScroll::Following;
    }
    // Stage 4: nothing. (Each earlier stage must `return` once Phase 5
    // adds stages 1–2 — first hit wins.)
}

/// Scroll bounds under the paused layout (the paused rule takes a row, so
/// paging targets that geometry).
fn scroll_metrics(chat: &ChatView, model: &Model, viewport: (u16, u16)) -> (usize, usize) {
    let layout = render::layout(model, chat, viewport);
    let feed_h = layout.feed_height_when_paused().max(1);
    let page = feed_h.saturating_sub(1).max(1);
    (page, feed_h)
}

fn pause_at(chat: &mut ChatView, model: &Model, top_line: usize) {
    let entry_watermark = match chat.scroll {
        // Re-anchoring while already paused keeps the original watermark:
        // "new entries" counts from when following stopped.
        FeedScroll::Paused {
            entry_watermark, ..
        } => entry_watermark,
        FeedScroll::Following => entry_watermark(model, chat.agent),
    };
    chat.scroll = FeedScroll::Paused {
        top_line,
        entry_watermark,
    };
}

fn page_up(chat: &mut ChatView, model: &Model, viewport: (u16, u16)) {
    let (page, feed_h) = scroll_metrics(chat, model, viewport);
    let total = render::feed_line_count(model, chat, viewport.0 as usize);
    match chat.scroll {
        FeedScroll::Following => {
            let max_top = total.saturating_sub(feed_h);
            if max_top == 0 {
                // Everything already fits: nothing to scroll back to.
                return;
            }
            pause_at(chat, model, max_top.saturating_sub(page));
        }
        FeedScroll::Paused { top_line, .. } => {
            pause_at(chat, model, top_line.saturating_sub(page));
        }
    }
}

fn page_down(chat: &mut ChatView, model: &Model, viewport: (u16, u16)) {
    let FeedScroll::Paused { top_line, .. } = chat.scroll else {
        return;
    };
    let (page, feed_h) = scroll_metrics(chat, model, viewport);
    let total = render::feed_line_count(model, chat, viewport.0 as usize);
    let max_top = total.saturating_sub(feed_h);
    let next = top_line + page;
    if next >= max_top {
        // Reaching the bottom resumes following — sticky-bottom re-pins,
        // no dedicated resume key (keybindings §2.4).
        chat.scroll = FeedScroll::Following;
    } else {
        pause_at(chat, model, next);
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::{Model, Msg, OpId, ServerMsg, StreamEntry, StreamMsg, update};
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn agent_id() -> amux_ui::AgentId {
        Uuid::from_u128(7)
    }

    fn t(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_755_000_000 + seconds, 0).expect("epoch")
    }

    fn fold(model: &mut Model, msgs: Vec<Msg>) {
        for msg in msgs {
            update(model, msg);
        }
    }

    fn base_msgs() -> Vec<Msg> {
        let agent = amux_ui::Agent {
            id: agent_id(),
            host_id: Uuid::from_u128(1),
            name: Some("fix-auth".to_string()),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            agent_type: "claude".to_string(),
            io_protocols: vec![
                "claude_raw_v1".to_string(),
                "claude_pty_transcript_v1".to_string(),
            ],
            readonly: false,
            args: Vec::new(),
            created_at: t(0),
        };
        let host = amux_ui::HostEntry {
            id: Uuid::from_u128(1),
            name: "mbp".to_string(),
            online: true,
            version: None,
            capabilities: None,
            trust_status: amux_ui::HostTrustStatus::Trusted,
            last_dial_error: None,
        };
        vec![
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(Uuid::from_u128(1)),
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
            Msg::Server(ServerMsg::AgentUpserted { agent }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Opened { truncated: false },
            },
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::ReplayComplete,
            },
        ]
    }

    fn rows(at: i64, first_seq: u64, payloads: Vec<serde_json::Value>) -> Msg {
        Msg::Stream {
            agent: agent_id(),
            event: StreamMsg::Batch {
                at: t(at),
                entries: payloads
                    .into_iter()
                    .enumerate()
                    .map(|(offset, payload)| StreamEntry {
                        seq: first_seq + offset as u64,
                        payload,
                    })
                    .collect(),
            },
        }
    }

    fn ready_row() -> serde_json::Value {
        json!({"type": "amux.transcript_ready"})
    }

    fn prompt_row(n: u8) -> serde_json::Value {
        json!({
            "type": "user",
            "uuid": format!("dddddddd-0000-4000-8000-0000000000{n:02}"),
            "sessionId": "22222222-2222-4222-8222-222222222222",
            "timestamp": "2026-08-12T09:00:00.000Z",
            "message": {"role": "user", "content": format!("prompt {n}")},
            "origin": {"kind": "human"},
            "promptSource": "typed",
        })
    }

    fn idle_model() -> Model {
        let mut model = Model::default();
        fold(&mut model, base_msgs());
        fold(&mut model, vec![rows(1, 1, vec![ready_row()])]);
        model
    }

    fn working_model() -> Model {
        let mut model = idle_model();
        fold(&mut model, vec![rows(2, 2, vec![prompt_row(1)])]);
        model
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn chat_with_draft(text: &str) -> ChatView {
        let mut chat = ChatView::open(agent_id());
        chat.composer.insert_str(text);
        chat
    }

    const VIEWPORT: (u16, u16) = (80, 20);

    #[test]
    fn enter_sends_when_ready_and_clears_the_draft() {
        let model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let action = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT);
        assert_eq!(
            action,
            Some(UiAction::Dispatch(Command::SendPrompt {
                agent: agent_id(),
                text: "add retry".to_string(),
            }))
        );
        assert!(chat.composer.is_empty(), "the draft moved into the send");
    }

    #[test]
    fn enter_is_a_noop_while_gated_and_keeps_the_draft() {
        let model = working_model();
        let mut chat = chat_with_draft("and document it");
        let action = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT);
        assert_eq!(action, None, "send is gated while working (D2)");
        assert_eq!(chat.composer.text(), "and document it", "draft kept");
    }

    #[test]
    fn enter_on_an_empty_draft_is_a_noop() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id());
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT),
            None
        );
    }

    #[test]
    fn ctrl_x_interrupts_in_every_state_and_never_touches_the_draft() {
        for model in [idle_model(), working_model()] {
            let mut chat = chat_with_draft("precious draft");
            let action = handle_chat_key(&mut chat, &model, ctrl('x'), VIEWPORT);
            assert_eq!(
                action,
                Some(UiAction::Dispatch(Command::Interrupt { agent: agent_id() }))
            );
            assert_eq!(chat.composer.text(), "precious draft");
        }
    }

    #[test]
    fn ctrl_c_clears_a_nonempty_draft_as_a_yankable_kill() {
        let model = idle_model();
        let mut chat = chat_with_draft("half a thought");
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT),
            None
        );
        assert!(chat.composer.is_empty());
        chat.composer.yank();
        assert_eq!(chat.composer.text(), "half a thought", "clear is a kill");
    }

    #[test]
    fn ctrl_c_on_an_empty_draft_is_a_noop_until_phase_6() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id());
        assert_eq!(
            handle_chat_key(&mut chat, &model, ctrl('c'), VIEWPORT),
            None
        );
        assert!(chat.composer.is_empty());
    }

    #[test]
    fn shift_tab_cycles_the_mode_only_when_the_injection_would_reach_claude() {
        let mut chat = ChatView::open(agent_id());
        let action = handle_chat_key(&mut chat, &idle_model(), press(KeyCode::BackTab), VIEWPORT);
        assert_eq!(
            action,
            Some(UiAction::Dispatch(Command::CyclePermissionMode {
                agent: agent_id()
            }))
        );

        // A pending ask owns the keystroke channel: CSI Z would navigate
        // the form — refused.
        let mut model = working_model();
        fold(
            &mut model,
            vec![rows(
                3,
                3,
                vec![json!({
                    "type": "hook.permission_request",
                    "tool_name": "Bash",
                    "tool_input": {"command": "echo hi"},
                })],
            )],
        );
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::BackTab), VIEWPORT),
            None
        );
    }

    #[test]
    fn tab_is_reserved_and_question_mark_types() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id());
        assert_eq!(
            handle_chat_key(&mut chat, &model, press(KeyCode::Tab), VIEWPORT),
            None
        );
        assert!(
            chat.composer.is_empty(),
            "tab stays a no-op (queueing door)"
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::Char('?')), VIEWPORT);
        assert_eq!(
            chat.composer.text(),
            "?",
            "`?` types until the Phase 6 overlay"
        );
    }

    /// A feed tall enough to scroll at the test viewport.
    fn long_feed_model() -> Model {
        let mut model = idle_model();
        for n in 0..20u8 {
            fold(
                &mut model,
                vec![rows(2 + n as i64, 2 + n as u64, vec![prompt_row(n)])],
            );
        }
        model
    }

    #[test]
    fn pgup_pauses_with_a_watermark_and_pgdn_at_the_bottom_resumes() {
        let model = long_feed_model();
        let mut chat = ChatView::open(agent_id());
        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT);
        let FeedScroll::Paused {
            entry_watermark, ..
        } = chat.scroll
        else {
            panic!("PgUp pauses following");
        };
        assert_eq!(entry_watermark, 20, "watermark is the entry count at pause");

        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT);
        handle_chat_key(&mut chat, &model, press(KeyCode::PageDown), VIEWPORT);
        assert!(
            matches!(chat.scroll, FeedScroll::Paused { .. }),
            "mid-feed PgDn stays paused"
        );
        handle_chat_key(&mut chat, &model, press(KeyCode::PageDown), VIEWPORT);
        handle_chat_key(&mut chat, &model, press(KeyCode::PageDown), VIEWPORT);
        assert_eq!(
            chat.scroll,
            FeedScroll::Following,
            "reaching the bottom resumes following"
        );
    }

    #[test]
    fn pgup_with_a_short_feed_stays_following() {
        let model = idle_model();
        let mut chat = ChatView::open(agent_id());
        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT);
        assert_eq!(chat.scroll, FeedScroll::Following);
    }

    #[test]
    fn esc_resets_scroll_only_on_an_empty_draft() {
        let model = long_feed_model();
        let mut chat = chat_with_draft("reading notes");
        handle_chat_key(&mut chat, &model, press(KeyCode::PageUp), VIEWPORT);
        handle_chat_key(&mut chat, &model, press(KeyCode::Esc), VIEWPORT);
        assert!(
            matches!(chat.scroll, FeedScroll::Paused { .. }),
            "a non-empty draft keeps Esc away from the scroll (stage 3 gate)"
        );
        chat.composer.kill_all();
        handle_chat_key(&mut chat, &model, press(KeyCode::Esc), VIEWPORT);
        assert_eq!(chat.scroll, FeedScroll::Following);
    }

    #[test]
    fn readline_chords_edit_the_draft() {
        let model = idle_model();
        let mut chat = chat_with_draft("fix the tests");
        handle_chat_key(&mut chat, &model, ctrl('w'), VIEWPORT);
        assert_eq!(chat.composer.text(), "fix the ");
        handle_chat_key(&mut chat, &model, ctrl('u'), VIEWPORT);
        assert!(chat.composer.is_empty());
        handle_chat_key(&mut chat, &model, ctrl('y'), VIEWPORT);
        assert_eq!(chat.composer.text(), "fix the ");
        handle_chat_key(&mut chat, &model, ctrl('b'), VIEWPORT);
        handle_chat_key(&mut chat, &model, ctrl('d'), VIEWPORT);
        assert_eq!(chat.composer.text(), "fix the");
    }

    #[test]
    fn ctrl_j_inserts_a_newline_instead_of_sending() {
        let model = idle_model();
        let mut chat = chat_with_draft("first");
        let action = handle_chat_key(&mut chat, &model, ctrl('j'), VIEWPORT);
        assert_eq!(action, None);
        assert_eq!(chat.composer.text(), "first\n");
    }

    #[test]
    fn a_failed_send_restores_the_draft_and_states_the_failure() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let action = handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT);
        let Some(UiAction::Dispatch(command)) = action else {
            panic!("enter dispatches");
        };
        let op = OpId(Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        fold(
            &mut model,
            vec![Msg::OpResult {
                op,
                outcome: amux_ui::OpOutcome::Error {
                    error: amux_ui::OpError {
                        message: "input raced the session".to_string(),
                        auth_required: false,
                    },
                },
            }],
        );
        chat.reconcile(&model);
        assert_eq!(chat.composer.text(), "add retry", "draft resurfaced (C5)");
        assert_eq!(chat.send_failure(), Some("input raced the session"));

        // The next keypress dismisses the stated failure.
        handle_chat_key(&mut chat, &model, press(KeyCode::End), VIEWPORT);
        assert_eq!(chat.send_failure(), None);
    }

    #[test]
    fn a_failed_send_never_clobbers_text_typed_in_the_meantime() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let Some(UiAction::Dispatch(command)) =
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT)
        else {
            panic!("enter dispatches");
        };
        let op = OpId(Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        chat.composer.insert_str("newer thought");
        fold(
            &mut model,
            vec![Msg::OpResult {
                op,
                outcome: amux_ui::OpOutcome::Error {
                    error: amux_ui::OpError {
                        message: "transport lost".to_string(),
                        auth_required: false,
                    },
                },
            }],
        );
        chat.reconcile(&model);
        assert_eq!(chat.composer.text(), "newer thought");
        assert_eq!(chat.send_failure(), Some("transport lost"));
    }

    #[test]
    fn a_successful_send_leaves_no_failure_and_no_pending_watch() {
        let mut model = idle_model();
        let mut chat = chat_with_draft("add retry");
        let Some(UiAction::Dispatch(command)) =
            handle_chat_key(&mut chat, &model, press(KeyCode::Enter), VIEWPORT)
        else {
            panic!("enter dispatches");
        };
        let op = OpId(Uuid::from_u128(99));
        chat.note_dispatched(op, &command);
        fold(&mut model, vec![Msg::Command { op, command }]);
        fold(
            &mut model,
            vec![Msg::OpResult {
                op,
                outcome: amux_ui::OpOutcome::InputSent,
            }],
        );
        chat.reconcile(&model);
        assert_eq!(chat.send_failure(), None);
        assert!(chat.composer.is_empty());
    }
}

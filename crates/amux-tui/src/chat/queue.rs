//! Terminal gestures and strip text over the shared queue.

use amux_ui::{AgentId, Command, Draft, Model, OpId, OpOutcome, QueueCommand, QueueDelivery};
use ratatui::text::Line;

use crate::composer::Composer;
use crate::render::{Theme, clip_to_width};
use crate::view::UiAction;

pub(super) fn key(
    model: &Model,
    agent: AgentId,
    composer: &mut Composer,
    review: Option<&amux_ui::review::Review>,
) -> Option<UiAction> {
    let queued = model.queued(agent);
    if queued.is_some_and(|q| matches!(q.delivery, QueueDelivery::Sending { .. })) {
        return None;
    }
    if composer.is_empty() {
        return queued.map(|_| UiAction::Dispatch(Command::Queue(QueueCommand::Cancel { agent })));
    }
    if queued.is_none() && !amux_ui::queue::can_hold(model, agent) {
        return None;
    }
    let (text, attachments) = composer.export(review);
    let draft = Draft { text, attachments };
    let command = if queued.is_some() {
        QueueCommand::Replace { agent, draft }
    } else {
        QueueCommand::Hold { agent, draft }
    };
    composer.clear_for_send();
    Some(UiAction::Dispatch(Command::Queue(command)))
}

pub(super) fn reconcile(
    model: &Model,
    pending: &mut Option<OpId>,
    composer: &mut Composer,
    failure: &mut Option<String>,
) {
    let Some(finished) = pending.and_then(|op| model.finished_op(op)) else {
        return;
    };
    match &finished.outcome {
        OpOutcome::QueueCancelled { draft } => composer.restore_queued(draft),
        OpOutcome::Error { error } => {
            *failure = Some(error.message());
            if composer.is_empty() {
                composer.restore_sent();
            }
        }
        _ => {}
    }
    *pending = None;
}

pub(super) fn strip(
    model: &Model,
    agent: AgentId,
    activity: Option<Line<'static>>,
    theme: Theme,
    width: usize,
    composer_available: bool,
) -> Vec<Line<'static>> {
    let mut rows: Vec<_> = activity.into_iter().collect();
    if let Some(queue) = model.queued(agent) {
        let state = match &queue.delivery {
            QueueDelivery::Held if composer_available => "queued · tab edit or replace".to_string(),
            QueueDelivery::Held => "queued".to_string(),
            QueueDelivery::Sending { .. } => "sending queued message".to_string(),
            QueueDelivery::Failed { error } => format!(
                "queued · {} · retry on reconnect{}",
                error.message(),
                if composer_available {
                    " · tab edit"
                } else {
                    ""
                }
            ),
        };
        let text = queue
            .draft
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        rows.push(Line::styled(
            clip_to_width(&format!("  {state} · {text}"), width).to_owned(),
            theme.muted(),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use uuid::Uuid;

    use super::*;
    use crate::chat::handle_chat_key;
    use crate::fixtures::{NamedState, fixture};
    use crate::render::{FrameContext, render};

    /// The actual Tab handler holds, replaces and unqueues; the production
    /// renderer shows the queued words beside the still-available interrupt.
    #[test]
    fn queue_tui_tab_roundtrip_and_rendered_strip() {
        for state in [NamedState::ClaudeWorking, NamedState::CodexWorking] {
            let mut fixture = fixture(state);
            let agent = fixture.view.chat.as_ref().unwrap().agent;
            for (n, words) in [(1, "check the queue"), (2, "run the tests next")] {
                let chat = fixture.view.chat.as_mut().unwrap();
                chat.composer_mut().insert_str(words);
                let action = handle_chat_key(
                    chat,
                    &fixture.model,
                    KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                    (120, 40),
                    fixture.now,
                );
                let Some(UiAction::Dispatch(command)) = action else {
                    panic!("Tab dispatches the shared queue");
                };
                let op = OpId(Uuid::from_u128(n));
                chat.note_dispatched(op, &command);
                assert!(
                    amux_ui::update(&mut fixture.model, amux_ui::Msg::Command { op, command })
                        .is_empty()
                );
                chat.reconcile(&fixture.model);
                assert!(chat.composer_mut().is_empty());
                assert_eq!(fixture.model.queued(agent).unwrap().draft.text, words);
            }
            let ctx = FrameContext {
                viewport: (120, 40),
                theme: Theme::default(),
                now: fixture.now,
            };
            let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
            terminal
                .draw(|frame| render(&fixture.model, &fixture.view, &ctx, frame))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let mut text = String::new();
            for y in 0..40 {
                for x in 0..120 {
                    text.push_str(buffer[(x, y)].symbol());
                }
                text.push('\n');
            }
            assert!(text.contains("queued · tab edit or replace · run the tests next"));
            assert!(text.contains("interrupt"));
            println!("{} queued terminal frame:\n{text}", state.name());
            let chat = fixture.view.chat.as_mut().unwrap();
            let Some(UiAction::Dispatch(command)) = handle_chat_key(
                chat,
                &fixture.model,
                KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                (120, 40),
                fixture.now,
            ) else {
                panic!("empty Tab cancels into the composer");
            };
            let op = OpId(Uuid::from_u128(3));
            chat.note_dispatched(op, &command);
            amux_ui::update(&mut fixture.model, amux_ui::Msg::Command { op, command });
            chat.reconcile(&fixture.model);
            assert!(fixture.model.queued(agent).is_none());
            assert_eq!(chat.composer_mut().text(), "run the tests next");
        }
    }
}

//! One open chat as the phone holds it: the session driver, its changes
//! gathered for the host's next turn, and the views the host asks for by
//! row key.

use std::sync::Arc;

use tokio::task::JoinHandle;
use ui_runtime::{InputError, PageError, Session};
use ui_state::{InputOutcome, InputState, Key, SessionState};
use ui_view::{AskCard, ChatOptions, Pick, Row, Strip};
use wire::{BlobRef, send_input_response};

use crate::coalesce::{Coalescer, WakeFn};
use crate::values::{
    ActOutcome, ChatChanges, ChatFrame, Draft, PageOutcome, RowOptions, SendOutcome,
};

/// An open chat. Row ids are item keys and never move, so the host's id
/// sequence only ever grows at its two edges: newer keys above the newest
/// it holds, older ones below its oldest, until a reload.
pub struct Chat {
    id: u64,
    session: Session,
    changes: Coalescer<Key>,
    watcher: JoinHandle<()>,
    clock: Arc<dyn client::Clock>,
}

impl Chat {
    pub(crate) fn start(
        id: u64,
        session: Session,
        wake: WakeFn,
        clock: Arc<dyn client::Clock>,
    ) -> Arc<Chat> {
        Arc::new_cyclic(|chat: &std::sync::Weak<Chat>| {
            let mut changed = session.changed();
            let watched = chat.clone();
            let watcher = tokio::spawn(async move {
                // The opening's rows are the host's first read, not news.
                changed.borrow_and_update();
                while changed.changed().await.is_ok() {
                    let Some(chat) = watched.upgrade() else {
                        return;
                    };
                    let changes = chat.session.take_changes();
                    chat.changes
                        .push(changes.keys, changes.reloaded, changes.session);
                }
            });
            Chat {
                id,
                session,
                changes: Coalescer::new(wake),
                watcher,
                clock,
            }
        })
    }

    /// The id the host's wake names this chat by.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The held window's keys, oldest first.
    pub fn keys(&self) -> Vec<Key> {
        self.session.state().transcript().keys().cloned().collect()
    }

    /// Keys newer than `newest`, oldest first; None when `newest` is not
    /// held, and the host reads [`Chat::keys`] again.
    pub fn keys_above(&self, newest: &str) -> Option<Vec<Key>> {
        let state = self.session.state();
        let transcript = state.transcript();
        let order = transcript.get(newest)?.item.order;
        Some(
            transcript
                .range(order.saturating_add(1)..=u64::MAX)
                .map(|held| held.item.key.clone())
                .collect(),
        )
    }

    /// Keys older than `oldest`, oldest first; None when `oldest` is not
    /// held.
    pub fn keys_below(&self, oldest: &str) -> Option<Vec<Key>> {
        let state = self.session.state();
        let transcript = state.transcript();
        let order = transcript.get(oldest)?.item.order;
        if order == 0 {
            return Some(Vec::new());
        }
        Some(
            transcript
                .range(0..=order - 1)
                .map(|held| held.item.key.clone())
                .collect(),
        )
    }

    /// Rows for these keys, in order, skipping keys not held.
    pub fn rows_for(&self, keys: &[Key], options: &RowOptions) -> Vec<Row> {
        let expanded = options.expanded();
        let opts = ChatOptions {
            tools: options.tool_rows(&expanded),
        };
        ui_view::chat_rows_for(&self.session.state(), keys, &opts)
    }

    pub fn ask_card(&self) -> Option<AskCard> {
        ui_view::ask_card(&self.session.state())
    }

    pub fn strip(&self) -> Strip {
        ui_view::session_strip(&self.session.state())
    }

    pub fn frame(&self) -> ChatFrame {
        let ended = self.session.ended().map(|error| error.to_string());
        let state = self.session.state();
        frame(&state, self.clock.now_ms(), ended)
    }

    /// Everything that changed since the last take; the next change wakes
    /// the host again.
    pub fn take_changes(&self) -> ChatChanges {
        let batch = self.changes.take();
        ChatChanges {
            keys: batch.keys,
            reloaded: batch.reloaded,
            session: batch.other,
        }
    }

    pub async fn send(&self, draft: &Draft) -> SendOutcome {
        let attachments = draft.attachments.iter().map(|a| a.to_wire()).collect();
        let sent = self.session.send_prompt(&draft.text, attachments).await;
        let state = self
            .session
            .state()
            .input_state(&sent.id)
            .unwrap_or_else(|| outcome_state(&sent.outcome));
        SendOutcome {
            input_id: sent.id,
            state,
        }
    }

    /// Answers the head ask with its choice at `index`; `note` goes back
    /// with a choice that takes one.
    pub async fn answer_choice(&self, ask_key: &str, index: usize, note: &str) -> ActOutcome {
        let input = {
            let Some(card) = self.card_for(ask_key) else {
                return moved_on();
            };
            let Some(choice) = card.choices.get(index) else {
                return ActOutcome::Rejected(format!("the ask has no choice {index}"));
            };
            ui_view::answer_input(&card, &choice.answer, note)
        };
        self.answer(input).await
    }

    /// Answers the head question ask: one pick per question, in order.
    pub async fn answer_questions(&self, ask_key: &str, picks: &[Pick], note: &str) -> ActOutcome {
        let input = {
            let Some(card) = self.card_for(ask_key) else {
                return moved_on();
            };
            let answer = ui_view::question_answer(&card, picks, note);
            ui_view::answer_input(&card, &answer, note)
        };
        self.answer(input).await
    }

    fn card_for(&self, ask_key: &str) -> Option<AskCard> {
        self.ask_card().filter(|card| card.key == ask_key)
    }

    async fn answer(&self, input: Option<wire::Input>) -> ActOutcome {
        match input {
            Some(input) => acted(self.session.answer(input).await),
            None => ActOutcome::Rejected("this agent does not take that answer".into()),
        }
    }

    pub async fn withdraw(&self, input_id: &[u8]) -> ActOutcome {
        acted(self.session.withdraw(input_id).await)
    }

    pub async fn send_now(&self, input_id: &[u8]) -> ActOutcome {
        acted(self.session.send_now(input_id).await)
    }

    pub async fn interrupt(&self) -> ActOutcome {
        acted(self.session.interrupt().await)
    }

    /// Sends a not-confirmed input again under a new id, forgetting the
    /// old one.
    pub async fn resend(&self, input_id: &[u8]) -> Option<SendOutcome> {
        let mut input = self
            .session
            .state()
            .inputs()
            .get(input_id)
            .map(|sent| sent.input.clone())?;
        self.session.discard(input_id);
        input.input_id.clear();
        let sent = self.session.send(input).await;
        let state = self
            .session
            .state()
            .input_state(&sent.id)
            .unwrap_or_else(|| outcome_state(&sent.outcome));
        Some(SendOutcome {
            input_id: sent.id,
            state,
        })
    }

    pub fn discard(&self, input_id: &[u8]) {
        self.session.discard(input_id);
    }

    /// The exited composer's one tap: the draft is the new incarnation's
    /// first prompt.
    pub async fn resume_with(&self, draft: &Draft) -> ActOutcome {
        let kind = self.session.state().kind();
        let attachments = draft.attachments.iter().map(|a| a.to_wire()).collect();
        let Some(input) = ui_runtime::inputs::prompt(kind, &draft.text, attachments) else {
            return ActOutcome::Rejected("unsupported".into());
        };
        match self.session.resume_with(input).await {
            Ok(_) => ActOutcome::Done,
            Err(error) if error.is_transport() => ActOutcome::NotConfirmed,
            Err(error) => ActOutcome::Failed(error.to_string()),
        }
    }

    pub async fn page_older(&self, n: u32) -> PageOutcome {
        match self.session.page_older(n).await {
            Ok(count) => PageOutcome::Arrived(count as u32),
            Err(PageError::OriginUnreachable) => PageOutcome::OriginUnreachable,
            Err(PageError::Rpc(error)) => PageOutcome::Failed(error.to_string()),
        }
    }

    /// Stores bytes to attach to a prompt.
    pub async fn put_blob(
        &self,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<BlobRef, client::RpcError> {
        self.session.put_blob(name, mime, bytes).await
    }

    /// An attachment's bytes once fetched; asking starts the fetch, and the
    /// rows that show it change when it lands.
    pub fn blob(&self, hash: &[u8]) -> Option<Arc<[u8]>> {
        self.session.blob(hash)
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

pub(crate) fn frame(state: &SessionState, now_ms: i64, ended: Option<String>) -> ChatFrame {
    let agent = state.agent();
    ChatFrame {
        agent: ui_state::agent_key(agent),
        name: agent.name.clone().unwrap_or_default(),
        kind: state.kind(),
        phase: state.phase(),
        composer: ui_view::composer(state, now_ms),
        waiting: ui_view::waiting(state),
        connection: state.connection(),
        caught_up: state.caught_up(),
        has_older: state.transcript().has_older(),
        queue: ui_view::queue_rows(state),
        outbox: ui_view::outbox_rows(state),
        ended,
    }
}

fn outcome_state(outcome: &InputOutcome) -> InputState {
    match outcome {
        InputOutcome::Lost => InputState::Uncertain,
        InputOutcome::Reply(reply) => match &reply.of {
            Some(send_input_response::Of::Accepted(accepted)) if accepted.queued => {
                InputState::Queued
            }
            Some(send_input_response::Of::Accepted(_)) => InputState::Settled,
            Some(send_input_response::Of::Rejected(rejected)) => {
                InputState::Rejected(rejected.reason.clone())
            }
            None => InputState::Uncertain,
        },
    }
}

fn acted(result: Result<(), InputError>) -> ActOutcome {
    match result {
        Ok(()) => ActOutcome::Done,
        Err(InputError::Rejected(reason)) => ActOutcome::Rejected(reason),
        Err(InputError::Uncertain) => ActOutcome::NotConfirmed,
    }
}

fn moved_on() -> ActOutcome {
    ActOutcome::Rejected("that ask is no longer the one waiting".into())
}

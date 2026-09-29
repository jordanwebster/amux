//! One open chat as the phone holds it: the session driver, its changes
//! gathered for the host's next turn, and the views the host asks for by
//! row key.

use std::sync::{Arc, OnceLock};

use tokio::task::JoinHandle;
use ui_runtime::{InputError, PageError, Session};
use ui_state::{InputOutcome, InputState, Key, SessionState};
use ui_view::{AskCard, ChatOptions, Pick, Row, SettingChange, SettingsView, Strip};
use wire::{BlobRef, send_input_response};

use crate::coalesce::{Coalescer, WakeFn};
use crate::values::{
    ActOutcome, ChatChanges, ChatFrame, Draft, FrozenReview, PageOutcome, RowOptions, SendOutcome,
};

/// An open chat. Row ids are item keys and never move, so the host's id
/// sequence changes only at its two edges: newer keys above the newest it
/// holds, older ones below its oldest, and while the reader follows, the
/// oldest dropped as the window trims to its cap; until a reload.
pub struct Chat {
    id: u64,
    session: Session,
    changes: Coalescer<Key>,
    watcher: OnceLock<JoinHandle<()>>,
    clock: Arc<dyn client::Clock>,
}

impl Chat {
    pub(crate) fn start(
        id: u64,
        session: Session,
        wake: WakeFn,
        clock: Arc<dyn client::Clock>,
    ) -> Arc<Chat> {
        // Subscribed here, the receiver has seen the opening: its rows are
        // the host's first read, not news. Anything after is, even a change
        // that lands before the watcher first runs, so the watcher must not
        // mark the channel seen itself.
        let mut changed = session.changed();
        let chat = Arc::new(Chat {
            id,
            session,
            changes: Coalescer::new(wake),
            watcher: OnceLock::new(),
            clock,
        });
        // Spawned once the chat exists, so every change it sees finds the
        // chat; it ends when the chat is dropped or the session ends.
        let watched = Arc::downgrade(&chat);
        let watcher = tokio::spawn(async move {
            while changed.changed().await.is_ok() {
                let Some(chat) = watched.upgrade() else {
                    return;
                };
                let changes = chat.session.take_changes();
                chat.changes
                    .push(changes.keys, changes.reloaded, changes.session);
            }
        });
        let _ = chat.watcher.set(watcher);
        chat
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

    /// The oldest key the window holds: while the reader follows, the
    /// window drops its oldest rows as new ones arrive, and the host drops
    /// the keys before this one.
    pub fn oldest_key(&self) -> Option<Key> {
        let state = self.session.state();
        state
            .transcript()
            .iter()
            .next()
            .map(|held| held.item.key.clone())
    }

    /// Where the reader is: at the newest row, or in history.
    pub fn follow(&self, following: bool) {
        self.session.follow(following);
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

    /// What the agent offers to change, with the current values marked.
    pub fn settings(&self) -> SettingsView {
        ui_view::settings(&self.session.state())
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
        self.answer_with(ask_key, index, note, None).await
    }

    /// Submits the head form ask with its choice at `index` and the
    /// person's field values as a JSON object.
    pub async fn answer_form(&self, ask_key: &str, index: usize, content_json: &str) -> ActOutcome {
        self.answer_with(ask_key, index, "", Some(content_json.as_bytes().to_vec()))
            .await
    }

    async fn answer_with(
        &self,
        ask_key: &str,
        index: usize,
        note: &str,
        content: Option<Vec<u8>>,
    ) -> ActOutcome {
        let input = {
            let Some(card) = self.card_for(ask_key) else {
                return moved_on();
            };
            let Some(choice) = card.choices.get(index) else {
                return ActOutcome::Rejected(format!("the ask has no choice {index}"));
            };
            let answer = match content {
                Some(content) => ui_view::with_form_content(&choice.answer, content),
                None => choice.answer.clone(),
            };
            ui_view::answer_input(&card, &answer, note)
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

    /// Sends a pick from the settings view; the strip shows the new value
    /// once the agent reports it.
    pub async fn change_setting(&self, change: &SettingChange) -> ActOutcome {
        let kind = self.session.state().kind();
        match ui_view::setting_input(kind, change) {
            Some(input) => acted(self.session.answer(input).await),
            None => ActOutcome::Rejected("this agent does not take that change".into()),
        }
    }

    /// A queued or sent prompt as the draft it came from: its words and
    /// every attachment, whole, for a withdraw or an edit to put back.
    pub fn draft_of(&self, input_id: &[u8]) -> Option<Draft> {
        let state = self.session.state();
        if let Some(row) = state
            .queue()
            .into_iter()
            .find(|row| row.entry.input_id == input_id)
        {
            return Some(Draft::from_queued(row.entry));
        }
        state.inputs().get(input_id).and_then(Draft::from_sent)
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

    /// The agent's working-tree diff and its patch, frozen for a review.
    pub async fn review(&self) -> Result<FrozenReview, client::RpcError> {
        let (diff, patch) = self.session.working_tree_review().await?;
        Ok(FrozenReview { diff, patch })
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
        if let Some(watcher) = self.watcher.get() {
            watcher.abort();
        }
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
        arrivals_held: state.arrivals_held(),
        queue: ui_view::queue_rows(state),
        outbox: ui_view::outbox_rows(state),
        ask_input: ui_view::ask_card(state)
            .and_then(|card| state.answering(&card.key).map(|sent| sent.id.clone())),
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

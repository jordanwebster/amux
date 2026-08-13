use std::sync::Arc;

use tokio::sync::mpsc;

use crate::approval::{ApprovalResponse, RequestId};
use crate::config::{self, TurnConfig, TurnInput};
use crate::dispatch::{ServerInner, ThreadRegistration};
use crate::error::Error;
use crate::notification::ThreadEvent;
use crate::thread_event_stream::ThreadEventStream;
use crate::turn_stream::TurnStream;
use crate::types::{
    DynamicToolCallResponse, ReviewStartResponse, ReviewTarget, ThreadInfo, ThreadSessionInfo,
    TurnStartResponse, TurnSteerResponse,
};

// ── Thread ───────────────────────────────────────────────────────

/// Handle to a single conversation thread on the codex app-server.
///
/// Created via [`Codex::start_thread()`] or [`Codex::resume_thread()`].
/// Cheap to clone (internally `Arc`-wrapped). Clones share the event channel.
#[derive(Clone)]
pub struct Thread {
    pub(crate) inner: Arc<ThreadInner>,
}

pub(crate) struct ThreadInner {
    pub server: Arc<ServerInner>,
    pub thread_id: String,
    pub session: ThreadSessionInfo,
    pub registration: Arc<ThreadRegistration>,
}

struct TurnSlot {
    thread_inner: Arc<ThreadInner>,
    rx: Option<mpsc::Receiver<ThreadEvent>>,
}

impl TurnSlot {
    async fn acquire(thread_inner: Arc<ThreadInner>) -> Result<Self, Error> {
        let rx = thread_inner
            .registration
            .take_receiver()
            .await
            .ok_or(Error::TurnActive)?;
        Ok(Self {
            thread_inner,
            rx: Some(rx),
        })
    }

    fn into_stream(mut self, initial_turn_id: String) -> TurnStream {
        TurnStream::new(
            self.rx.take().expect("turn slot missing receiver"),
            self.thread_inner.clone(),
            initial_turn_id,
        )
    }
}

impl Drop for TurnSlot {
    fn drop(&mut self) {
        if let Some(rx) = self.rx.take() {
            restore_event_receiver(self.thread_inner.clone(), rx);
        }
    }
}

impl Thread {
    pub(crate) fn new(
        server: Arc<ServerInner>,
        session: ThreadSessionInfo,
        registration: Arc<ThreadRegistration>,
    ) -> Self {
        let thread_id = session.thread.id.clone();
        Self {
            inner: Arc::new(ThreadInner {
                server,
                thread_id,
                session,
                registration,
            }),
        }
    }

    /// The thread ID.
    pub fn id(&self) -> &str {
        &self.inner.thread_id
    }

    /// The thread info returned at creation/resume time.
    pub fn info(&self) -> &ThreadInfo {
        &self.inner.session.thread
    }

    /// Thread/session metadata returned by thread/start/resume/fork.
    pub fn session_info(&self) -> &ThreadSessionInfo {
        &self.inner.session
    }

    /// Take the continuous receiver for all notifications and server requests
    /// routed to this thread. Only one event consumer may be active at a time.
    pub async fn events(&self) -> Result<ThreadEventStream, Error> {
        let rx = self
            .inner
            .registration
            .take_receiver()
            .await
            .ok_or(Error::TurnActive)?;
        Ok(ThreadEventStream::new(rx, self.inner.clone()))
    }

    // ── Turn management ──────────────────────────────────────────

    /// Start a turn with default config.
    pub async fn turn(&self, input: impl Into<TurnInput>) -> Result<TurnStream, Error> {
        self.turn_with(input, TurnConfig::default()).await
    }

    /// Start a turn without taking ownership of the thread event receiver.
    ///
    /// This is intended for callers that continuously consume [`Self::events`].
    pub async fn start_turn(&self, input: impl Into<TurnInput>) -> Result<String, Error> {
        self.start_turn_with(input, TurnConfig::default()).await
    }

    /// Start a turn with explicit config without creating a [`TurnStream`].
    pub async fn start_turn_with(
        &self,
        input: impl Into<TurnInput>,
        turn_config: TurnConfig,
    ) -> Result<String, Error> {
        let input_value = config::turn_input_to_value(input.into());
        let mut params = config::turn_config_to_params(&turn_config);
        params.insert("threadId".into(), serde_json::json!(self.inner.thread_id));
        params.insert("input".into(), input_value);

        let start: TurnStartResponse = self
            .inner
            .server
            .request("turn/start", serde_json::Value::Object(params))
            .await?;
        Ok(start.turn.id)
    }

    /// Start a turn with explicit config.
    pub async fn turn_with(
        &self,
        input: impl Into<TurnInput>,
        turn_config: TurnConfig,
    ) -> Result<TurnStream, Error> {
        let turn_slot = TurnSlot::acquire(self.inner.clone()).await?;

        let input_value = config::turn_input_to_value(input.into());
        let mut params = config::turn_config_to_params(&turn_config);
        params.insert("threadId".into(), serde_json::json!(self.inner.thread_id));
        params.insert("input".into(), input_value);

        let start: TurnStartResponse = self
            .inner
            .server
            .request("turn/start", serde_json::Value::Object(params))
            .await?;

        Ok(turn_slot.into_stream(start.turn.id))
    }

    /// Steer an active turn with additional input.
    pub async fn steer(&self, turn_id: &str, input: impl Into<TurnInput>) -> Result<String, Error> {
        let input_value = config::turn_input_to_value(input.into());
        let response: TurnSteerResponse = self
            .inner
            .server
            .request(
                "turn/steer",
                serde_json::json!({
                    "threadId": self.inner.thread_id,
                    "expectedTurnId": turn_id,
                    "input": input_value,
                }),
            )
            .await?;
        Ok(response.turn_id)
    }

    /// Interrupt an active turn.
    pub async fn interrupt(&self, turn_id: &str) -> Result<(), Error> {
        self.inner
            .server
            .request_unit(
                "turn/interrupt",
                serde_json::json!({
                    "threadId": self.inner.thread_id,
                    "turnId": turn_id,
                }),
            )
            .await
    }

    /// Start a review turn.
    pub async fn review(&self, target: ReviewTarget) -> Result<TurnStream, Error> {
        let turn_slot = TurnSlot::acquire(self.inner.clone()).await?;

        let review: ReviewStartResponse = self
            .inner
            .server
            .request(
                "review/start",
                serde_json::json!({
                    "threadId": self.inner.thread_id,
                    "target": review_target_to_wire(target),
                }),
            )
            .await?;

        if review.review_thread_id != self.inner.thread_id {
            return Err(Error::Internal(anyhow::anyhow!(
                "detached review is not yet supported by Thread::review"
            )));
        }

        Ok(turn_slot.into_stream(review.turn.id))
    }

    /// Compact the thread's context.
    ///
    /// The app-server models compaction as an asynchronous turn-like flow, so
    /// callers must continue consuming events until the compact turn completes.
    pub async fn compact(&self) -> Result<TurnStream, Error> {
        let turn_slot = TurnSlot::acquire(self.inner.clone()).await?;

        self.inner
            .server
            .request_unit(
                "thread/compact/start",
                serde_json::json!({ "threadId": self.inner.thread_id }),
            )
            .await?;

        Ok(turn_slot.into_stream(String::new()))
    }

    /// Roll back the last N turns.
    pub async fn rollback(&self, num_turns: u32) -> Result<ThreadInfo, Error> {
        let response: crate::types::ThreadReadResponse = self
            .inner
            .server
            .request(
                "thread/rollback",
                serde_json::json!({
                    "threadId": self.inner.thread_id,
                    "numTurns": num_turns,
                }),
            )
            .await?;
        Ok(response.thread)
    }

    // ── Manual approval response ─────────────────────────────────

    /// Respond to an approval request manually.
    /// Only needed when no `ApprovalHandler` is configured on `CodexConfig`.
    pub async fn respond_approval(
        &self,
        request_id: RequestId,
        response: ApprovalResponse,
    ) -> Result<(), Error> {
        self.inner
            .server
            .respond(request_id, response.to_wire_value())
            .await
    }

    /// Respond to a JSON-RPC request with a raw JSON value.
    /// Used for structured responses like question answers that don't
    /// map to ApprovalResponse variants.
    pub async fn respond_raw(
        &self,
        request_id: RequestId,
        value: serde_json::Value,
    ) -> Result<(), Error> {
        self.inner.server.respond(request_id, value).await
    }

    /// Respond to an `item/tool/call` request surfaced by the turn stream.
    pub async fn respond_tool_call(
        &self,
        request_id: RequestId,
        response: DynamicToolCallResponse,
    ) -> Result<(), Error> {
        self.inner
            .server
            .respond(request_id, serde_json::to_value(response)?)
            .await
    }
}

fn review_target_to_wire(target: ReviewTarget) -> serde_json::Value {
    match target {
        ReviewTarget::UncommittedChanges => serde_json::json!({ "type": "uncommittedChanges" }),
        ReviewTarget::BaseBranch { branch } => {
            serde_json::json!({ "type": "baseBranch", "branch": branch })
        }
        ReviewTarget::Commit { sha, title } => {
            serde_json::json!({ "type": "commit", "sha": sha, "title": title })
        }
        ReviewTarget::Custom { instructions } => {
            serde_json::json!({ "type": "custom", "instructions": instructions })
        }
    }
}

pub(crate) fn restore_event_receiver(
    thread_inner: Arc<ThreadInner>,
    rx: mpsc::Receiver<ThreadEvent>,
) {
    let rx = match thread_inner.registration.try_restore_receiver(rx) {
        Ok(()) => return,
        Err(rx) => rx,
    };

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            thread_inner.registration.restore_receiver(rx).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;

    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::{ApprovalPolicy, ApprovalsReviewer, ReadOnlyAccess, SandboxPolicy};
    use crate::notification::ServerNotification;
    use crate::notification::TurnEvent;
    use crate::types::{ThreadSessionInfo, ThreadStatus};

    fn test_server() -> Arc<ServerInner> {
        let (stdin_tx, stdin_rx) = mpsc::channel(1);
        let (global_notif_tx, _global_notif_rx) = mpsc::channel::<ServerNotification>(1);
        drop(stdin_rx);

        Arc::new(ServerInner {
            stdin_tx,
            pending_requests: Mutex::new(HashMap::new()),
            thread_channels: Mutex::new(HashMap::new()),
            approval_handler: None,
            global_notif_tx,
            init_result: std::sync::OnceLock::new(),
            request_counter: AtomicU64::new(1),
            cancel: CancellationToken::new(),
            child_waiter: Mutex::new(None),
        })
    }

    fn test_thread(server: Arc<ServerInner>) -> Thread {
        Thread::new(
            server,
            ThreadSessionInfo {
                thread: ThreadInfo {
                    id: "thread-1".into(),
                    preview: String::new(),
                    ephemeral: false,
                    model_provider: String::new(),
                    created_at: 0,
                    updated_at: 0,
                    status: ThreadStatus::Idle,
                    path: None,
                    cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
                    cli_version: String::new(),
                    source: crate::types::SessionSource::default(),
                    agent_nickname: None,
                    agent_role: None,
                    git_info: None,
                    name: None,
                    turns: Vec::new(),
                },
                model: String::new(),
                model_provider: String::new(),
                service_tier: None,
                cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
                approval_policy: ApprovalPolicy::OnRequest,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox: SandboxPolicy::ReadOnly {
                    access: ReadOnlyAccess::FullAccess,
                    network_access: false,
                },
                reasoning_effort: None,
            },
            crate::dispatch::ThreadRegistration::new(),
        )
    }

    #[tokio::test]
    async fn failed_turn_start_restores_receiver() {
        let thread = test_thread(test_server());

        let err = thread.turn("hello").await.unwrap_err();
        assert!(matches!(err, Error::TransportClosed));

        let err = thread.turn("hello again").await.unwrap_err();
        assert!(matches!(err, Error::TransportClosed));
    }

    #[tokio::test]
    async fn failed_review_restores_receiver() {
        let thread = test_thread(test_server());

        let err = thread
            .review(ReviewTarget::UncommittedChanges)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::TransportClosed));

        let err = thread
            .review(ReviewTarget::UncommittedChanges)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::TransportClosed));
    }

    #[tokio::test]
    async fn failed_compact_restores_receiver() {
        let thread = test_thread(test_server());

        let err = thread.compact().await.unwrap_err();
        assert!(matches!(err, Error::TransportClosed));

        let err = thread.compact().await.unwrap_err();
        assert!(matches!(err, Error::TransportClosed));
    }

    #[tokio::test]
    async fn continuous_events_span_multiple_turns() {
        let thread = test_thread(test_server());
        let registration = thread.inner.registration.clone();
        let mut events = thread.events().await.unwrap();
        for (method, turn_id) in [("turn/started", "turn-1"), ("turn/completed", "turn-2")] {
            assert!(registration.send(ThreadEvent {
                method: method.into(),
                params: serde_json::json!({
                    "threadId": "thread-1",
                    "turnId": turn_id,
                }),
                turn_id: Some(turn_id.into()),
                event: TurnEvent::Warning {
                    message: method.into(),
                },
            }));
        }
        assert_eq!(events.next().await.unwrap().unwrap().method, "turn/started");
        assert_eq!(
            events.next().await.unwrap().unwrap().method,
            "turn/completed"
        );
    }
}

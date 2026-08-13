use std::fmt;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::notification::{ThreadEvent, TurnEvent};
use crate::thread::ThreadInner;
use crate::thread::restore_event_receiver;
use crate::types::Turn;

/// An owned stream of turn events for a single turn.
///
/// Created by [`Thread::turn()`](crate::Thread::turn) or
/// [`Thread::turn_with()`](crate::Thread::turn_with).
///
/// Call [`next()`](Self::next) to receive events. The stream ends when a
/// `TurnCompleted` event is received or when the channel closes.
///
/// On `Drop`, the internal receiver is returned to the thread so that
/// subsequent `turn()` calls can create a new `TurnStream`.
#[must_use = "turn events must be consumed; drop the stream to release the turn"]
pub struct TurnStream {
    rx: Option<mpsc::Receiver<ThreadEvent>>,
    thread_inner: Arc<ThreadInner>,
    turn_id: String,
    completed_turn: Option<Turn>,
    done: bool,
}

impl fmt::Debug for TurnStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnStream")
            .field("turn_id", &self.turn_id)
            .field("done", &self.done)
            .field("has_completed_turn", &self.completed_turn.is_some())
            .finish_non_exhaustive()
    }
}

impl TurnStream {
    pub(crate) fn new(
        rx: mpsc::Receiver<ThreadEvent>,
        thread_inner: Arc<ThreadInner>,
        initial_turn_id: String,
    ) -> Self {
        Self {
            rx: Some(rx),
            thread_inner,
            turn_id: initial_turn_id,
            completed_turn: None,
            done: false,
        }
    }

    /// Receive the next turn event.
    ///
    /// Returns `None` when the turn is complete — either because a
    /// `TurnCompleted` event was received (check [`completed_turn()`](Self::completed_turn))
    /// or because the channel closed.
    pub async fn next(&mut self) -> Option<TurnEvent> {
        if self.done {
            return None;
        }
        let rx = self.rx.as_mut()?;
        loop {
            match rx.recv().await {
                Some(ThreadEvent::Turn(event)) => {
                    // Compaction starts without a turn ID, so capture its first start event.
                    if self.turn_id.is_empty()
                        && let TurnEvent::TurnStarted { ref turn } = event
                    {
                        self.turn_id = turn.id.clone();
                    }
                    // A dropped stream can leave its terminal event queued on the shared
                    // thread receiver. Ignore it instead of ending the next turn's stream.
                    if let TurnEvent::TurnCompleted { ref turn } = event {
                        if turn.id != self.turn_id {
                            continue;
                        }
                        self.completed_turn = Some(turn.clone());
                        self.done = true;
                        return Some(event);
                    }
                    return Some(event);
                }
                None => {
                    // Channel closed (EOF)
                    self.done = true;
                    return None;
                }
            }
        }
    }

    /// Returns the completed `Turn` if the turn finished with a `TurnCompleted` event.
    pub fn completed_turn(&self) -> Option<&Turn> {
        self.completed_turn.as_ref()
    }

    /// The turn ID (populated after `TurnStarted` is received).
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }
}

impl Drop for TurnStream {
    fn drop(&mut self) {
        // Return the receiver to ThreadInner so the next turn() can take it.
        if let Some(rx) = self.rx.take() {
            restore_event_receiver(self.thread_inner.clone(), rx);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicU64;

    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::{ApprovalPolicy, ApprovalsReviewer, ReadOnlyAccess, SandboxPolicy};
    use crate::dispatch::ServerInner;
    use crate::notification::ServerNotification;
    use crate::types::{SessionSource, ThreadInfo, ThreadSessionInfo, ThreadStatus, TurnStatus};

    fn turn(id: &str) -> Turn {
        Turn {
            id: id.to_owned(),
            items: Vec::new(),
            status: TurnStatus::Completed,
            error: None,
        }
    }

    fn thread_inner() -> Arc<ThreadInner> {
        let (stdin_tx, _stdin_rx) = mpsc::channel(1);
        let (global_notif_tx, _global_notif_rx) = mpsc::channel::<ServerNotification>(1);
        let server = Arc::new(ServerInner {
            stdin_tx,
            pending_requests: Mutex::new(HashMap::new()),
            thread_channels: Mutex::new(HashMap::new()),
            approval_handler: None,
            global_notif_tx,
            init_result: OnceLock::new(),
            request_counter: AtomicU64::new(1),
            cancel: CancellationToken::new(),
        });
        Arc::new(ThreadInner {
            server,
            thread_id: "thread-1".into(),
            session: ThreadSessionInfo {
                thread: ThreadInfo {
                    id: "thread-1".into(),
                    preview: String::new(),
                    ephemeral: false,
                    model_provider: String::new(),
                    created_at: 0,
                    updated_at: 0,
                    status: ThreadStatus::Idle,
                    path: None,
                    cwd: ".".into(),
                    cli_version: String::new(),
                    source: SessionSource::default(),
                    agent_nickname: None,
                    agent_role: None,
                    git_info: None,
                    name: None,
                    turns: Vec::new(),
                },
                model: String::new(),
                model_provider: String::new(),
                service_tier: None,
                cwd: ".".into(),
                approval_policy: ApprovalPolicy::OnRequest,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox: SandboxPolicy::ReadOnly {
                    access: ReadOnlyAccess::FullAccess,
                    network_access: false,
                },
                reasoning_effort: None,
            },
            event_rx: Mutex::new(None),
        })
    }

    #[tokio::test]
    async fn ignores_completion_from_an_earlier_turn() {
        let (tx, rx) = mpsc::channel(3);
        tx.send(ThreadEvent::Turn(TurnEvent::TurnCompleted {
            turn: turn("turn-old"),
        }))
        .await
        .unwrap();
        tx.send(ThreadEvent::Turn(TurnEvent::Warning {
            message: "current turn is still running".into(),
        }))
        .await
        .unwrap();
        tx.send(ThreadEvent::Turn(TurnEvent::TurnCompleted {
            turn: turn("turn-current"),
        }))
        .await
        .unwrap();

        let mut stream = TurnStream::new(rx, thread_inner(), "turn-current".into());

        assert!(matches!(
            stream.next().await,
            Some(TurnEvent::Warning { .. })
        ));
        assert!(matches!(
            stream.next().await,
            Some(TurnEvent::TurnCompleted { ref turn }) if turn.id == "turn-current"
        ));
        assert_eq!(
            stream.completed_turn().map(|turn| turn.id.as_str()),
            Some("turn-current")
        );
    }
}

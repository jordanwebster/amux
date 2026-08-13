use std::fmt;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::dispatch::ThreadChannelState;
use crate::error::Error;
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
    /// Returns `Ok(None)` when the turn is complete — either because a
    /// `TurnCompleted` event was received (check [`completed_turn()`](Self::completed_turn))
    /// or because the channel closed. Returns an explicit error if dispatch
    /// overflowed the bounded per-thread queue.
    pub async fn next(&mut self) -> Result<Option<TurnEvent>, Error> {
        if self.done {
            return Ok(None);
        }
        let registration = self.thread_inner.registration.clone();
        let Some(rx) = self.rx.as_mut() else {
            return Ok(None);
        };
        loop {
            let state_changed = registration.state_changed().notified();
            tokio::pin!(state_changed);
            match registration.state() {
                ThreadChannelState::Overflow => {
                    self.done = true;
                    return Err(Error::ThreadQueueOverflow(
                        self.thread_inner.thread_id.clone(),
                    ));
                }
                ThreadChannelState::Closed => {
                    self.done = true;
                    return Ok(None);
                }
                ThreadChannelState::Open => {}
            }

            let received = tokio::select! {
                biased;
                _ = &mut state_changed => continue,
                event = rx.recv() => event,
            };
            match received {
                Some(ThreadEvent::Turn { turn_id, event }) => {
                    // Compaction starts without a turn ID, so capture its first start event.
                    if self.turn_id.is_empty()
                        && let TurnEvent::TurnStarted { ref turn } = event
                    {
                        self.turn_id = turn.id.clone();
                    }
                    // A dropped stream can leave any turn-scoped event queued on the shared
                    // receiver. Ignore all events from other turns.
                    if let Some(event_turn_id) = turn_id
                        && !self.turn_id.is_empty()
                        && event_turn_id != self.turn_id
                    {
                        continue;
                    }
                    if let TurnEvent::TurnCompleted { ref turn } = event {
                        self.completed_turn = Some(turn.clone());
                        self.done = true;
                        return Ok(Some(event));
                    }
                    return Ok(Some(event));
                }
                None => {
                    // Channel closed (EOF)
                    self.done = true;
                    return Ok(None);
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

    fn thread_inner() -> (Arc<ThreadInner>, Arc<crate::dispatch::ThreadRegistration>) {
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
            child_waiter: Mutex::new(None),
        });
        let registration = crate::dispatch::ThreadRegistration::new();
        let inner = Arc::new(ThreadInner {
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
            registration: registration.clone(),
        });
        (inner, registration)
    }

    #[tokio::test]
    async fn ignores_completion_from_an_earlier_turn() {
        let (thread_inner, registration) = thread_inner();
        assert!(registration.send(ThreadEvent::Turn {
            turn_id: Some("turn-old".into()),
            event: TurnEvent::AgentMessageDelta {
                item_id: "old-item".into(),
                delta: "stale".into(),
            },
        }));
        assert!(registration.send(ThreadEvent::Turn {
            turn_id: Some("turn-old".into()),
            event: TurnEvent::TurnCompleted {
                turn: turn("turn-old"),
            },
        }));
        assert!(registration.send(ThreadEvent::Turn {
            turn_id: Some("turn-current".into()),
            event: TurnEvent::Warning {
                message: "current turn is still running".into(),
            },
        }));
        assert!(registration.send(ThreadEvent::Turn {
            turn_id: Some("turn-current".into()),
            event: TurnEvent::TurnCompleted {
                turn: turn("turn-current"),
            },
        }));
        let rx = registration.take_receiver().await.unwrap();
        let mut stream = TurnStream::new(rx, thread_inner, "turn-current".into());

        assert!(matches!(
            stream.next().await,
            Ok(Some(TurnEvent::Warning { .. }))
        ));
        assert!(matches!(
            stream.next().await,
            Ok(Some(TurnEvent::TurnCompleted { ref turn })) if turn.id == "turn-current"
        ));
        assert_eq!(
            stream.completed_turn().map(|turn| turn.id.as_str()),
            Some("turn-current")
        );
    }

    #[tokio::test]
    async fn reports_thread_queue_overflow_instead_of_hanging() {
        let (thread_inner, registration) = thread_inner();
        for index in 0..256 {
            assert!(registration.send(ThreadEvent::Turn {
                turn_id: Some("turn-current".into()),
                event: TurnEvent::AgentMessageDelta {
                    item_id: format!("item-{index}"),
                    delta: "queued".into(),
                },
            }));
        }
        assert!(!registration.send(ThreadEvent::Turn {
            turn_id: Some("turn-current".into()),
            event: TurnEvent::TurnCompleted {
                turn: turn("turn-current"),
            },
        }));
        let rx = registration.take_receiver().await.unwrap();
        let mut stream = TurnStream::new(rx, thread_inner, "turn-current".into());

        assert!(matches!(
            stream.next().await,
            Err(Error::ThreadQueueOverflow(ref thread_id)) if thread_id == "thread-1"
        ));
        assert!(matches!(stream.next().await, Ok(None)));
    }
}

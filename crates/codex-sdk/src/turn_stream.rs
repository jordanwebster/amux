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
        match rx.recv().await {
            Some(ThreadEvent::Turn(event)) => {
                // Capture turn ID from TurnStarted
                if let TurnEvent::TurnStarted { ref turn } = event {
                    self.turn_id = turn.id.clone();
                }
                // Capture completed turn and signal done
                if let TurnEvent::TurnCompleted { ref turn } = event {
                    self.completed_turn = Some(turn.clone());
                    self.done = true;
                    return Some(event);
                }
                Some(event)
            }
            None => {
                // Channel closed (EOF)
                self.done = true;
                None
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

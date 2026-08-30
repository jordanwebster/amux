use crate::{ApprovalResponse, Error, RequestId, Thread, ThreadEventStream, TurnInput};

/// Codex's opaque identifier for one turn in a thread.
pub type TurnId = String;

/// One owned event stream and the control handle for its Codex thread.
pub struct Session {
    pub events: ThreadEventStream,
    pub control: ThreadControl,
}

/// Opens a thread's event stream and pairs it with its restricted control handle.
pub async fn open(thread: Thread) -> Result<Session, Error> {
    let events = thread.events().await?;
    Ok(Session {
        events,
        control: ThreadControl { thread },
    })
}

/// The operations a session host may perform on a Codex thread.
#[derive(Clone)]
pub struct ThreadControl {
    thread: Thread,
}

impl ThreadControl {
    pub async fn user_turn(&self, input: TurnInput) -> Result<TurnId, Error> {
        self.thread.start_turn(input).await
    }

    pub async fn empty_turn(&self) -> Result<TurnId, Error> {
        self.thread.start_empty_turn().await
    }

    pub async fn steer(&self, turn: &TurnId, input: TurnInput) -> Result<TurnId, Error> {
        self.thread.steer(turn, input).await
    }

    pub async fn interrupt(&self, turn: &TurnId) -> Result<(), Error> {
        self.thread.interrupt(turn).await
    }

    pub async fn approve(
        &self,
        request: RequestId,
        decision: ApprovalResponse,
    ) -> Result<(), Error> {
        self.thread.respond_approval(request, decision).await
    }

    pub async fn inject(&self, items: Vec<serde_json::Value>) -> Result<(), Error> {
        self.thread.inject_items(items).await
    }

    pub fn thread_id(&self) -> &str {
        self.thread.id()
    }
}

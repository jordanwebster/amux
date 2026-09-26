//! Inputs and agent messages.
//!
//! A person's input is relayed to the agent over its control socket and
//! the interpreter's verdict comes back as the answer; the daemon answers
//! only for an agent that has exited. An agent message goes through the
//! recipient's lane: one message at a time, deduped against the
//! recipient's items by envelope id, and accepted only once the item the
//! recipient wrote on accepting it has committed. A retry therefore always
//! arrives after the original's answer, so that one store lookup is the
//! whole dedupe. A parent's message to its exited child resumes the child
//! with the message as the new incarnation's first input.

use std::sync::Arc;
use std::time::Duration;

use store::{AgentKey, Store as _, StoreError};
use tokio::sync::oneshot;
use uuid::Uuid;
use wire::{
    AgentSender, Envelope, EnvelopeKind, ErrorCode, Human, Input, Lifecycle, Rejected,
    SendInputRequest, SendInputResponse, SendMessageResponse, Sender, input, send_input_response,
    sender,
};

use crate::runtime::{AgentId, ProfileRuntime, RegistryError};

/// The agent has exited; the composer offers Resume.
pub const EXITED: &str = "exited";
/// The agent has decided to exit; the daemon treats it as exited.
pub const EXITING: &str = "exiting";

/// How long an input waits for the agent to be connected.
const CONNECT_PATIENCE: Duration = Duration::from_secs(5);
/// How long an input waits for the interpreter's verdict, which it gives at
/// once; past this the connection is taken to be lost.
const REPLY_PATIENCE: Duration = Duration::from_secs(30);
/// How long an accepted message waits for its acceptance item to commit.
const ACCEPT_PATIENCE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("the request carries no input")]
    NoInput,
    #[error("the agent lives on another host")]
    OtherHost,
    #[error("the agent is not connected")]
    Unavailable,
    #[error("the agent's answer was lost; the input may or may not have arrived")]
    Lost,
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("the recipient's incarnation the message was for has ended")]
    Stale,
    #[error("resuming the agent: {0}")]
    Resume(Box<RegistryError>),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl RelayError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent => ErrorCode::NotFound,
            Self::NoInput => ErrorCode::InvalidArgument,
            Self::OtherHost => ErrorCode::Unimplemented,
            Self::Unavailable => ErrorCode::Unavailable,
            Self::Lost => ErrorCode::Aborted,
            Self::Rejected(_) | Self::Stale => ErrorCode::FailedPrecondition,
            Self::Resume(_) | Self::Store(_) => ErrorCode::Internal,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

pub(crate) fn rejected(reason: &str) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Rejected(Rejected {
            reason: reason.to_owned(),
        })),
    }
}

fn rejection(verdict: &SendInputResponse) -> Option<&str> {
    match &verdict.of {
        Some(send_input_response::Of::Rejected(rejected)) => Some(&rejected.reason),
        _ => None,
    }
}

/// The envelope id of an outbox row: the same for every re-send of one
/// row, so the receiver's lookup by envelope id catches a duplicate.
pub(crate) fn delivery_envelope_id(delivery: &store::Delivery) -> Vec<u8> {
    let mut name = delivery.child_id.clone();
    name.extend(delivery.incarnation.to_be_bytes());
    name.extend(delivery.kind.to_be_bytes());
    name.extend(delivery.turn_id.to_be_bytes());
    Uuid::new_v5(&Uuid::NAMESPACE_OID, &name)
        .as_bytes()
        .to_vec()
}

impl ProfileRuntime {
    /// An own agent's id from request bytes: not found, or on another host.
    async fn own_agent(&self, agent_id: &[u8]) -> Result<AgentId, RelayError> {
        let id = Uuid::from_slice(agent_id).map_err(|_| RelayError::NoAgent)?;
        let store = self.store.lock().await;
        if store.agent(&self.key(id))?.is_some() {
            return Ok(id);
        }
        if store
            .agents()?
            .iter()
            .any(|row| row.agent.agent == agent_id)
        {
            return Err(RelayError::OtherHost);
        }
        Err(RelayError::NoAgent)
    }

    /// Relays a person's input and returns the interpreter's verdict. An
    /// exited agent is answered here; one that answers "exiting" is from
    /// then on treated as leaving, so a resume waits for it.
    pub async fn send_input(
        &self,
        request: &SendInputRequest,
    ) -> Result<SendInputResponse, RelayError> {
        let input = request.input.clone().ok_or(RelayError::NoInput)?;
        let id = self.own_agent(&request.agent_id).await?;
        let row = self
            .store
            .lock()
            .await
            .agent(&self.key(id))?
            .ok_or(RelayError::NoAgent)?;
        if row.lifecycle == Lifecycle::Exited as i32 {
            return Ok(rejected(EXITED));
        }
        let verdict = self.relay(id, input).await?;
        if rejection(&verdict) == Some(EXITING) {
            self.mark_exiting(id);
        }
        Ok(verdict)
    }

    /// Hands an input to the agent over its control connection and waits
    /// for the verdict.
    pub(crate) async fn relay(
        &self,
        id: AgentId,
        input: Input,
    ) -> Result<SendInputResponse, RelayError> {
        let Some(handle) = self.handle(id) else {
            return Ok(rejected(EXITED));
        };
        let (tx, rx) = oneshot::channel();
        let input_id = input.input_id.clone();
        handle.expect_reply(input_id.clone(), tx);
        let frame = wire::CtlFrame {
            of: Some(wire::ctl_frame::Of::Input(input)),
        };
        let connect_by = tokio::time::Instant::now() + CONNECT_PATIENCE;
        loop {
            if let Some(ctl) = handle.ctl.lock().await.as_mut() {
                if agent_dir::write_frame(ctl, &frame).await.is_err() {
                    handle.forget_reply(&input_id);
                    return Err(RelayError::Lost);
                }
                break;
            }
            if *handle.exited.borrow() {
                handle.forget_reply(&input_id);
                return Ok(rejected(EXITED));
            }
            if tokio::time::Instant::now() >= connect_by {
                handle.forget_reply(&input_id);
                return Err(RelayError::Unavailable);
            }
            tokio::time::sleep(POLL).await;
        }
        match tokio::time::timeout(REPLY_PATIENCE, rx).await {
            Ok(Ok(verdict)) => Ok(verdict),
            // The connection ended before the answer: the watcher dropped
            // every waiting reply.
            Ok(Err(_)) => Err(RelayError::Lost),
            Err(_) => {
                handle.forget_reply(&input_id);
                Err(RelayError::Lost)
            }
        }
    }

    /// Sends an agent message. `caller` is the agent whose tools socket the
    /// call came in on; the envelope's sender is set from it, never taken
    /// from the request. Answers once the recipient has accepted.
    pub async fn send_message(
        &self,
        mut envelope: Envelope,
        caller: Option<AgentId>,
    ) -> Result<SendMessageResponse, RelayError> {
        let to = envelope.to.clone().ok_or(RelayError::NoAgent)?;
        if to.host_id != self.host().as_bytes() {
            // Forwarding over the peer link is not built yet.
            return Err(RelayError::OtherHost);
        }
        let id = self.own_agent(&to.agent_id).await?;
        if envelope.id.is_empty() {
            envelope.id = Uuid::new_v4().as_bytes().to_vec();
        }
        if envelope.kind == EnvelopeKind::Unspecified as i32 {
            envelope.kind = EnvelopeKind::Message as i32;
        }
        let (from, is_parent) = {
            let store = self.store.lock().await;
            let recipient = store.agent(&self.key(id))?.ok_or(RelayError::NoAgent)?;
            match caller {
                None => (
                    Sender {
                        value: Some(sender::Value::Human(Human {})),
                    },
                    false,
                ),
                Some(caller) => {
                    let row = store.agent(&self.key(caller))?;
                    let is_parent = recipient.parent.as_ref() == Some(&self.key(caller));
                    (self.agent_sender(caller, row.as_ref()), is_parent)
                }
            }
        };
        envelope.from = Some(from);
        let envelope_id = envelope.id.clone();
        self.deliver(id, envelope, is_parent, None).await?;
        Ok(SendMessageResponse { envelope_id })
    }

    pub(crate) fn agent_sender(&self, id: AgentId, row: Option<&store::AgentRow>) -> Sender {
        Sender {
            value: Some(sender::Value::Agent(AgentSender {
                agent_id: id.as_bytes().to_vec(),
                host_id: self.host().as_bytes().to_vec(),
                name: row.and_then(|row| row.name.clone()).unwrap_or_default(),
                kind: row.map(|row| row.kind.clone()).unwrap_or_default(),
            })),
        }
    }

    /// The recipient's lane: one message at a time, answered only once
    /// accepted and committed. `resume` lets an exited recipient be resumed
    /// with the message, which only its parent may do. A message for one
    /// `incarnation` of the recipient is stale once another has begun.
    pub(crate) async fn deliver(
        &self,
        id: AgentId,
        envelope: Envelope,
        resume: bool,
        incarnation: Option<u32>,
    ) -> Result<(), RelayError> {
        let lane = self.lane(id);
        let _turn = lane.lock().await;
        // Until every journal has been read once after start, the lookup
        // below could miss an acceptance item still in a journal.
        let mut read = self.journals_read.subscribe();
        let _ = read.wait_for(|read| *read).await;

        let key = self.key(id);
        let row = {
            let store = self.store.lock().await;
            if store.item_by_input(&key, &envelope.id)?.is_some() {
                return Ok(());
            }
            store.agent(&key)?.ok_or(RelayError::NoAgent)?
        };
        // Checked again under the lane: whoever held it may have resumed
        // the recipient while this message waited.
        if incarnation.is_some_and(|incarnation| incarnation != row.incarnation) {
            return Err(RelayError::Stale);
        }
        let input = Input {
            input_id: envelope.id.clone(),
            of: Some(input::Of::AgentMessage(envelope.clone())),
        };
        if row.lifecycle == Lifecycle::Exited as i32 {
            return if resume {
                self.resume_with(id, input).await
            } else {
                Err(RelayError::Rejected(EXITED.to_owned()))
            };
        }
        let verdict = self.relay(id, input.clone()).await?;
        match rejection(&verdict) {
            None => self.accepted(id, &key, &envelope.id).await,
            Some(EXITING) => {
                self.mark_exiting(id);
                if resume {
                    self.resume_with(id, input).await
                } else {
                    Err(RelayError::Rejected(EXITED.to_owned()))
                }
            }
            Some(reason) => Err(RelayError::Rejected(reason.to_owned())),
        }
    }

    async fn resume_with(&self, id: AgentId, input: Input) -> Result<(), RelayError> {
        let envelope_id = input.input_id.clone();
        self.resume(id, Some(input))
            .await
            .map_err(|error| RelayError::Resume(Box::new(error)))?;
        self.accepted(id, &self.key(id), &envelope_id).await
    }

    /// Waits for the item the recipient wrote on accepting the message.
    async fn accepted(
        &self,
        id: AgentId,
        key: &AgentKey,
        envelope_id: &[u8],
    ) -> Result<(), RelayError> {
        let mut commits = self.commits.subscribe();
        let deadline = tokio::time::Instant::now() + ACCEPT_PATIENCE;
        loop {
            commits.borrow_and_update();
            if self
                .store
                .lock()
                .await
                .item_by_input(key, envelope_id)?
                .is_some()
            {
                return Ok(());
            }
            let exited = self.handle(id).is_none_or(|handle| *handle.exited.borrow());
            if exited {
                // Everything the process wrote was ingested before its exit
                // was recorded, and the item was not in it.
                return Err(RelayError::Lost);
            }
            match tokio::time::timeout_at(deadline, commits.changed()).await {
                Ok(Ok(())) => {}
                _ => return Err(RelayError::Lost),
            }
        }
    }

    fn lane(&self, id: AgentId) -> Arc<tokio::sync::Mutex<()>> {
        self.lanes.lock().unwrap().entry(id).or_default().clone()
    }
}

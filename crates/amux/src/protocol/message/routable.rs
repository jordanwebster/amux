use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::common::{
    CreateAgentRequest, ProtocolError, RenameAgentRequest, SubscribeQuery, SubscriptionCloseReason,
    SubscriptionId, TerminalSize,
};

/// Messages that carry src/dst routing information and can be forwarded across hops.
/// agent_id is Uuid — callers must resolve names to UUIDs before constructing.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutableMessage {
    SubscribeRaw {
        agent_id: Uuid,
        /// Terminal dimensions for PTY resize. None means don't resize.
        terminal_size: Option<TerminalSize>,
    },
    SubscribeRawResult {
        subscription_id: SubscriptionId,
        lease_ms: u64,
        error: Option<ProtocolError>,
    },
    SubscribeStructured {
        agent_id: Uuid,
        query: Option<SubscribeQuery>,
    },
    SubscribeStructuredResult {
        subscription_id: SubscriptionId,
        /// Current sequence number at subscribe time. Clients use this as
        /// their initial seq when no StructuredOutput messages have arrived.
        seq: u64,
        /// Structured I/O contract for this subscription, if the session exposes one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured_protocol: Option<String>,
        lease_ms: u64,
        error: Option<ProtocolError>,
    },
    ExtendSubscription {
        subscription_id: SubscriptionId,
    },
    ExtendSubscriptionResult {
        subscription_id: SubscriptionId,
        lease_ms: u64,
        error: Option<ProtocolError>,
    },
    Unsubscribe {
        subscription_id: SubscriptionId,
    },
    CreateAgent(CreateAgentRequest),
    CreateAgentResult {
        agent_id: Uuid,
        error: Option<ProtocolError>,
    },
    RenameAgent(RenameAgentRequest),
    RenameAgentResult {
        agent_id: Uuid,
        error: Option<ProtocolError>,
    },
    DeleteAgent {
        agent_id: Uuid,
    },
    DeleteAgentResult {
        agent_id: Uuid,
        error: Option<ProtocolError>,
    },
    RawInput {
        agent_id: Uuid,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    RawOutput {
        subscription_id: SubscriptionId,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    StructuredOutput {
        subscription_id: SubscriptionId,
        seq: u64,
        payload: Value,
    },
    StructuredInput {
        agent_id: Uuid,
        seq: u64,
        payload: Value,
    },
    StructuredInputResult {
        agent_id: Uuid,
        error: Option<ProtocolError>,
    },
    SubscriptionClosed {
        subscription_id: SubscriptionId,
        reason: SubscriptionCloseReason,
    },
    /// Sent by an intermediate hop when it cannot forward a message.
    /// Analogous to ICMP Destination Unreachable — the hop doesn't inspect the
    /// payload, it just reports that delivery failed. The original sender
    /// matches on `request_id` to fail the pending request.
    Unreachable {
        request_id: u64,
    },
    UnsupportedMessage,
    InvalidMessage,
    #[serde(other)]
    Unknown,
}

impl RoutableMessage {
    /// Human-readable label for this variant, used in error messages.
    pub fn type_label(&self) -> &'static str {
        match self {
            RoutableMessage::SubscribeRaw { .. } => "SubscribeRaw",
            RoutableMessage::SubscribeStructured { .. } => "SubscribeStructured",
            RoutableMessage::SubscribeRawResult { .. } => "SubscribeRawResult",
            RoutableMessage::SubscribeStructuredResult { .. } => "SubscribeStructuredResult",
            RoutableMessage::ExtendSubscription { .. } => "ExtendSubscription",
            RoutableMessage::ExtendSubscriptionResult { .. } => "ExtendSubscriptionResult",
            RoutableMessage::Unsubscribe { .. } => "Unsubscribe",
            RoutableMessage::CreateAgent(_) => "CreateAgent",
            RoutableMessage::CreateAgentResult { .. } => "CreateAgentResult",
            RoutableMessage::RenameAgent(_) => "RenameAgent",
            RoutableMessage::RenameAgentResult { .. } => "RenameAgentResult",
            RoutableMessage::DeleteAgent { .. } => "DeleteAgent",
            RoutableMessage::DeleteAgentResult { .. } => "DeleteAgentResult",
            RoutableMessage::RawInput { .. } => "RawInput",
            RoutableMessage::RawOutput { .. } => "RawOutput",
            RoutableMessage::StructuredOutput { .. } => "StructuredOutput",
            RoutableMessage::StructuredInput { .. } => "StructuredInput",
            RoutableMessage::StructuredInputResult { .. } => "StructuredInputResult",
            RoutableMessage::SubscriptionClosed { .. } => "SubscriptionClosed",
            RoutableMessage::Unreachable { .. } => "Unreachable",
            RoutableMessage::UnsupportedMessage => "UnsupportedMessage",
            RoutableMessage::InvalidMessage => "InvalidMessage",
            RoutableMessage::Unknown => "Unknown",
        }
    }

    /// Encode routable message to bytes using MessagePack (named/map format)
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// Decode routable message from bytes using MessagePack
    pub fn decode(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }
}

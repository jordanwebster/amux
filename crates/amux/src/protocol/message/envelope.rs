use serde::{Deserialize, Serialize};

use super::command::Command;
use super::direct::DirectMessage;
use super::routable::RoutableMessage;
use crate::protocol::route::Route;

/// All protocol messages between client and server
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    Routable {
        src: Route,
        dst: Route,
        request_id: u64,
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },
    Direct {
        message: DirectMessage,
    },
    Command {
        command: Command,
    },
    #[serde(other)]
    Unknown,
}

impl Message {
    /// Convenience constructor for Routable messages.
    ///
    /// Encodes the RoutableMessage into the opaque payload. Panics if encoding
    /// fails; use [`Message::try_routable`] at sites where the payload may
    /// carry untrusted `serde_json::Value` data (e.g. StructuredInput/Output).
    pub fn routable(src: Route, dst: Route, request_id: u64, message: &RoutableMessage) -> Self {
        Self::try_routable(src, dst, request_id, message)
            .expect("RoutableMessage encode cannot fail")
    }

    /// Fallible constructor for Routable messages.
    ///
    /// MessagePack encoding of `serde_json::Value` payloads can fail for
    /// non-string map keys or invalid floats, so callers handling user-supplied
    /// JSON should use this instead of [`Message::routable`].
    pub fn try_routable(
        src: Route,
        dst: Route,
        request_id: u64,
        message: &RoutableMessage,
    ) -> Result<Self, rmp_serde::encode::Error> {
        Ok(Message::Routable {
            src,
            dst,
            request_id,
            payload: message.encode()?,
        })
    }

    /// Short qualified label for this variant, for use in logs and error messages.
    /// Returns e.g. "Routable", "Direct::Reauth", "Command::Shutdown".
    pub fn type_label(&self) -> &'static str {
        match self {
            Message::Routable { .. } => "Routable",
            Message::Direct { message: d } => match d {
                DirectMessage::Reauth { .. } => "Direct::Reauth",
                DirectMessage::ReauthResult { .. } => "Direct::ReauthResult",
                DirectMessage::Heartbeat => "Direct::Heartbeat",
                DirectMessage::HeartbeatAck => "Direct::HeartbeatAck",
                DirectMessage::InitialSyncComplete => "Direct::InitialSyncComplete",
                DirectMessage::AnnounceAgent { .. } => "Direct::AnnounceAgent",
                DirectMessage::WithdrawAgent { .. } => "Direct::WithdrawAgent",
                DirectMessage::AnnounceHost { .. } => "Direct::AnnounceHost",
                DirectMessage::WithdrawHost { .. } => "Direct::WithdrawHost",
                DirectMessage::Unknown => "Direct::Unknown",
            },
            Message::Command { command: c } => match c {
                Command::ListAgents => "Command::ListAgents",
                Command::ListAgentsResult { .. } => "Command::ListAgentsResult",
                Command::ResolveAgent { .. } => "Command::ResolveAgent",
                Command::ResolveAgentResult { .. } => "Command::ResolveAgentResult",
                Command::Shutdown => "Command::Shutdown",
                Command::ShutdownNotification { .. } => "Command::ShutdownNotification",
                Command::Debug { .. } => "Command::Debug",
                Command::DebugResult { .. } => "Command::DebugResult",
                Command::ConnectToServer { .. } => "Command::ConnectToServer",
                Command::ConnectToServerResult { .. } => "Command::ConnectToServerResult",
                Command::HandleHook { .. } => "Command::HandleHook",
                Command::HandleHookResult { .. } => "Command::HandleHookResult",
                Command::Suspend => "Command::Suspend",
                Command::SuspendResult { .. } => "Command::SuspendResult",
                Command::Resume => "Command::Resume",
                Command::ResumeResult { .. } => "Command::ResumeResult",
                Command::Unknown => "Command::Unknown",
            },
            Message::Unknown => "Unknown",
        }
    }
}

impl Message {
    /// Encode message to bytes using MessagePack (named/map format for compatibility)
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(self)
    }

    /// Decode message from bytes using MessagePack
    pub fn decode(data: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(data)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use serde::Serialize;
    use uuid::Uuid;

    use super::super::common::{
        AgentType, RenameAgentRequest, SubscriptionCloseReason, SubscriptionId, TerminalSize,
    };
    use super::*;
    use crate::protocol::agent::Agent;
    use crate::protocol::link::Link;
    use crate::protocol::message::ProtocolError;

    // --- Core architectural pattern: opaque payload two-step encoding ---

    #[test]
    fn test_routable_message_standalone_encode_decode() {
        let subscription_id = SubscriptionId::random();
        let rm = RoutableMessage::RawOutput {
            subscription_id,
            data: b"hello".to_vec(),
        };
        let encoded = rm.encode().unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        let RoutableMessage::RawOutput {
            subscription_id: decoded_id,
            data,
        } = decoded
        else {
            panic!("Expected RawOutput");
        };
        assert_eq!(decoded_id, subscription_id);
        assert_eq!(data, b"hello");
    }

    #[test]
    fn test_subscription_variants_roundtrip() {
        let agent_id = Uuid::new_v4();
        let subscription_id = SubscriptionId::random();
        let variants = vec![
            RoutableMessage::SubscribeRaw {
                agent_id,
                terminal_size: Some(TerminalSize {
                    rows: 40,
                    cols: 120,
                }),
            },
            RoutableMessage::SubscribeRawResult {
                subscription_id,
                lease_ms: 30_000,
                error: None,
            },
            RoutableMessage::SubscribeStructured {
                agent_id,
                query: None,
            },
            RoutableMessage::SubscribeStructuredResult {
                subscription_id,
                seq: 42,
                structured_protocol: Some("claude_pty_v1".to_string()),
                lease_ms: 30_000,
                error: None,
            },
            RoutableMessage::ExtendSubscription { subscription_id },
            RoutableMessage::ExtendSubscriptionResult {
                subscription_id,
                lease_ms: 30_000,
                error: Some(ProtocolError::UnknownSubscription),
            },
            RoutableMessage::Unsubscribe { subscription_id },
            RoutableMessage::StructuredOutput {
                subscription_id,
                seq: 7,
                payload: serde_json::json!({"type": "event"}),
            },
            RoutableMessage::SubscriptionClosed {
                subscription_id,
                reason: SubscriptionCloseReason::LeaseExpired,
            },
        ];

        for variant in variants {
            let encoded = variant.encode().unwrap();
            let decoded = RoutableMessage::decode(&encoded).unwrap();
            assert_eq!(decoded.type_label(), variant.type_label());
        }
    }

    #[test]
    fn test_subscription_close_reason_roundtrip() {
        for reason in [
            SubscriptionCloseReason::SourceClosed,
            SubscriptionCloseReason::Unsubscribed,
            SubscriptionCloseReason::LeaseExpired,
        ] {
            let encoded = rmp_serde::to_vec_named(&reason).unwrap();
            let decoded: SubscriptionCloseReason = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(decoded, reason);
        }
    }

    #[test]
    fn test_opaque_payload_two_step_roundtrip() {
        // Core architectural invariant: RoutableMessage is encoded into an opaque
        // payload inside Message::Routable, allowing intermediate hops to forward
        // without deserializing the inner message.
        let agent_id = Uuid::new_v4();
        let rm = RoutableMessage::CreateAgentResult {
            agent_id,
            error: None,
        };
        let msg = Message::routable(
            Route::from_link(Link::new("src").unwrap()),
            Route::from_link(Link::new("dst").unwrap()),
            99,
            &rm,
        );
        let wire = msg.encode().unwrap();
        let decoded_msg = Message::decode(&wire).unwrap();
        let Message::Routable {
            payload,
            request_id,
            ..
        } = decoded_msg
        else {
            panic!("Expected Routable");
        };
        assert_eq!(request_id, 99);
        let decoded_rm = RoutableMessage::decode(&payload).unwrap();
        assert!(matches!(
            decoded_rm,
            RoutableMessage::CreateAgentResult { error: None, .. }
        ));
    }

    #[test]
    fn test_rename_agent_roundtrip() {
        let agent_id = Uuid::new_v4();
        let rm = RoutableMessage::RenameAgent(RenameAgentRequest {
            agent_id,
            name: "renamed".to_string(),
        });
        let encoded = rm.encode().unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        let RoutableMessage::RenameAgent(req) = decoded else {
            panic!("Expected RenameAgent");
        };
        assert_eq!(req.agent_id, agent_id);
        assert_eq!(req.name, "renamed");
    }

    #[test]
    fn test_delete_agent_roundtrip() {
        let agent_id = Uuid::new_v4();
        let rm = RoutableMessage::DeleteAgent { agent_id };
        let encoded = rm.encode().unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        assert!(matches!(
            decoded,
            RoutableMessage::DeleteAgent { agent_id: decoded_id } if decoded_id == agent_id
        ));
    }

    #[test]
    fn test_unreachable_roundtrip() {
        let rm = RoutableMessage::Unreachable { request_id: 42 };
        let encoded = rm.encode().unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        assert!(matches!(
            decoded,
            RoutableMessage::Unreachable { request_id: 42 }
        ));
    }

    // --- Behavioral method tests ---

    #[test]
    fn test_agent_is_remote_after_deserialization() {
        let info = Agent {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: Some("remote-agent".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::from_link(Link::new("host-a").unwrap()),
            agent_type: "claude".to_string(),
            structured_protocol: Some("claude_pty_v1".to_string()),
            readonly: false,
            args: vec![],
            created_at: Utc::now(),
        };
        let encoded = rmp_serde::to_vec_named(&info).unwrap();
        let decoded: Agent = rmp_serde::from_slice(&encoded).unwrap();
        assert!(decoded.is_remote());
    }

    #[test]
    fn test_create_agent_backward_compat_without_terminal_size() {
        // Old clients send CreateAgent without terminal_size field.
        // The #[serde(default)] ensures it defaults to None.
        #[derive(Serialize)]
        struct OldCreateAgentRequest {
            agent_id: Uuid,
            name: Option<String>,
            agent_type: AgentType,
            working_dir: PathBuf,
        }
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldRoutableMessage {
            CreateAgent(OldCreateAgentRequest),
        }
        let agent_id = Uuid::new_v4();
        let old_msg = OldRoutableMessage::CreateAgent(OldCreateAgentRequest {
            agent_id,
            name: Some("test".to_string()),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/tmp"),
        });
        let encoded = rmp_serde::to_vec_named(&old_msg).unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        let RoutableMessage::CreateAgent(req) = decoded else {
            panic!("Expected CreateAgent, got {:?}", decoded);
        };
        assert_eq!(req.agent_id, agent_id);
        assert_eq!(
            req.terminal_size, None,
            "missing terminal_size should default to None"
        );
    }

    // --- Forward compatibility contract tests ---
    // These verify that unknown message variants deserialize to explicit Unknown
    // cases instead of failing the entire frame decode.

    #[test]
    fn test_unknown_direct_variant_deserializes_to_unknown() {
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum FutureDirectMessage {
            Ping { seq: u64 },
        }
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum FutureMessage {
            Direct { message: FutureDirectMessage },
        }
        let future_msg = FutureMessage::Direct {
            message: FutureDirectMessage::Ping { seq: 42 },
        };
        let encoded = rmp_serde::to_vec_named(&future_msg).unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert!(matches!(
            decoded,
            Message::Direct {
                message: DirectMessage::Unknown
            }
        ));
    }

    #[test]
    fn test_unknown_routable_variant_deserializes_to_unknown() {
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum FutureRoutableMessage {
            FancyPing { seq: u64 },
        }

        let encoded =
            rmp_serde::to_vec_named(&FutureRoutableMessage::FancyPing { seq: 42 }).unwrap();
        let decoded = RoutableMessage::decode(&encoded).unwrap();
        assert!(matches!(decoded, RoutableMessage::Unknown));
    }

    #[test]
    fn test_direct_heartbeat_roundtrip_and_type_label() {
        let msg = Message::Direct {
            message: DirectMessage::Heartbeat,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert!(matches!(
            decoded,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));
        assert_eq!(msg.type_label(), "Direct::Heartbeat");
    }

    #[test]
    fn test_direct_initial_sync_complete_roundtrip_and_type_label() {
        let msg = Message::Direct {
            message: DirectMessage::InitialSyncComplete,
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert!(matches!(
            decoded,
            Message::Direct {
                message: DirectMessage::InitialSyncComplete
            }
        ));
        assert_eq!(msg.type_label(), "Direct::InitialSyncComplete");
    }

    #[test]
    fn test_unknown_top_level_variant_deserializes_to_unknown() {
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum FutureMessage {
            Stream { id: u64 },
        }
        let future_msg = FutureMessage::Stream { id: 1 };
        let encoded = rmp_serde::to_vec_named(&future_msg).unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        assert!(matches!(decoded, Message::Unknown));
    }
}

//! Claude Code hook types.
//!
//! Claude-specific hook parsing and metadata helpers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct ParsedClaudeHook {
    pub(crate) hook: ClaudeHook,
    pub(crate) raw: Value,
}

impl ParsedClaudeHook {
    #[cfg(test)]
    pub(crate) fn from_typed(hook: ClaudeHook) -> Self {
        let raw = serde_json::to_value(&hook).unwrap_or(Value::Null);
        Self { hook, raw }
    }

    pub(crate) fn parse_payload(payload: &[u8]) -> Result<Self, serde_json::Error> {
        let raw: Value = serde_json::from_slice(payload)?;
        let hook = serde_json::from_value(raw.clone())?;
        Ok(Self { hook, raw })
    }

    pub(crate) fn is_unknown(&self) -> bool {
        matches!(self.hook, ClaudeHook::Unknown)
    }

    pub(crate) fn is_session_end(&self) -> bool {
        matches!(self.hook, ClaudeHook::SessionEnd(_))
    }

    pub(crate) fn cwd(&self) -> Option<&str> {
        self.hook.cwd()
    }

    pub(crate) fn transcript_path(&self) -> Option<&str> {
        self.hook.transcript_path()
    }
}

/// Claude Code hook event — discriminated by `hook_event_name`.
///
/// All variants carry the same common fields (`session_id`, `transcript_path`,
/// `cwd`). The server uses the variant to decide what action to take (link
/// transcript, inject type field, etc.) but never inspects tool-specific or
/// hook-specific payload fields.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "hook_event_name")]
pub(crate) enum ClaudeHook {
    SessionStart(HookCommon),
    PermissionRequest(HookCommon),
    Stop(HookCommon),
    SessionEnd(HookCommon),
    Notification(HookCommon),
    #[serde(other)]
    Unknown,
}

impl ClaudeHook {
    pub(crate) fn session_id(&self) -> Option<Uuid> {
        self.common().map(|c| c.session_id)
    }

    pub(crate) fn cwd(&self) -> Option<&str> {
        self.common().map(|c| c.cwd.as_str())
    }

    pub(crate) fn transcript_path(&self) -> Option<&str> {
        self.common().map(|c| c.transcript_path.as_str())
    }

    fn common(&self) -> Option<&HookCommon> {
        match self {
            Self::SessionStart(c)
            | Self::PermissionRequest(c)
            | Self::Stop(c)
            | Self::SessionEnd(c)
            | Self::Notification(c) => Some(c),
            Self::Unknown => None,
        }
    }
}

impl std::fmt::Display for ClaudeHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeHook::SessionStart(c) => {
                write!(f, "session {} started", c.session_id)
            }
            ClaudeHook::PermissionRequest(c) => {
                write!(f, "session {} permission request", c.session_id)
            }
            ClaudeHook::Stop(c) => {
                write!(f, "session {} stopped", c.session_id)
            }
            ClaudeHook::SessionEnd(c) => {
                write!(f, "session {} ended", c.session_id)
            }
            ClaudeHook::Notification(c) => {
                write!(f, "session {} notification", c.session_id)
            }
            ClaudeHook::Unknown => write!(f, "unknown hook"),
        }
    }
}

/// Common fields present on all Claude Code hook events.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct HookCommon {
    pub(crate) session_id: Uuid,
    pub(crate) transcript_path: String,
    pub(crate) cwd: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hook_roundtrip() {
        let json = r#"{
            "hook_event_name": "SessionStart",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp"
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        assert!(matches!(hook, ClaudeHook::SessionStart(_)));
        assert_eq!(
            hook.session_id().unwrap(),
            "00000000-0000-0000-0000-000000000001"
                .parse::<Uuid>()
                .unwrap()
        );
        assert_eq!(hook.transcript_path().unwrap(), "/tmp/transcript.jsonl");
        assert_eq!(hook.cwd().unwrap(), "/tmp");
    }

    #[test]
    fn unknown_hook_deserializes() {
        let json = r#"{
            "hook_event_name": "SomeFutureEvent",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "transcript_path": "/tmp",
            "cwd": "/tmp"
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        assert!(matches!(hook, ClaudeHook::Unknown));
        assert!(hook.session_id().is_none());
    }

    #[test]
    fn extra_fields_are_ignored() {
        let json = r#"{
            "hook_event_name": "PermissionRequest",
            "session_id": "00000000-0000-0000-0000-000000000001",
            "transcript_path": "/tmp",
            "cwd": "/tmp",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"}
        }"#;
        let hook: ClaudeHook = serde_json::from_str(json).unwrap();
        assert!(matches!(hook, ClaudeHook::PermissionRequest(_)));
    }

    #[test]
    fn hook_message_msgpack_roundtrip() {
        use crate::protocol::message::{Command, HookProvider, Message};

        let agent_id = Uuid::new_v4();
        let hook = ParsedClaudeHook::from_typed(ClaudeHook::SessionStart(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        }));
        let msg = Message::Command {
            command: Command::HandleHook {
                agent_id,
                provider: HookProvider::Claude,
                payload: serde_json::to_vec(&hook.raw).unwrap(),
                external: true,
            },
        };
        let encoded = msg.encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();
        let Message::Command {
            command:
                Command::HandleHook {
                    agent_id: decoded_id,
                    provider,
                    payload,
                    external,
                },
        } = decoded
        else {
            panic!("Expected HandleHook, got {decoded:?}");
        };
        assert_eq!(decoded_id, agent_id);
        assert_eq!(provider, HookProvider::Claude);
        assert!(external);
        let parsed = ParsedClaudeHook::parse_payload(&payload).unwrap();
        assert!(matches!(parsed.hook, ClaudeHook::SessionStart(_)));
    }
}

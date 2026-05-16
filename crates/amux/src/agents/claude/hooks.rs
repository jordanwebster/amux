//! Claude Code hook types.
//!
//! Claude-specific hook parsing and metadata helpers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct ParsedClaudeHook {
    pub(crate) kind: ClaudeHookKind,
    pub(crate) common: Option<HookCommon>,
    pub(crate) raw: Value,
}

impl ParsedClaudeHook {
    #[cfg(test)]
    pub(crate) fn from_typed(kind: ClaudeHookKind, common: HookCommon) -> Self {
        let mut raw = serde_json::to_value(&common).unwrap_or(Value::Null);
        if let Some(obj) = raw.as_object_mut() {
            obj.insert(
                "hook_event_name".to_string(),
                Value::String(kind.as_event_name().to_string()),
            );
        }
        Self {
            kind,
            common: Some(common),
            raw,
        }
    }

    pub(crate) fn parse_payload(payload: &[u8]) -> Result<Self, serde_json::Error> {
        let raw: Value = serde_json::from_slice(payload)?;
        let kind =
            ClaudeHookKind::from_event_name(raw.get("hook_event_name").and_then(Value::as_str));
        let common = match kind {
            ClaudeHookKind::Unknown => None,
            _ => Some(serde_json::from_value(raw.clone())?),
        };
        Ok(Self { kind, common, raw })
    }

    pub(crate) fn is_unknown(&self) -> bool {
        self.kind == ClaudeHookKind::Unknown
    }

    pub(crate) fn is_session_end(&self) -> bool {
        self.kind == ClaudeHookKind::SessionEnd
    }

    pub(crate) fn session_id(&self) -> Option<Uuid> {
        self.common.as_ref().map(|c| c.session_id)
    }

    pub(crate) fn cwd(&self) -> Option<&str> {
        self.common.as_ref().map(|c| c.cwd.as_str())
    }

    pub(crate) fn transcript_path(&self) -> Option<&str> {
        self.common.as_ref().map(|c| c.transcript_path.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeHookKind {
    SessionStart,
    PermissionRequest,
    Stop,
    SessionEnd,
    Notification,
    Unknown,
}

impl ClaudeHookKind {
    fn from_event_name(event_name: Option<&str>) -> Self {
        match event_name {
            Some("SessionStart") => Self::SessionStart,
            Some("PermissionRequest") => Self::PermissionRequest,
            Some("Stop") => Self::Stop,
            Some("SessionEnd") => Self::SessionEnd,
            Some("Notification") => Self::Notification,
            _ => Self::Unknown,
        }
    }

    #[cfg(test)]
    fn as_event_name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::PermissionRequest => "PermissionRequest",
            Self::Stop => "Stop",
            Self::SessionEnd => "SessionEnd",
            Self::Notification => "Notification",
            Self::Unknown => "Unknown",
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
        let hook = ParsedClaudeHook::parse_payload(json.as_bytes()).unwrap();
        assert_eq!(hook.kind, ClaudeHookKind::SessionStart);
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
        let hook = ParsedClaudeHook::parse_payload(json.as_bytes()).unwrap();
        assert_eq!(hook.kind, ClaudeHookKind::Unknown);
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
        let hook = ParsedClaudeHook::parse_payload(json.as_bytes()).unwrap();
        assert_eq!(hook.kind, ClaudeHookKind::PermissionRequest);
    }
}

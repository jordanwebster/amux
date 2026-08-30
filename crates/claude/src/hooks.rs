//! External Claude Code hook payloads and their per-session socket transport.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::sdk::PermissionSuggestion;

const FORWARD_ENVELOPE_FIELD: &str = "amux_hook_forward_v1";

#[derive(Clone, Deserialize, Serialize)]
pub struct MessagingCredentials {
    pub socket_path: PathBuf,
    pub token: String,
}

impl std::fmt::Debug for MessagingCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessagingCredentials")
            .field("socket_path", &self.socket_path)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HookCommon {
    pub session_id: Uuid,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(skip)]
    pub messaging: Option<MessagingCredentials>,
    #[serde(skip)]
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub enum HookPayload {
    SessionStart(HookCommon),
    UserPromptSubmit(HookCommon),
    PermissionRequest {
        common: HookCommon,
        tool_name: String,
        tool_input: Value,
        suggestions: Vec<PermissionSuggestion>,
    },
    PreToolUse {
        common: HookCommon,
        tool_name: String,
        tool_input: Value,
    },
    PostToolUse {
        common: HookCommon,
        tool_name: String,
        tool_response: Value,
    },
    Notification {
        common: HookCommon,
        message: String,
    },
    Stop {
        common: HookCommon,
        permission_mode: String,
        last_assistant_message: Option<String>,
    },
    SessionEnd(HookCommon),
    Unknown {
        name: String,
        common: HookCommon,
        raw: Value,
    },
}

impl HookPayload {
    pub fn common(&self) -> &HookCommon {
        match self {
            Self::SessionStart(common)
            | Self::UserPromptSubmit(common)
            | Self::SessionEnd(common) => common,
            Self::PermissionRequest { common, .. }
            | Self::PreToolUse { common, .. }
            | Self::PostToolUse { common, .. }
            | Self::Notification { common, .. }
            | Self::Stop { common, .. }
            | Self::Unknown { common, .. } => common,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::SessionStart(_) => "SessionStart",
            Self::UserPromptSubmit(_) => "UserPromptSubmit",
            Self::PermissionRequest { .. } => "PermissionRequest",
            Self::PreToolUse { .. } => "PreToolUse",
            Self::PostToolUse { .. } => "PostToolUse",
            Self::Notification { .. } => "Notification",
            Self::Stop { .. } => "Stop",
            Self::SessionEnd(_) => "SessionEnd",
            Self::Unknown { name, .. } => name,
        }
    }

    pub fn raw(&self) -> &Value {
        match self {
            Self::Unknown { raw, .. } => raw,
            _ => &self.common().raw,
        }
    }

    fn common_mut(&mut self) -> &mut HookCommon {
        match self {
            Self::SessionStart(common)
            | Self::UserPromptSubmit(common)
            | Self::SessionEnd(common) => common,
            Self::PermissionRequest { common, .. }
            | Self::PreToolUse { common, .. }
            | Self::PostToolUse { common, .. }
            | Self::Notification { common, .. }
            | Self::Stop { common, .. }
            | Self::Unknown { common, .. } => common,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HookParseError {
    #[error("hook payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("hook payload omitted `{0}`")]
    Missing(&'static str),
    #[error("hook payload field `{0}` had the wrong shape")]
    Invalid(&'static str),
}

pub fn parse(payload: &[u8]) -> Result<HookPayload, HookParseError> {
    let raw: Value = serde_json::from_slice(payload)?;
    parse_value(raw)
}

fn parse_value(raw: Value) -> Result<HookPayload, HookParseError> {
    let name = string(&raw, "hook_event_name")?.to_string();
    let common = parse_common(&raw)?;
    let parsed = match name.as_str() {
        "SessionStart" => HookPayload::SessionStart(common),
        "UserPromptSubmit" => HookPayload::UserPromptSubmit(common),
        "PermissionRequest" => HookPayload::PermissionRequest {
            common,
            tool_name: string(&raw, "tool_name")?.to_string(),
            tool_input: raw.get("tool_input").cloned().unwrap_or(Value::Null),
            suggestions: raw
                .get("permission_suggestions")
                .or_else(|| raw.get("suggestions"))
                .cloned()
                .map(serde_json::from_value)
                .transpose()?
                .unwrap_or_default(),
        },
        "PreToolUse" => HookPayload::PreToolUse {
            common,
            tool_name: string(&raw, "tool_name")?.to_string(),
            tool_input: raw.get("tool_input").cloned().unwrap_or(Value::Null),
        },
        "PostToolUse" => HookPayload::PostToolUse {
            common,
            tool_name: string(&raw, "tool_name")?.to_string(),
            tool_response: raw.get("tool_response").cloned().unwrap_or(Value::Null),
        },
        "Notification" => HookPayload::Notification {
            common,
            message: raw
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "Stop" => HookPayload::Stop {
            permission_mode: raw
                .get("permission_mode")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            last_assistant_message: raw
                .get("last_assistant_message")
                .and_then(Value::as_str)
                .map(str::to_string),
            common,
        },
        "SessionEnd" => HookPayload::SessionEnd(common),
        _ => HookPayload::Unknown { name, common, raw },
    };
    Ok(parsed)
}

fn parse_common(raw: &Value) -> Result<HookCommon, HookParseError> {
    let session_id = string(raw, "session_id")?
        .parse()
        .map_err(|_| HookParseError::Invalid("session_id"))?;
    Ok(HookCommon {
        session_id,
        transcript_path: PathBuf::from(string(raw, "transcript_path")?),
        cwd: PathBuf::from(string(raw, "cwd")?),
        permission_mode: raw
            .get("permission_mode")
            .and_then(Value::as_str)
            .map(str::to_string),
        messaging: None,
        raw: raw.clone(),
    })
}

fn parse_forwarded(payload: &[u8]) -> Result<HookPayload, HookParseError> {
    let raw: Value = serde_json::from_slice(payload)?;
    let Some(envelope) = raw.get(FORWARD_ENVELOPE_FIELD) else {
        return parse_value(raw);
    };
    let version = envelope.get("version").and_then(Value::as_u64);
    if version != Some(1) {
        return Err(HookParseError::Invalid("amux_hook_forward_v1.version"));
    }
    let original = envelope
        .get("payload")
        .cloned()
        .ok_or(HookParseError::Missing("amux_hook_forward_v1.payload"))?;
    let messaging = envelope
        .get("messaging")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    let mut parsed = parse_value(original)?;
    parsed.common_mut().messaging = messaging;
    Ok(parsed)
}

fn string<'a>(raw: &'a Value, field: &'static str) -> Result<&'a str, HookParseError> {
    raw.get(field)
        .ok_or(HookParseError::Missing(field))?
        .as_str()
        .ok_or(HookParseError::Invalid(field))
}

/// A per-session Unix socket receiving forwarded hook stdin.
pub struct HookReceiver {
    pub path: PathBuf,
    payloads: Mutex<Option<mpsc::Receiver<HookPayload>>>,
    task: tokio::task::JoinHandle<()>,
}

impl HookReceiver {
    #[cfg(unix)]
    pub fn bind_sync(dir: &Path) -> Result<Self, std::io::Error> {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        std::fs::create_dir_all(dir)?;
        let nonce = Uuid::new_v4().simple().to_string();
        let path = dir.join(format!("h-{}.sock", &nonce[..12]));
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(listener)?;
        let (tx, rx) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut bytes = Vec::new();
                if stream.read_to_end(&mut bytes).await.is_ok()
                    && let Ok(payload) = parse_forwarded(&bytes)
                    && tx.send(payload).await.is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            path,
            payloads: Mutex::new(Some(rx)),
            task,
        })
    }

    #[cfg(not(unix))]
    pub fn bind_sync(_dir: &Path) -> Result<Self, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Claude hook sockets require Unix",
        ))
    }

    #[cfg(unix)]
    pub async fn bind(dir: &Path) -> Result<Self, std::io::Error> {
        Self::bind_sync(dir)
    }

    #[cfg(not(unix))]
    pub async fn bind(_dir: &Path) -> Result<Self, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Claude hook sockets require Unix",
        ))
    }

    pub fn payloads(&self) -> mpsc::Receiver<HookPayload> {
        self.payloads
            .lock()
            .expect("hook receiver mutex poisoned")
            .take()
            .expect("hook payload stream already taken")
    }
}

impl Drop for HookReceiver {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Forward hook stdin to a session socket without waiting for a response.
#[cfg(unix)]
pub fn forward(stdin: &[u8], socket: &Path) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.write_all(stdin)?;
    stream.shutdown(std::net::Shutdown::Write)
}

/// Forward hook stdin and the provider's per-session messaging credentials.
///
/// The transport envelope is removed by [`HookReceiver`]; the credential token
/// is never inserted into [`HookPayload::raw`].
#[cfg(unix)]
pub fn forward_with_messaging(
    stdin: &[u8],
    socket: &Path,
    messaging: &MessagingCredentials,
) -> Result<(), std::io::Error> {
    let payload: Value = serde_json::from_slice(stdin)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let envelope = serde_json::json!({
        FORWARD_ENVELOPE_FIELD: {
            "version": 1,
            "payload": payload,
            "messaging": messaging,
        }
    });
    let encoded = serde_json::to_vec(&envelope)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    forward(&encoded, socket)
}

#[cfg(not(unix))]
pub fn forward_with_messaging(
    _stdin: &[u8],
    _socket: &Path,
    _messaging: &MessagingCredentials,
) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Claude hook sockets require Unix",
    ))
}

#[cfg(not(unix))]
pub fn forward(_stdin: &[u8], _socket: &Path) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Claude hook sockets require Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(name: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "hook_event_name": name,
            "session_id": "00000000-0000-0000-0000-000000000001",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp",
            "tool_name": "Bash",
            "tool_input": {"command":"ls"}
        }))
        .unwrap()
    }

    #[test]
    fn known_and_unknown_payloads_preserve_raw_fields() {
        let known = parse(&payload("PermissionRequest")).unwrap();
        assert!(matches!(known, HookPayload::PermissionRequest { .. }));
        assert_eq!(known.raw()["tool_input"]["command"], "ls");

        let unknown = parse(&payload("FutureHook")).unwrap();
        assert_eq!(unknown.name(), "FutureHook");
        assert_eq!(unknown.common().session_id, Uuid::from_u128(1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn receiver_accepts_forwarded_stdin() {
        let dir = tempfile::Builder::new()
            .prefix("ch")
            .tempdir_in("/tmp")
            .unwrap();
        let receiver = HookReceiver::bind(dir.path()).await.unwrap();
        let mut payloads = receiver.payloads();
        forward(&payload("SessionStart"), &receiver.path).unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), payloads.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received.name(), "SessionStart");
        assert!(received.common().messaging.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn receiver_keeps_messaging_credentials_out_of_raw_payload() {
        let dir = tempfile::Builder::new()
            .prefix("ch")
            .tempdir_in("/tmp")
            .unwrap();
        let receiver = HookReceiver::bind(dir.path()).await.unwrap();
        let mut payloads = receiver.payloads();
        let messaging = MessagingCredentials {
            socket_path: PathBuf::from("/runtime/claude.sock"),
            token: "secret".to_string(),
        };

        forward_with_messaging(&payload("SessionStart"), &receiver.path, &messaging).unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), payloads.recv())
            .await
            .unwrap()
            .unwrap();

        let received_messaging = received.common().messaging.as_ref().unwrap();
        assert_eq!(received_messaging.socket_path, messaging.socket_path);
        assert_eq!(received_messaging.token, messaging.token);
        assert!(received.raw().get(FORWARD_ENVELOPE_FIELD).is_none());
        assert!(!received.raw().to_string().contains("secret"));
        assert!(!format!("{received:?}").contains("secret"));
    }
}

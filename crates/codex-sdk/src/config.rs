use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::approval::ApprovalHandler;
use crate::types::ServiceTier;

// ── CodexConfig ───────────────────────────────────────────────────

pub struct CodexConfig {
    /// Path to the `codex` binary. Defaults to `"codex"` (found via PATH).
    pub codex_path: Option<PathBuf>,
    /// Working directory for the subprocess.
    pub cwd: Option<PathBuf>,
    /// Client name sent in the initialize handshake.
    pub client_name: String,
    /// Optional client title sent in the initialize handshake.
    pub client_title: Option<String>,
    /// Client version sent in the initialize handshake.
    pub client_version: String,
    /// Whether to enable the experimental API surface.
    pub experimental_api: bool,
    /// Exact notification method names to suppress for this connection.
    pub opt_out_notification_methods: Vec<String>,
    /// Optional approval handler for server-initiated permission requests.
    /// If `None`, approvals surface as `TurnEvent::ApprovalRequired`.
    pub approval_handler: Option<Arc<dyn ApprovalHandler>>,
    /// Extra environment variables for the subprocess.
    pub env: Option<HashMap<String, String>>,
    /// `--config key=value` pairs passed to the codex CLI.
    pub config_overrides: Vec<(String, String)>,
    /// Optional JSONL path that receives an exact timestamped tee of JSON-RPC
    /// lines in both directions.
    pub record_io: Option<PathBuf>,
}

// Manual Default because `Option<Arc<dyn ApprovalHandler>>` prevents derive.
impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            codex_path: None,
            cwd: None,
            client_name: "codex-rust-sdk".into(),
            client_title: None,
            client_version: env!("CARGO_PKG_VERSION").into(),
            experimental_api: true,
            opt_out_notification_methods: Vec::new(),
            approval_handler: None,
            env: None,
            config_overrides: Vec::new(),
            record_io: None,
        }
    }
}

// ── ThreadConfig ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ThreadConfig {
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub service_tier: Option<ServiceTier>,
    pub cwd: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub sandbox: Option<SandboxMode>,
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
    pub service_name: Option<String>,
    pub base_instructions: Option<String>,
    pub developer_instructions: Option<String>,
    pub personality: Option<Personality>,
    pub ephemeral: Option<bool>,
    pub experimental_raw_events: Option<bool>,
    pub persist_extended_history: Option<bool>,
    /// Escape hatch for experimental/new fields not yet in the typed API.
    /// Keys are camelCase wire names, values are arbitrary JSON.
    pub extra: HashMap<String, serde_json::Value>,
}

// ── TurnConfig ────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TurnConfig {
    pub cwd: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub sandbox_policy: Option<SandboxPolicy>,
    pub model: Option<String>,
    pub service_tier: Option<ServiceTier>,
    pub effort: Option<ReasoningEffort>,
    pub summary: Option<SummaryMode>,
    pub personality: Option<Personality>,
    pub output_schema: Option<serde_json::Value>,
    pub collaboration_mode: Option<CollaborationMode>,
    /// Escape hatch for experimental/new fields.
    pub extra: HashMap<String, serde_json::Value>,
}

// ── TurnInput ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TurnInput {
    Text(String),
    Items(Vec<InputItem>),
}

impl From<&str> for TurnInput {
    fn from(s: &str) -> Self {
        TurnInput::Text(s.to_owned())
    }
}

impl From<String> for TurnInput {
    fn from(s: String) -> Self {
        TurnInput::Text(s)
    }
}

impl From<Vec<InputItem>> for TurnInput {
    fn from(items: Vec<InputItem>) -> Self {
        TurnInput::Items(items)
    }
}

// ── InputItem ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputItem {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { url: String },
    #[serde(rename = "localImage")]
    LocalImage { path: PathBuf },
}

impl InputItem {
    pub fn text(text: impl Into<String>) -> Self {
        InputItem::Text { text: text.into() }
    }

    pub fn image(url: impl Into<String>) -> Self {
        InputItem::Image { url: url.into() }
    }

    pub fn local_image(path: impl Into<PathBuf>) -> Self {
        InputItem::LocalImage { path: path.into() }
    }
}

// ── Enums ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Require approval unless tool is trusted.
    Untrusted,
    /// Require approval on request (default).
    OnRequest,
    /// Never require approval (full auto).
    Never,
    /// Experimental granular approval routing.
    Granular(GranularApprovalPolicy),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GranularApprovalPolicy {
    pub sandbox_approval: bool,
    pub rules: bool,
    #[serde(default)]
    pub skill_approval: bool,
    #[serde(default)]
    pub request_permissions: bool,
    pub mcp_elicitations: bool,
}

impl Serialize for ApprovalPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Untrusted => serializer.serialize_str("untrusted"),
            Self::OnRequest => serializer.serialize_str("on-request"),
            Self::Never => serializer.serialize_str("never"),
            Self::Granular(granular) => {
                #[derive(Serialize)]
                struct Wrapper<'a> {
                    granular: &'a GranularApprovalPolicy,
                }
                Wrapper { granular }.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ApprovalPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Named(String),
            Granular { granular: GranularApprovalPolicy },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Named(value) => match value.as_str() {
                "untrusted" => Ok(Self::Untrusted),
                "on-request" => Ok(Self::OnRequest),
                "never" => Ok(Self::Never),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["untrusted", "on-request", "never", "granular"],
                )),
            },
            Repr::Granular { granular } => Ok(Self::Granular(granular)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxMode {
    #[serde(rename = "danger-full-access")]
    DangerFullAccess,
    #[serde(rename = "workspace-write")]
    WorkspaceWrite,
    #[serde(rename = "read-only")]
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SandboxPolicy {
    DangerFullAccess,
    ReadOnly {
        #[serde(default, skip_serializing_if = "ReadOnlyAccess::is_full_access")]
        access: ReadOnlyAccess,
        #[serde(default)]
        network_access: bool,
    },
    ExternalSandbox {
        #[serde(default)]
        network_access: NetworkAccess,
    },
    WorkspaceWrite {
        #[serde(default)]
        writable_roots: Vec<PathBuf>,
        #[serde(default, skip_serializing_if = "ReadOnlyAccess::is_full_access")]
        read_only_access: ReadOnlyAccess,
        #[serde(default)]
        network_access: bool,
        #[serde(default)]
        exclude_tmpdir_env_var: bool,
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ReadOnlyAccess {
    Restricted {
        include_platform_defaults: bool,
        readable_roots: Vec<PathBuf>,
    },
    #[default]
    FullAccess,
}

impl ReadOnlyAccess {
    fn is_full_access(&self) -> bool {
        matches!(self, Self::FullAccess)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NetworkAccess {
    #[default]
    Restricted,
    Enabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalsReviewer {
    User,
    AutoReview,
    GuardianSubagent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Personality {
    None,
    Friendly,
    Pragmatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryMode {
    Auto,
    Concise,
    Detailed,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationMode {
    pub mode: CollaborationModeKind,
    pub settings: CollaborationModeSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollaborationModeSettings {
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub developer_instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CollaborationModeKind {
    Plan,
    Default,
}

// ── Config serialization helper ──────────────────────────────────

/// Build a JSON object from typed fields, then merge `extra` on top.
pub(crate) fn merge_config(
    typed: serde_json::Value,
    extra: &HashMap<String, serde_json::Value>,
) -> serde_json::Value {
    let mut obj = match typed {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    for (k, v) in extra {
        obj.insert(k.clone(), v.clone());
    }
    serde_json::Value::Object(obj)
}

fn insert_json(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: impl Into<serde_json::Value>,
) {
    obj.insert(key.to_owned(), value.into());
}

fn insert_serialized<T: Serialize>(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<&T>,
) {
    if let Some(value) = value {
        insert_json(obj, key, serde_json::to_value(value).unwrap());
    }
}

fn extend_extra(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    extra: &HashMap<String, serde_json::Value>,
) {
    obj.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
}

/// Serialize `ThreadConfig` into JSON-RPC params.
pub(crate) fn thread_config_to_params(config: &ThreadConfig) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    insert_serialized(&mut obj, "model", config.model.as_ref());
    insert_serialized(&mut obj, "modelProvider", config.model_provider.as_ref());
    insert_serialized(&mut obj, "serviceTier", config.service_tier.as_ref());
    insert_serialized(&mut obj, "cwd", config.cwd.as_ref());
    insert_serialized(&mut obj, "approvalPolicy", config.approval_policy.as_ref());
    insert_serialized(
        &mut obj,
        "approvalsReviewer",
        config.approvals_reviewer.as_ref(),
    );
    insert_serialized(&mut obj, "sandbox", config.sandbox.as_ref());
    if let Some(config_map) = &config.config {
        insert_json(
            &mut obj,
            "config",
            serde_json::Value::Object(config_map.clone()),
        );
    }
    insert_serialized(&mut obj, "serviceName", config.service_name.as_ref());
    insert_serialized(
        &mut obj,
        "baseInstructions",
        config.base_instructions.as_ref(),
    );
    insert_serialized(
        &mut obj,
        "developerInstructions",
        config.developer_instructions.as_ref(),
    );
    insert_serialized(&mut obj, "personality", config.personality.as_ref());
    if let Some(e) = config.ephemeral {
        insert_json(&mut obj, "ephemeral", e);
    }
    if let Some(value) = config.experimental_raw_events {
        insert_json(&mut obj, "experimentalRawEvents", value);
    }
    if let Some(value) = config.persist_extended_history {
        insert_json(&mut obj, "persistExtendedHistory", value);
    }
    merge_config(serde_json::Value::Object(obj), &config.extra)
}

/// Serialize `TurnConfig` into a partial JSON object (merged into turn/start params).
pub(crate) fn turn_config_to_params(
    config: &TurnConfig,
) -> serde_json::Map<String, serde_json::Value> {
    let mut obj = serde_json::Map::new();
    insert_serialized(&mut obj, "cwd", config.cwd.as_ref());
    insert_serialized(&mut obj, "approvalPolicy", config.approval_policy.as_ref());
    insert_serialized(
        &mut obj,
        "approvalsReviewer",
        config.approvals_reviewer.as_ref(),
    );
    insert_serialized(&mut obj, "sandboxPolicy", config.sandbox_policy.as_ref());
    insert_serialized(&mut obj, "model", config.model.as_ref());
    insert_serialized(&mut obj, "serviceTier", config.service_tier.as_ref());
    insert_serialized(&mut obj, "effort", config.effort.as_ref());
    insert_serialized(&mut obj, "summary", config.summary.as_ref());
    insert_serialized(&mut obj, "personality", config.personality.as_ref());
    if let Some(ref s) = config.output_schema {
        insert_json(&mut obj, "outputSchema", s.clone());
    }
    insert_serialized(
        &mut obj,
        "collaborationMode",
        config.collaboration_mode.as_ref(),
    );
    extend_extra(&mut obj, &config.extra);
    obj
}

/// Serialize `TurnInput` into JSON value for the `input` field.
pub(crate) fn turn_input_to_value(input: TurnInput) -> serde_json::Value {
    match input {
        TurnInput::Text(s) => serde_json::json!([{"type": "text", "text": s}]),
        TurnInput::Items(items) => serde_json::to_value(&items).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_policy_round_trips_generated_0_147_schema_shape() {
        let schema_shaped = serde_json::json!({
            "type": "workspaceWrite",
            "writableRoots": ["/workspace", "/tmp/output"],
            "networkAccess": true,
            "excludeTmpdirEnvVar": true,
            "excludeSlashTmp": false
        });

        let policy: SandboxPolicy = serde_json::from_value(schema_shaped.clone()).unwrap();
        assert_eq!(serde_json::to_value(policy).unwrap(), schema_shaped);
    }

    #[test]
    fn restricted_read_only_access_uses_camel_case_fields() {
        let schema_shaped = serde_json::json!({
            "type": "restricted",
            "includePlatformDefaults": true,
            "readableRoots": ["/opt/shared"]
        });

        let access: ReadOnlyAccess = serde_json::from_value(schema_shaped.clone()).unwrap();
        assert_eq!(serde_json::to_value(access).unwrap(), schema_shaped);
    }
}

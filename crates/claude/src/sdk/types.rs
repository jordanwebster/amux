use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub type Extensions = serde_json::Map<String, serde_json::Value>;

/// A valid protocol object whose discriminant is not known to this SDK.
/// The complete object is retained so newer producers remain observable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawFrame {
    pub raw: serde_json::Value,
}

impl RawFrame {
    pub fn new(raw: serde_json::Value) -> Self {
        Self { raw }
    }

    pub fn field(&self, name: &str) -> Option<&serde_json::Value> {
        self.raw.get(name)
    }
}

macro_rules! open_string_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
            Unknown(String),
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let value = match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(value) => value,
                };
                serializer.serialize_str(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($wire => Self::$variant,)+
                    _ => Self::Unknown(value),
                })
            }
        }
    };
}
// ── PermissionMode ──────────────────────────────────────────────────

open_string_enum!(PermissionMode {
    Default => "default",
    AcceptEdits => "acceptEdits",
    BypassPermissions => "bypassPermissions",
    Plan => "plan",
    DontAsk => "dontAsk",
    Auto => "auto",
});

// ── Role ────────────────────────────────────────────────────────────

open_string_enum!(Role {
    User => "user",
    Assistant => "assistant",
});

// ── StopReason ──────────────────────────────────────────────────────

open_string_enum!(StopReason {
    EndTurn => "end_turn",
    MaxTokens => "max_tokens",
    StopSequence => "stop_sequence",
    ToolUse => "tool_use",
    Refusal => "refusal",
    PauseTurn => "pause_turn",
});

// ── ContentBlock ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Image {
        source: ImageSource,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default, deserialize_with = "deserialize_tool_result_content")]
        content: Vec<ToolResultContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(flatten)]
        extensions: Extensions,
    },
    RedactedThinking {
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "text" => parse_payload(raw, "type")
                .map(|value: TextContentBlock| Self::Text {
                    text: value.text,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "image" => parse_payload(raw, "type")
                .map(|value: ImageContentBlock| Self::Image {
                    source: value.source,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "tool_use" => parse_payload(raw, "type")
                .map(|value: ToolUseContentBlock| Self::ToolUse {
                    id: value.id,
                    name: value.name,
                    input: value.input,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "tool_result" => parse_payload(raw, "type")
                .map(|value: ToolResultContentBlock| Self::ToolResult {
                    tool_use_id: value.tool_use_id,
                    content: value.content,
                    is_error: value.is_error,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "thinking" => parse_payload(raw, "type")
                .map(|value: ThinkingContentBlock| Self::Thinking {
                    thinking: value.thinking,
                    signature: value.signature,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "redacted_thinking" => parse_payload(raw, "type")
                .map(|value: ExtensionOnly| Self::RedactedThinking {
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::new(raw))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text {
        text: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Image {
        source: ImageSource,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl<'de> Deserialize<'de> for ToolResultContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "text" => parse_payload(raw, "type")
                .map(|value: TextContentBlock| Self::Text {
                    text: value.text,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "image" => parse_payload(raw, "type")
                .map(|value: ImageContentBlock| Self::Image {
                    source: value.source,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::new(raw))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub r#type: ImageSourceType,
    pub media_type: String,
    pub data: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

open_string_enum!(ImageSourceType { Base64 => "base64" });

// ── Usage ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── ModelUsage ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub web_search_requests: u64,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
    pub context_window: u64,
    pub max_output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_basis: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── ApiMessage ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub id: String,
    pub r#type: String,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub model: String,
    /// Outer None means absent; Some(None) preserves an explicit null at stream start.
    #[serde(
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_reason: Option<Option<StopReason>>,
    /// Distinguishes an omitted field from an explicit null on the wire.
    #[serde(
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_sequence: Option<Option<String>>,
    pub usage: Usage,
    #[serde(flatten)]
    pub extensions: Extensions,
}

fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

// ── MessageParam ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: Role,
    pub content: MessageContent,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

// ── PermissionDenial ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDenial {
    pub tool_name: String,
    pub tool_use_id: String,
    pub tool_input: serde_json::Value,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── ApiKeySource ────────────────────────────────────────────────────

open_string_enum!(ApiKeySource {
    AnthropicApiKey => "ANTHROPIC_API_KEY",
    ApiKeyHelper => "apiKeyHelper",
    LoginManagedKey => "/login managed key",
    None => "none",
    User => "user",
    Project => "project",
    Org => "org",
    Temporary => "temporary",
    OAuth => "oauth",
});

// ── AssistantMessageError ───────────────────────────────────────────

open_string_enum!(AssistantMessageError {
    AuthenticationFailed => "authentication_failed",
    OauthOrgNotAllowed => "oauth_org_not_allowed",
    AccountOnHold => "account_on_hold",
    BillingError => "billing_error",
    RateLimit => "rate_limit",
    Overloaded => "overloaded",
    InvalidRequest => "invalid_request",
    ModelNotFound => "model_not_found",
    ServerError => "server_error",
    UnknownError => "unknown",
    MaxOutputTokens => "max_output_tokens",
});

// ── RawMessageStreamEvent ───────────────────────────────────────────

/// One event from the Messages API lifecycle carried by a `stream_event`
/// envelope. Known events retain extension fields; newer events remain
/// available through [`StreamEvent::Unknown`].
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: ApiMessage,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ContentBlockDelta {
        index: u32,
        delta: StreamDelta,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ContentBlockStop {
        index: u32,
        #[serde(flatten)]
        extensions: Extensions,
    },
    MessageDelta {
        delta: StreamMessageDelta,
        usage: StreamMessageUsage,
        #[serde(flatten)]
        extensions: Extensions,
    },
    MessageStop {
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl<'de> Deserialize<'de> for StreamEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "message_start" => parse_payload(raw, "type")
                .map(|value: MessageStartEvent| Self::MessageStart {
                    message: value.message,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "content_block_start" => parse_payload(raw, "type")
                .map(|value: ContentBlockStartEvent| Self::ContentBlockStart {
                    index: value.index,
                    content_block: value.content_block,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "content_block_delta" => parse_payload(raw, "type")
                .map(|value: ContentBlockDeltaEvent| Self::ContentBlockDelta {
                    index: value.index,
                    delta: value.delta,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "content_block_stop" => parse_payload(raw, "type")
                .map(|value: ContentBlockStopEvent| Self::ContentBlockStop {
                    index: value.index,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "message_delta" => parse_payload(raw, "type")
                .map(|value: MessageDeltaEvent| Self::MessageDelta {
                    delta: value.delta,
                    usage: value.usage,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "message_stop" => parse_payload(raw, "type")
                .map(|value: ExtensionOnly| Self::MessageStop {
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::new(raw))),
        }
    }
}

/// A typed incremental change to a content block.
///
/// The variant names are spelled out rather than derived from the Rust names,
/// so that serializing a delta produces the discriminant the wire uses and this
/// same type can read it back. Deriving them gave `thinking` where the wire
/// says `thinking_delta`, which only showed up when a recorded frame was parsed
/// and re-serialized.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum StreamDelta {
    #[serde(rename = "text_delta")]
    Text {
        text: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(rename = "thinking_delta")]
    Thinking {
        thinking: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(rename = "signature_delta")]
    Signature {
        signature: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(rename = "input_json_delta")]
    ToolInput {
        partial_json: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl<'de> Deserialize<'de> for StreamDelta {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "text_delta" => parse_payload(raw, "type")
                .map(|value: TextStreamDelta| Self::Text {
                    text: value.text,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "thinking_delta" => parse_payload(raw, "type")
                .map(|value: ThinkingStreamDelta| Self::Thinking {
                    thinking: value.thinking,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "signature_delta" => parse_payload(raw, "type")
                .map(|value: SignatureStreamDelta| Self::Signature {
                    signature: value.signature,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "input_json_delta" => parse_payload(raw, "type")
                .map(|value: ToolInputStreamDelta| Self::ToolInput {
                    partial_json: value.partial_json,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::new(raw))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMessageDelta {
    #[serde(deserialize_with = "required_stream_option")]
    pub stop_reason: Option<StopReason>,
    #[serde(deserialize_with = "required_stream_option")]
    pub stop_sequence: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMessageUsage {
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Deserialize)]
struct MessageStartEvent {
    message: ApiMessage,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ContentBlockStartEvent {
    index: u32,
    content_block: ContentBlock,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ContentBlockDeltaEvent {
    index: u32,
    delta: StreamDelta,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ContentBlockStopEvent {
    index: u32,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct MessageDeltaEvent {
    delta: StreamMessageDelta,
    usage: StreamMessageUsage,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct TextStreamDelta {
    text: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ThinkingStreamDelta {
    thinking: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct SignatureStreamDelta {
    signature: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ToolInputStreamDelta {
    partial_json: String,
    #[serde(flatten)]
    extensions: Extensions,
}

fn required_stream_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Deserialize)]
struct TextContentBlock {
    text: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ImageContentBlock {
    source: ImageSource,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ToolUseContentBlock {
    id: String,
    name: String,
    input: serde_json::Value,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ToolResultContentBlock {
    tool_use_id: String,
    #[serde(default, deserialize_with = "deserialize_tool_result_content")]
    content: Vec<ToolResultContent>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ThinkingContentBlock {
    thinking: String,
    #[serde(default)]
    signature: Option<String>,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ExtensionOnly {
    #[serde(flatten)]
    extensions: Extensions,
}

fn required_discriminant<'a, E>(raw: &'a serde_json::Value, field: &str) -> Result<&'a str, E>
where
    E: serde::de::Error,
{
    let object = raw
        .as_object()
        .ok_or_else(|| E::custom("protocol variant must be a JSON object"))?;
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| E::custom(format!("protocol variant requires string field `{field}`")))
}

fn parse_payload<T: DeserializeOwned>(
    mut raw: serde_json::Value,
    discriminant: &str,
) -> Result<T, serde_json::Error> {
    if let Some(object) = raw.as_object_mut() {
        object.remove(discriminant);
    }
    serde_json::from_value(raw)
}

// ── Permission types ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PermissionUpdate {
    AddRules {
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionBehavior,
        destination: PermissionUpdateDestination,
    },
    ReplaceRules {
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionBehavior,
        destination: PermissionUpdateDestination,
    },
    RemoveRules {
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionBehavior,
        destination: PermissionUpdateDestination,
    },
    SetMode {
        mode: PermissionMode,
        destination: PermissionUpdateDestination,
    },
    AddDirectories {
        directories: Vec<String>,
        destination: PermissionUpdateDestination,
    },
    RemoveDirectories {
        directories: Vec<String>,
        destination: PermissionUpdateDestination,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleValue {
    pub tool_name: String,
    pub rule_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionUpdateDestination {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    Session,
    CliArg,
}

// ── PermissionResult ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "behavior",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PermissionResult {
    Allow {
        updated_input: Option<serde_json::Value>,
        updated_permissions: Option<Vec<PermissionUpdate>>,
        tool_use_id: Option<String>,
    },
    Deny {
        message: String,
        interrupt: Option<bool>,
        tool_use_id: Option<String>,
    },
}

// ── CanUseToolOptions ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CanUseToolOptions {
    pub suggestions: Vec<PermissionUpdate>,
    pub blocked_path: Option<String>,
    pub decision_reason: Option<String>,
    pub decision_reason_type: Option<String>,
    pub classifier_approvable: Option<bool>,
    pub suppress_always_allow_rule: Option<bool>,
    pub default_to_no: Option<bool>,
    pub title: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub tool_use_id: String,
    pub agent_id: Option<String>,
    pub request_id: String,
    pub matched_ask_rule: Option<MatchedAskRule>,
    pub requires_user_interaction: Option<bool>,
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchedAskRule {
    pub source: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── Hook enums used in multiple modules ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartSource {
    Startup,
    Resume,
    Clear,
    Compact,
}

open_string_enum!(CompactTrigger {
    Manual => "manual",
    Auto => "auto",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupTrigger {
    Init,
    Maintenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigChangeSource {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    PolicySettings,
    Skills,
}

// ── Deserialization helpers ────────────────────────────────────────

/// Deserialize the `content` field of a ToolResult, which the CLI may
/// send as a plain string, an array of content blocks, or null.
fn deserialize_tool_result_content<'de, D>(
    deserializer: D,
) -> Result<Vec<ToolResultContent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct Visitor;

    impl<'de> de::Visitor<'de> for Visitor {
        type Value = Vec<ToolResultContent>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string, array of content blocks, or null")
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(vec![])
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(vec![])
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(vec![ToolResultContent::Text {
                text: v.to_string(),
                extensions: Extensions::new(),
            }])
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(vec![ToolResultContent::Text {
                text: v,
                extensions: Extensions::new(),
            }])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            Vec::deserialize(de::value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(Visitor)
}

//! Shared vocabulary derived from structured-agent observations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{CLAUDE_PTY_TRANSCRIPT_V1, CLAUDE_SDK_V1, CODEX_SDK_V1, ContextMeter};

/// One row's number within an agent. Zero names the position before row one.
pub type Seq = u64;

/// The structured protocol whose rows a fold consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuredProtocol {
    #[serde(rename = "claude_pty_transcript_v1")]
    ClaudePtyTranscript,
    #[serde(rename = "claude_sdk_v1")]
    ClaudeSdk,
    #[serde(rename = "codex_sdk_v1")]
    Codex,
}

impl StructuredProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudePtyTranscript => CLAUDE_PTY_TRANSCRIPT_V1,
            Self::ClaudeSdk => CLAUDE_SDK_V1,
            Self::Codex => CODEX_SDK_V1,
        }
    }
}

/// Whether an agent currently needs the operator's attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attention {
    Unknown,
    Idle,
    Working,
    NeedsYou { why: Why },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    Permission,
    Question,
    Finished,
}

/// Lifecycle phase observed from session-stream facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPhase {
    Running,
    Exited { exit_code: Option<i32> },
}

/// The compact todo value shown in an agent summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoProgress {
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
}

/// The small folded value a fleet row draws.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub attention: Attention,
    pub phase: AgentPhase,
    pub last_activity: Option<DateTime<Utc>>,
    pub todo: Option<TodoProgress>,
    pub context: Option<ContextMeter>,
    pub model: Option<String>,
    pub unknown: Vec<SummaryField>,
}

/// Summary fields for which the fold has no knowledge since its baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryField {
    Attention,
    Phase,
    LastActivity,
    Todo,
    Context,
    Model,
    Outstanding,
}

/// Advisory host-produced summary with its fold and publication positions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryEnvelope {
    pub through: Seq,
    pub producer_version: u32,
    pub observed_at: DateTime<Utc>,
    pub stale: bool,
    pub revision: u64,
    pub summary: Summary,
}

/// Folded-through watermark published on the fleet stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub through: Seq,
    pub at: DateTime<Utc>,
    pub revision: u64,
}

/// One durable counter per host, reserved before authoritative publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HostRevision(pub u64);

// Human-readable serializers keep the established API JSON. Persisted binary
// formats use external tags so non-self-describing codecs such as postcard can
// decode the same domain types without `deserialize_any`.
#[derive(Serialize, Deserialize)]
#[serde(tag = "attention", rename_all = "snake_case")]
enum HumanAttention {
    Unknown,
    Idle,
    Working,
    NeedsYou { why: Why },
}

#[derive(Serialize, Deserialize)]
enum BinaryAttention {
    Unknown,
    Idle,
    Working,
    NeedsYou { why: Why },
}

impl From<Attention> for HumanAttention {
    fn from(value: Attention) -> Self {
        match value {
            Attention::Unknown => Self::Unknown,
            Attention::Idle => Self::Idle,
            Attention::Working => Self::Working,
            Attention::NeedsYou { why } => Self::NeedsYou { why },
        }
    }
}

impl From<HumanAttention> for Attention {
    fn from(value: HumanAttention) -> Self {
        match value {
            HumanAttention::Unknown => Self::Unknown,
            HumanAttention::Idle => Self::Idle,
            HumanAttention::Working => Self::Working,
            HumanAttention::NeedsYou { why } => Self::NeedsYou { why },
        }
    }
}

impl From<Attention> for BinaryAttention {
    fn from(value: Attention) -> Self {
        match value {
            Attention::Unknown => Self::Unknown,
            Attention::Idle => Self::Idle,
            Attention::Working => Self::Working,
            Attention::NeedsYou { why } => Self::NeedsYou { why },
        }
    }
}

impl From<BinaryAttention> for Attention {
    fn from(value: BinaryAttention) -> Self {
        match value {
            BinaryAttention::Unknown => Self::Unknown,
            BinaryAttention::Idle => Self::Idle,
            BinaryAttention::Working => Self::Working,
            BinaryAttention::NeedsYou { why } => Self::NeedsYou { why },
        }
    }
}

impl Serialize for Attention {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            HumanAttention::from(*self).serialize(serializer)
        } else {
            BinaryAttention::from(*self).serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Attention {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            return HumanAttention::deserialize(deserializer).map(Into::into);
        }
        BinaryAttention::deserialize(deserializer).map(Into::into)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum HumanAgentPhase {
    Running,
    Exited { exit_code: Option<i32> },
}

#[derive(Serialize, Deserialize)]
enum BinaryAgentPhase {
    Running,
    Exited { exit_code: Option<i32> },
}

impl From<AgentPhase> for HumanAgentPhase {
    fn from(value: AgentPhase) -> Self {
        match value {
            AgentPhase::Running => Self::Running,
            AgentPhase::Exited { exit_code } => Self::Exited { exit_code },
        }
    }
}

impl From<HumanAgentPhase> for AgentPhase {
    fn from(value: HumanAgentPhase) -> Self {
        match value {
            HumanAgentPhase::Running => Self::Running,
            HumanAgentPhase::Exited { exit_code } => Self::Exited { exit_code },
        }
    }
}

impl From<AgentPhase> for BinaryAgentPhase {
    fn from(value: AgentPhase) -> Self {
        match value {
            AgentPhase::Running => Self::Running,
            AgentPhase::Exited { exit_code } => Self::Exited { exit_code },
        }
    }
}

impl From<BinaryAgentPhase> for AgentPhase {
    fn from(value: BinaryAgentPhase) -> Self {
        match value {
            BinaryAgentPhase::Running => Self::Running,
            BinaryAgentPhase::Exited { exit_code } => Self::Exited { exit_code },
        }
    }
}

impl Serialize for AgentPhase {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            HumanAgentPhase::from(self.clone()).serialize(serializer)
        } else {
            BinaryAgentPhase::from(self.clone()).serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for AgentPhase {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            return HumanAgentPhase::deserialize(deserializer).map(Into::into);
        }
        BinaryAgentPhase::deserialize(deserializer).map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentPhase, Attention, StructuredProtocol, Why};

    #[test]
    fn structured_protocol_keeps_existing_wire_names() {
        assert_eq!(
            serde_json::to_string(&StructuredProtocol::ClaudePtyTranscript).unwrap(),
            r#""claude_pty_transcript_v1""#
        );
        assert_eq!(
            serde_json::to_string(&StructuredProtocol::ClaudeSdk).unwrap(),
            r#""claude_sdk_v1""#
        );
        assert_eq!(
            serde_json::to_string(&StructuredProtocol::Codex).unwrap(),
            r#""codex_sdk_v1""#
        );
    }

    #[test]
    fn attention_keeps_existing_tagged_shape() {
        assert_eq!(
            serde_json::to_value(Attention::NeedsYou {
                why: Why::Permission,
            })
            .unwrap(),
            serde_json::json!({"attention": "needs_you", "why": "permission"})
        );
    }

    #[test]
    fn agent_phase_keeps_existing_tagged_shape() {
        let phase = AgentPhase::Exited { exit_code: Some(7) };
        assert_eq!(
            serde_json::to_value(&phase).unwrap(),
            serde_json::json!({"phase": "exited", "exit_code": 7})
        );
        assert_eq!(
            serde_json::from_value::<AgentPhase>(
                serde_json::json!({"phase": "exited", "exit_code": 7})
            )
            .unwrap(),
            phase
        );
    }
}

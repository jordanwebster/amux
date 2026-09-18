use std::path::PathBuf;
use std::str::FromStr;

use chrono::{TimeZone, Utc};
use uuid::Uuid;

use crate::{self as wire, DecodeError};

pub fn agent_to_wire(agent: &model::Agent) -> Result<wire::Agent, crate::EncodeError> {
    let working_dir = agent.working_dir.to_str().ok_or_else(|| {
        crate::EncodeError::Invalid("Agent.working_dir must be valid UTF-8".into())
    })?;
    Ok(wire::Agent {
        agent_id: agent.id.as_bytes().to_vec(),
        host_id: agent.host_id.as_bytes().to_vec(),
        name: agent.name.clone(),
        command: agent.command.clone(),
        working_dir: working_dir.to_owned(),
        kind: Some(wire::agent_kind_to_wire(agent.kind)),
        readonly: agent.readonly,
        args: agent.args.clone(),
        created_at_unix_ms: agent.created_at.timestamp_millis(),
        last_activity_unix_ms: agent.last_activity.timestamp_millis(),
        parent: agent.parent.map(|parent| wire::AgentParent {
            agent_id: parent.agent_id.as_bytes().to_vec(),
            host_id: parent.host_id.as_bytes().to_vec(),
        }),
        working_on: agent.working_on.as_ref().map(|working_on| wire::WorkingOn {
            text: working_on.text.clone(),
            updated_at_unix_ms: working_on.updated_at.timestamp_millis(),
        }),
        summary: agent.summary.as_ref().map(summary_to_wire),
        progress: agent.progress.as_ref().map(progress_to_wire),
        inventory_revision: agent.inventory_revision,
    })
}

pub fn agent_from_wire(agent: wire::Agent) -> Result<model::Agent, DecodeError> {
    let created_at = Utc
        .timestamp_millis_opt(agent.created_at_unix_ms)
        .single()
        .ok_or_else(|| DecodeError::Invalid("invalid agent created_at".into()))?;
    let last_activity = Utc
        .timestamp_millis_opt(agent.last_activity_unix_ms)
        .single()
        .ok_or_else(|| DecodeError::Invalid("invalid agent last_activity".into()))?;
    let parent = agent.parent.map(agent_parent_from_wire).transpose()?;
    let working_on = agent.working_on.map(working_on_from_wire).transpose()?;
    let summary = agent.summary.map(summary_from_wire).transpose()?;
    let progress = agent.progress.map(progress_from_wire).transpose()?;
    let kind = wire::agent_kind_from_wire(
        agent
            .kind
            .ok_or_else(|| DecodeError::Invalid("Agent missing kind".into()))?,
    )?;
    Ok(model::Agent {
        id: uuid_from_bytes("agent_id", agent.agent_id)?,
        host_id: uuid_from_bytes("host_id", agent.host_id)?,
        name: agent.name,
        command: agent.command,
        working_dir: PathBuf::from(agent.working_dir),
        kind,
        readonly: agent.readonly,
        args: agent.args,
        created_at,
        last_activity,
        parent,
        working_on,
        summary,
        progress,
        inventory_revision: agent.inventory_revision,
    })
}

pub fn summary_to_wire(envelope: &model::SummaryEnvelope) -> wire::AgentSummary {
    let (attention, why) = match envelope.summary.attention {
        model::Attention::Unknown => (wire::SummaryAttention::Unknown, None),
        model::Attention::Idle => (wire::SummaryAttention::Idle, None),
        model::Attention::Working => (wire::SummaryAttention::Working, None),
        model::Attention::NeedsYou { why } => (
            wire::SummaryAttention::NeedsYou,
            Some(match why {
                model::Why::Permission => wire::SummaryWhy::Permission as i32,
                model::Why::Question => wire::SummaryWhy::Question as i32,
                model::Why::Finished => wire::SummaryWhy::Finished as i32,
            }),
        ),
    };
    let (phase, exit_code) = match envelope.summary.phase {
        model::AgentPhase::Running => (wire::SummaryPhase::Running, None),
        model::AgentPhase::Exited { exit_code } => (wire::SummaryPhase::Exited, exit_code),
    };
    wire::AgentSummary {
        through: envelope.through,
        producer_version: envelope.producer_version,
        observed_at_unix_ms: envelope.observed_at.timestamp_millis(),
        stale: envelope.stale,
        revision: envelope.revision,
        attention: attention as i32,
        why,
        phase: phase as i32,
        exit_code,
        last_activity_unix_ms: envelope
            .summary
            .last_activity
            .map(|at| at.timestamp_millis()),
        todo: envelope
            .summary
            .todo
            .as_ref()
            .map(|todo| wire::SummaryTodoProgress {
                done: todo.done as u64,
                total: todo.total as u64,
                current: todo.current.clone(),
            }),
        context: envelope
            .summary
            .context
            .as_ref()
            .map(|context| wire::SummaryContextMeter {
                used_tokens: context.used_tokens,
                window_tokens: context.window_tokens,
                source: match context.source {
                    model::ContextMeterSource::AssistantUsage => {
                        wire::ContextMeterSource::AssistantUsage
                    }
                    model::ContextMeterSource::ResultUsage => wire::ContextMeterSource::ResultUsage,
                    model::ContextMeterSource::AssistantContextUsage => {
                        wire::ContextMeterSource::AssistantContextUsage
                    }
                    model::ContextMeterSource::CompactBoundary => {
                        wire::ContextMeterSource::CompactBoundary
                    }
                } as i32,
            }),
        model: envelope.summary.model.clone(),
        unknown: envelope
            .summary
            .unknown
            .iter()
            .map(|field| match field {
                model::SummaryField::Attention => wire::SummaryField::Attention,
                model::SummaryField::Phase => wire::SummaryField::Phase,
                model::SummaryField::LastActivity => wire::SummaryField::LastActivity,
                model::SummaryField::Todo => wire::SummaryField::Todo,
                model::SummaryField::Context => wire::SummaryField::Context,
                model::SummaryField::Model => wire::SummaryField::Model,
                model::SummaryField::Outstanding => wire::SummaryField::Outstanding,
            } as i32)
            .collect(),
    }
}

pub fn summary_from_wire(value: wire::AgentSummary) -> Result<model::SummaryEnvelope, DecodeError> {
    let observed_at = Utc
        .timestamp_millis_opt(value.observed_at_unix_ms)
        .single()
        .ok_or_else(|| DecodeError::Invalid("invalid summary observed_at".into()))?;
    let attention = match wire::SummaryAttention::try_from(value.attention) {
        Ok(wire::SummaryAttention::Unknown) => model::Attention::Unknown,
        Ok(wire::SummaryAttention::Idle) => model::Attention::Idle,
        Ok(wire::SummaryAttention::Working) => model::Attention::Working,
        Ok(wire::SummaryAttention::NeedsYou) => model::Attention::NeedsYou {
            why: match value
                .why
                .and_then(|why| wire::SummaryWhy::try_from(why).ok())
            {
                Some(wire::SummaryWhy::Permission) => model::Why::Permission,
                Some(wire::SummaryWhy::Question) => model::Why::Question,
                Some(wire::SummaryWhy::Finished) => model::Why::Finished,
                _ => return Err(DecodeError::Invalid("needs-you summary missing why".into())),
            },
        },
        _ => return Err(DecodeError::Invalid("invalid summary attention".into())),
    };
    let phase = match wire::SummaryPhase::try_from(value.phase) {
        Ok(wire::SummaryPhase::Running) => model::AgentPhase::Running,
        Ok(wire::SummaryPhase::Exited) => model::AgentPhase::Exited {
            exit_code: value.exit_code,
        },
        _ => return Err(DecodeError::Invalid("invalid summary phase".into())),
    };
    let last_activity = value
        .last_activity_unix_ms
        .map(|at| {
            Utc.timestamp_millis_opt(at)
                .single()
                .ok_or_else(|| DecodeError::Invalid("invalid summary last_activity".into()))
        })
        .transpose()?;
    let todo = value
        .todo
        .map(|todo| -> Result<model::TodoProgress, DecodeError> {
            Ok(model::TodoProgress {
                done: usize::try_from(todo.done)
                    .map_err(|_| DecodeError::Invalid("summary todo done is too large".into()))?,
                total: usize::try_from(todo.total)
                    .map_err(|_| DecodeError::Invalid("summary todo total is too large".into()))?,
                current: todo.current,
            })
        })
        .transpose()?;
    let context = value
        .context
        .map(|context| {
            let source = match wire::ContextMeterSource::try_from(context.source) {
                Ok(wire::ContextMeterSource::AssistantUsage) => {
                    model::ContextMeterSource::AssistantUsage
                }
                Ok(wire::ContextMeterSource::ResultUsage) => model::ContextMeterSource::ResultUsage,
                Ok(wire::ContextMeterSource::AssistantContextUsage) => {
                    model::ContextMeterSource::AssistantContextUsage
                }
                Ok(wire::ContextMeterSource::CompactBoundary) => {
                    model::ContextMeterSource::CompactBoundary
                }
                _ => return Err(DecodeError::Invalid("invalid context meter source".into())),
            };
            Ok(model::ContextMeter {
                used_tokens: context.used_tokens,
                window_tokens: context.window_tokens,
                source,
            })
        })
        .transpose()?;
    let unknown = value
        .unknown
        .into_iter()
        .map(|field| match wire::SummaryField::try_from(field) {
            Ok(wire::SummaryField::Attention) => Ok(model::SummaryField::Attention),
            Ok(wire::SummaryField::Phase) => Ok(model::SummaryField::Phase),
            Ok(wire::SummaryField::LastActivity) => Ok(model::SummaryField::LastActivity),
            Ok(wire::SummaryField::Todo) => Ok(model::SummaryField::Todo),
            Ok(wire::SummaryField::Context) => Ok(model::SummaryField::Context),
            Ok(wire::SummaryField::Model) => Ok(model::SummaryField::Model),
            Ok(wire::SummaryField::Outstanding) => Ok(model::SummaryField::Outstanding),
            _ => Err(DecodeError::Invalid("invalid summary unknown field".into())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(model::SummaryEnvelope {
        through: value.through,
        producer_version: value.producer_version,
        observed_at,
        stale: value.stale,
        revision: value.revision,
        summary: model::Summary {
            attention,
            phase,
            last_activity,
            todo,
            context,
            model: value.model,
            unknown,
        },
    })
}

pub fn progress_to_wire(value: &model::Progress) -> wire::Progress {
    wire::Progress {
        through: value.through,
        at_unix_ms: value.at.timestamp_millis(),
        revision: value.revision,
    }
}

pub fn progress_from_wire(value: wire::Progress) -> Result<model::Progress, DecodeError> {
    let at = Utc
        .timestamp_millis_opt(value.at_unix_ms)
        .single()
        .ok_or_else(|| DecodeError::Invalid("invalid progress at".into()))?;
    Ok(model::Progress {
        through: value.through,
        at,
        revision: value.revision,
    })
}

pub fn agent_parent_from_wire(
    parent: wire::AgentParent,
) -> Result<model::AgentParent, DecodeError> {
    Ok(model::AgentParent {
        agent_id: uuid_from_bytes("parent.agent_id", parent.agent_id)?,
        host_id: uuid_from_bytes("parent.host_id", parent.host_id)?,
    })
}

fn working_on_from_wire(value: wire::WorkingOn) -> Result<model::WorkingOn, DecodeError> {
    let updated_at = Utc
        .timestamp_millis_opt(value.updated_at_unix_ms)
        .single()
        .ok_or_else(|| DecodeError::Invalid("invalid working_on.updated_at".into()))?;
    Ok(model::WorkingOn {
        text: value.text,
        updated_at,
    })
}

pub fn artifact_ref_from_wire(value: wire::ArtifactRef) -> Result<model::ArtifactRef, DecodeError> {
    Ok(model::ArtifactRef {
        id: model::ArtifactId::from_str(&value.id)
            .map_err(|error| DecodeError::Invalid(format!("ArtifactRef.id is invalid: {error}")))?,
        kind: artifact_kind_from_wire(value.kind)?,
        name: value.name,
        mime: value.mime,
        size: value.size,
    })
}

pub fn artifact_ref_to_wire(value: &model::ArtifactRef) -> wire::ArtifactRef {
    wire::ArtifactRef {
        id: value.id.to_string(),
        kind: artifact_kind_to_wire(value.kind) as i32,
        name: value.name.clone(),
        mime: value.mime.clone(),
        size: value.size,
    }
}

pub const fn artifact_kind_to_wire(kind: model::ArtifactKind) -> wire::ArtifactKind {
    match kind {
        model::ArtifactKind::Image => wire::ArtifactKind::Image,
        model::ArtifactKind::File => wire::ArtifactKind::File,
        model::ArtifactKind::Diff => wire::ArtifactKind::Diff,
    }
}

pub fn artifact_kind_from_wire(kind: i32) -> Result<model::ArtifactKind, DecodeError> {
    match wire::ArtifactKind::try_from(kind) {
        Ok(wire::ArtifactKind::Image) => Ok(model::ArtifactKind::Image),
        Ok(wire::ArtifactKind::File) => Ok(model::ArtifactKind::File),
        Ok(wire::ArtifactKind::Diff) => Ok(model::ArtifactKind::Diff),
        Ok(wire::ArtifactKind::Unspecified) | Err(_) => Err(DecodeError::Invalid(format!(
            "invalid ArtifactKind value {kind}"
        ))),
    }
}

pub fn diff_base_to_wire(base: &model::DiffBase) -> wire::DiffBase {
    wire::DiffBase {
        base: Some(match base {
            model::DiffBase::WorkingTree => wire::diff_base::Base::WorkingTree(wire::Empty {}),
            model::DiffBase::Branch { base } => wire::diff_base::Base::Branch(base.clone()),
        }),
    }
}

pub fn diff_base_from_wire(base: wire::DiffBase) -> Result<model::DiffBase, DecodeError> {
    match base.base {
        Some(wire::diff_base::Base::WorkingTree(_)) => Ok(model::DiffBase::WorkingTree),
        Some(wire::diff_base::Base::Branch(base)) if !base.is_empty() => {
            Ok(model::DiffBase::Branch { base })
        }
        Some(wire::diff_base::Base::Branch(_)) => Err(DecodeError::Invalid(
            "DiffBase.branch must not be empty".to_string(),
        )),
        None => Err(DecodeError::Invalid(
            "DiffBase.base is required".to_string(),
        )),
    }
}

pub fn diff_response_to_wire(response: &model::DiffResponse) -> wire::DiffResponse {
    wire::DiffResponse {
        artifact: Some(artifact_ref_to_wire(&response.artifact)),
        patch: response.patch.clone(),
        identity: Some(wire::BaseIdentity {
            base: Some(diff_base_to_wire(&response.identity.base)),
            head: response.identity.head.clone(),
            merge_base: response.identity.merge_base.clone(),
            blobs: response
                .identity
                .blobs
                .iter()
                .map(|(path, blob)| wire::PathBlob {
                    path: path.clone(),
                    blob: blob.clone(),
                })
                .collect(),
        }),
        files: response
            .files
            .iter()
            .map(|file| wire::DiffFile {
                path: file.path.clone(),
                added: file.added,
                removed: file.removed,
            })
            .collect(),
    }
}

pub fn diff_response_from_wire(
    value: wire::DiffResponse,
) -> Result<model::DiffResponse, DecodeError> {
    let artifact = value
        .artifact
        .ok_or_else(|| DecodeError::Invalid("DiffResponse.artifact is required".into()))?;
    let identity = value
        .identity
        .ok_or_else(|| DecodeError::Invalid("DiffResponse.identity is required".into()))?;
    let base = identity
        .base
        .ok_or_else(|| DecodeError::Invalid("BaseIdentity.base is required".into()))?;
    Ok(model::DiffResponse {
        artifact: artifact_ref_from_wire(artifact)?,
        patch: value.patch,
        identity: model::BaseIdentity {
            base: diff_base_from_wire(base)?,
            head: identity.head,
            merge_base: identity.merge_base,
            blobs: identity
                .blobs
                .into_iter()
                .map(|blob| (blob.path, blob.blob))
                .collect(),
        },
        files: value
            .files
            .into_iter()
            .map(|file| model::DiffFile {
                path: file.path,
                added: file.added,
                removed: file.removed,
            })
            .collect(),
    })
}

pub fn capabilities_from_wire(
    capabilities: Option<wire::Capabilities>,
) -> Result<model::Capabilities, DecodeError> {
    let Some(capabilities) = capabilities else {
        return Ok(model::Capabilities::default());
    };
    Ok(model::Capabilities {
        features: capabilities.features,
        supported_agent_types: capabilities
            .supported_agents
            .into_iter()
            .map(|agent| {
                let kind = wire::agent_kind_from_wire(agent.kind.ok_or_else(|| {
                    DecodeError::Invalid("SupportedAgentType missing kind".to_string())
                })?)?;
                Ok(model::SupportedAgentType {
                    agent_type: kind.provider().to_string(),
                })
            })
            .collect::<Result<Vec<_>, DecodeError>>()?,
    })
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

impl From<model::ListRepositoriesResponse> for wire::ListRepositoriesResponse {
    fn from(value: model::ListRepositoriesResponse) -> Self {
        fn entry(value: model::ProjectEntry) -> wire::ProjectEntry {
            wire::ProjectEntry {
                path: value.path.to_string_lossy().into_owned(),
                name: value.name,
                last_used_unix_ms: value.last_used.map(|time| time.timestamp_millis()),
            }
        }
        Self {
            recent: value.recent.into_iter().map(entry).collect(),
            repositories: value.repositories.into_iter().map(entry).collect(),
            roots: value
                .roots
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        }
    }
}

impl TryFrom<wire::ListRepositoriesResponse> for model::ListRepositoriesResponse {
    type Error = String;

    fn try_from(value: wire::ListRepositoriesResponse) -> Result<Self, Self::Error> {
        fn entry(value: wire::ProjectEntry) -> Result<model::ProjectEntry, String> {
            Ok(model::ProjectEntry {
                path: value.path.into(),
                name: value.name,
                last_used: value
                    .last_used_unix_ms
                    .map(|time| {
                        chrono::DateTime::from_timestamp_millis(time)
                            .ok_or_else(|| "invalid ProjectEntry.last_used_unix_ms".to_owned())
                    })
                    .transpose()?,
            })
        }
        Ok(Self {
            recent: value
                .recent
                .into_iter()
                .map(entry)
                .collect::<Result<_, _>>()?,
            repositories: value
                .repositories
                .into_iter()
                .map(entry)
                .collect::<Result<_, _>>()?,
            roots: value.roots.into_iter().map(PathBuf::from).collect(),
        })
    }
}

#[cfg(test)]
mod repository_tests {
    use super::*;

    #[test]
    fn repositories_invalid_wire_timestamp_is_a_decode_error() {
        let result = model::ListRepositoriesResponse::try_from(wire::ListRepositoriesResponse {
            recent: vec![wire::ProjectEntry {
                path: "/project".into(),
                name: "project".into(),
                last_used_unix_ms: Some(i64::MAX),
            }],
            ..Default::default()
        });
        assert_eq!(
            result.unwrap_err(),
            "invalid ProjectEntry.last_used_unix_ms"
        );
    }
}

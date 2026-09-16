use std::path::PathBuf;
use std::str::FromStr;

use chrono::{TimeZone, Utc};
use uuid::Uuid;

use crate::{self as wire, DecodeError};

pub fn agent_from_wire(agent: wire::Agent) -> Result<model::Agent, DecodeError> {
    let created_at = Utc
        .timestamp_millis_opt(agent.created_at_unix_ms)
        .single()
        .ok_or_else(|| DecodeError::Invalid("invalid agent created_at".into()))?;
    let parent = agent.parent.map(agent_parent_from_wire).transpose()?;
    let working_on = agent.working_on.map(working_on_from_wire).transpose()?;
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
        parent,
        working_on,
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

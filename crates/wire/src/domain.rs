use std::path::PathBuf;
use std::str::FromStr;

use chrono::{TimeZone, Utc};
use prost::Message;
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

fn agent_parent_from_wire(parent: wire::AgentParent) -> Result<model::AgentParent, DecodeError> {
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

fn diff_base_from_wire(base: wire::DiffBase) -> Result<model::DiffBase, DecodeError> {
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

pub fn subscribe_protocol_to_client_wire(
    protocol: model::Protocol,
    args: Option<&[u8]>,
) -> Result<wire::client_subscribe_session_request::Protocol, DecodeError> {
    use wire::client_subscribe_session_request::Protocol as P;
    Ok(match protocol {
        model::Protocol::TerminalV1 => P::TerminalV1(decode_optional(args, "TerminalV1Args")?),
        model::Protocol::ClaudePtyTranscriptV1 => {
            P::ClaudePtyTranscriptV1(decode_optional(args, "ClaudePtyTranscriptV1Args")?)
        }
        model::Protocol::ClaudeSdkV1 => P::ClaudeSdkV1(decode_optional(args, "ClaudeSdkV1Args")?),
        model::Protocol::CodexSdkV1 => P::CodexSdkV1(decode_optional(args, "CodexSdkV1Args")?),
        model::Protocol::TestEchoV1 => {
            reject_args(args, "TestEchoV1Args")?;
            P::TestEchoV1(wire::TestEchoV1Args {})
        }
    })
}

pub fn send_input_to_client_wire(
    protocol: model::Protocol,
    input_id: Vec<u8>,
    payload: &[u8],
) -> Result<(Vec<u8>, wire::client_send_input_request::Event), DecodeError> {
    use wire::client_send_input_request::Event;
    let event = match protocol {
        model::Protocol::TerminalV1 => Event::TerminalV1(wire::TerminalV1Input {
            payload: payload.to_vec(),
        }),
        model::Protocol::ClaudePtyTranscriptV1 => {
            Event::ClaudePtyTranscriptV1(decode(payload, "ClaudePtyTranscriptV1Input")?)
        }
        model::Protocol::ClaudeSdkV1 => Event::ClaudeSdkV1(decode(payload, "ClaudeSdkV1Input")?),
        model::Protocol::CodexSdkV1 => Event::CodexSdkV1(decode(payload, "CodexSdkV1Input")?),
        model::Protocol::TestEchoV1 => Event::TestEchoV1(wire::TestEchoV1Input {
            payload: payload.to_vec(),
        }),
    };
    Ok((input_id, event))
}

pub fn session_output_payload_from_wire(
    output: wire::SessionOutput,
) -> Result<Vec<u8>, DecodeError> {
    use wire::session_output::Output;
    Ok(
        match output
            .output
            .ok_or_else(|| DecodeError::Invalid("SessionOutput missing output".into()))?
        {
            Output::TerminalV1(output) => output.payload,
            Output::ClaudePtyTranscriptV1(output) => output.encode_to_vec(),
            Output::ClaudeSdkV1(output) => output.encode_to_vec(),
            Output::CodexSdkV1(output) => output.encode_to_vec(),
            Output::TestEchoV1(output) => output.payload,
        },
    )
}

fn decode<M: Message + Default>(bytes: &[u8], name: &str) -> Result<M, DecodeError> {
    M::decode(bytes).map_err(|error| DecodeError::Invalid(format!("invalid {name}: {error}")))
}

fn decode_optional<M: Message + Default>(
    bytes: Option<&[u8]>,
    name: &str,
) -> Result<M, DecodeError> {
    bytes.map_or_else(|| Ok(M::default()), |bytes| decode(bytes, name))
}

fn reject_args(args: Option<&[u8]>, name: &str) -> Result<(), DecodeError> {
    if args.is_some_and(|args| !args.is_empty()) {
        Err(DecodeError::Invalid(format!("{name} does not accept args")))
    } else {
        Ok(())
    }
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

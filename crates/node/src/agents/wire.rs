use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
#[cfg(test)]
use model::AgentKind;
use model::{ClaudeDriver, SessionArgs, SessionInput};
use protocol_wire::DeleteAgentRequest;
use uuid::Uuid;
use wire::{
    self as protocol_wire, agent_kind_from_wire, agent_kind_to_wire, claude_driver_from_wire, pb,
};

use super::{Agent, AgentParent, SessionCloseReason, SubscribeSessionEvent, WorkingOn};
use crate::agents::{RenameAgentRequest, TerminalSize};
use crate::envelope::{AgentSender, Envelope, EnvelopeKind, Sender};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscribeSessionRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) args: SessionArgs,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendInputRequest {
    pub agent_id: Uuid,
    pub input_id: Vec<u8>,
    pub input: SessionInput,
    pub pin: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CreateAgentRpcRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) name: Option<String>,
    pub(crate) parent: Option<AgentParent>,
    pub(crate) initial_prompt: Option<String>,
    pub(crate) agent: CreateAgentConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct SetAgentStatusRequest {
    pub(crate) agent_id: Uuid,
    pub(crate) working_on: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum CreateAgentConfig {
    Claude {
        driver: ClaudeDriver,
        working_dir: PathBuf,
        args: Vec<String>,
        terminal_size: Option<TerminalSize>,
    },
    Codex {
        cwd: PathBuf,
        model: Option<String>,
        approval_policy: Option<String>,
        sandbox_policy: Option<String>,
        resume_thread_id: Option<String>,
    },
    TestAgent {
        command: String,
        working_dir: PathBuf,
        terminal_size: Option<TerminalSize>,
    },
}

/// Encode one typed session event for either service's subscription stream.
pub(crate) fn session_event_to_wire(event: &SubscribeSessionEvent) -> pb::SubscribeSessionResponse {
    let event = match event {
        SubscribeSessionEvent::Opened { replay } => {
            pb::subscribe_session_response::Event::Opened(pb::SessionOpened {
                replay: replay.as_ref().map(protocol_wire::replay_facts_to_wire),
            })
        }
        SubscribeSessionEvent::Output(output) => pb::subscribe_session_response::Event::Output(
            protocol_wire::session_output_to_wire(output),
        ),
        SubscribeSessionEvent::ReplayComplete => {
            pb::subscribe_session_response::Event::ReplayComplete(pb::ReplayComplete {})
        }
        SubscribeSessionEvent::Closed { reason } => {
            pb::subscribe_session_response::Event::Closed(session_closed_to_wire(reason))
        }
    };
    pb::SubscribeSessionResponse { event: Some(event) }
}

pub(crate) fn session_closed_to_wire(reason: &SessionCloseReason) -> pb::SessionClosed {
    let reason = match reason {
        SessionCloseReason::AgentDeleted => {
            pb::session_closed::Reason::AgentDeleted(pb::AgentDeleted {})
        }
        SessionCloseReason::AgentExited { exit_code } => {
            pb::session_closed::Reason::AgentExited(pb::AgentExited {
                exit_code: *exit_code,
            })
        }
        SessionCloseReason::HostUnreachable => {
            pb::session_closed::Reason::HostUnreachable(pb::HostUnreachable {})
        }
        SessionCloseReason::Reset => pb::session_closed::Reason::Reset(pb::Reset {
            reason: "reset".to_string(),
        }),
        SessionCloseReason::InternalError { detail } => {
            pb::session_closed::Reason::InternalError(pb::InternalError {
                detail: detail.clone(),
            })
        }
    };
    pb::SessionClosed {
        reason: Some(reason),
    }
}

pub(crate) fn create_agent_request_from_wire(
    request: protocol_wire::CreateAgentRequest,
) -> Result<CreateAgentRpcRequest, protocol_wire::DecodeError> {
    let agent_id = required_uuid_from_bytes("agent_id", request.agent_id)?;
    let agent = request.agent.ok_or_else(|| {
        protocol_wire::DecodeError::Invalid("CreateAgentRequest missing agent".into())
    })?;

    let agent = match agent {
        protocol_wire::create_agent_request::Agent::Claude(claude) => CreateAgentConfig::Claude {
            driver: claude_driver_from_wire(claude.driver)?,
            working_dir: PathBuf::from(claude.working_dir),
            args: claude.args,
            terminal_size: claude
                .initial_terminal_size
                .map(terminal_size_from_wire)
                .transpose()?,
        },
        protocol_wire::create_agent_request::Agent::Codex(codex) => CreateAgentConfig::Codex {
            cwd: PathBuf::from(codex.cwd),
            model: codex.model,
            approval_policy: codex.approval_policy,
            sandbox_policy: codex.sandbox_policy,
            resume_thread_id: codex.resume_thread_id,
        },
        protocol_wire::create_agent_request::Agent::TestAgent(test_agent) => {
            CreateAgentConfig::TestAgent {
                command: test_agent.command,
                working_dir: PathBuf::from(test_agent.working_dir),
                terminal_size: test_agent
                    .initial_terminal_size
                    .map(terminal_size_from_wire)
                    .transpose()?,
            }
        }
    };

    Ok(CreateAgentRpcRequest {
        agent_id,
        name: request.name,
        parent: request.parent.map(agent_parent_from_wire).transpose()?,
        initial_prompt: request.initial_prompt,
        agent,
    })
}

pub(crate) fn set_agent_status_request_from_wire(
    request: protocol_wire::SetAgentStatusRequest,
) -> Result<SetAgentStatusRequest, protocol_wire::DecodeError> {
    Ok(SetAgentStatusRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        working_on: request.working_on,
    })
}

pub(crate) fn envelope_from_wire(
    envelope: protocol_wire::Envelope,
) -> Result<Envelope, protocol_wire::DecodeError> {
    let from = envelope
        .from
        .and_then(|sender| sender.value)
        .ok_or_else(|| protocol_wire::DecodeError::Invalid("Envelope missing from".into()))?;
    let from = match from {
        protocol_wire::sender::Value::Agent(agent) => Sender::Agent(AgentSender {
            agent_id: required_uuid_from_bytes("from.agent_id", agent.agent_id)?,
            host_id: required_uuid_from_bytes("from.host_id", agent.host_id)?,
            name: agent.name,
            kind: agent.kind,
        }),
        protocol_wire::sender::Value::Human(_) => Sender::Human,
    };
    let kind = match protocol_wire::EnvelopeKind::try_from(envelope.kind) {
        Ok(protocol_wire::EnvelopeKind::Message) => EnvelopeKind::Message,
        Ok(protocol_wire::EnvelopeKind::Completed) => EnvelopeKind::Completed,
        Ok(protocol_wire::EnvelopeKind::Exited) => EnvelopeKind::Exited,
        Ok(protocol_wire::EnvelopeKind::Unspecified) | Err(_) => {
            return Err(protocol_wire::DecodeError::Invalid(
                "Envelope kind must be specified".into(),
            ));
        }
    };
    Ok(Envelope {
        id: required_uuid_from_bytes("id", envelope.id)?,
        context: envelope
            .context
            .map(|context| required_uuid_from_bytes("context", context))
            .transpose()?,
        from,
        to: envelope
            .to
            .map(agent_parent_from_wire)
            .transpose()?
            .ok_or_else(|| protocol_wire::DecodeError::Invalid("Envelope missing to".into()))?,
        kind,
        text: envelope.text,
    })
}

pub(crate) fn envelope_to_wire(envelope: &Envelope) -> protocol_wire::Envelope {
    let value = match &envelope.from {
        Sender::Agent(agent) => protocol_wire::sender::Value::Agent(protocol_wire::AgentSender {
            agent_id: uuid_to_bytes(agent.agent_id),
            host_id: uuid_to_bytes(agent.host_id),
            name: agent.name.clone(),
            kind: agent.kind.clone(),
        }),
        Sender::Human => protocol_wire::sender::Value::Human(protocol_wire::Human {}),
    };
    let kind = match envelope.kind {
        EnvelopeKind::Message => protocol_wire::EnvelopeKind::Message,
        EnvelopeKind::Completed => protocol_wire::EnvelopeKind::Completed,
        EnvelopeKind::Exited => protocol_wire::EnvelopeKind::Exited,
    };
    protocol_wire::Envelope {
        id: uuid_to_bytes(envelope.id),
        context: envelope.context.map(uuid_to_bytes),
        from: Some(protocol_wire::Sender { value: Some(value) }),
        to: Some(agent_parent_to_wire(envelope.to)),
        kind: kind as i32,
        text: envelope.text.clone(),
    }
}

pub(crate) fn rename_agent_request_from_wire(
    request: protocol_wire::RenameAgentRequest,
) -> Result<RenameAgentRequest, protocol_wire::DecodeError> {
    if request.name.is_empty() {
        return Err(protocol_wire::DecodeError::Invalid(
            "RenameAgentRequest.name must not be empty".into(),
        ));
    }
    Ok(RenameAgentRequest {
        agent_id: required_uuid_from_bytes("agent_id", request.agent_id)?,
        name: request.name,
    })
}

pub(crate) fn delete_agent_id_from_wire(
    request: DeleteAgentRequest,
) -> Result<Uuid, protocol_wire::DecodeError> {
    required_uuid_from_bytes("agent_id", request.agent_id)
}

pub(crate) fn agent_to_wire(
    agent: &Agent,
) -> Result<protocol_wire::Agent, protocol_wire::EncodeError> {
    Ok(protocol_wire::Agent {
        agent_id: uuid_to_bytes(agent.id),
        host_id: uuid_to_bytes(agent.host_id),
        name: agent.name.clone(),
        command: agent.command.clone(),
        working_dir: path_to_proto_string("Agent.working_dir", &agent.working_dir)?,
        kind: Some(agent_kind_to_wire(agent.kind)),
        readonly: agent.readonly,
        args: agent.args.clone(),
        created_at_unix_ms: agent.created_at.timestamp_millis(),
        parent: agent.parent.map(agent_parent_to_wire),
        working_on: agent.working_on.as_ref().map(working_on_to_wire),
        inventory_revision: agent.inventory_revision,
    })
}

pub fn agent_from_wire(agent: protocol_wire::Agent) -> Result<Agent, protocol_wire::DecodeError> {
    let created_at = Utc
        .timestamp_millis_opt(agent.created_at_unix_ms)
        .single()
        .ok_or_else(|| protocol_wire::DecodeError::Invalid("invalid agent created_at".into()))?;

    let parent = agent.parent.map(agent_parent_from_wire).transpose()?;
    let working_on = agent.working_on.map(working_on_from_wire).transpose()?;

    let kind = agent_kind_from_wire(
        agent
            .kind
            .ok_or_else(|| protocol_wire::DecodeError::Invalid("Agent missing kind".into()))?,
    )?;

    Ok(Agent {
        id: required_uuid_from_bytes("agent_id", agent.agent_id)?,
        host_id: required_uuid_from_bytes("host_id", agent.host_id)?,
        name: agent.name,
        command: agent.command,
        working_dir: PathBuf::from(agent.working_dir),
        kind,
        readonly: agent.readonly,
        args: agent.args,
        created_at,
        parent,
        working_on,
        inventory_revision: agent.inventory_revision,
    })
}

pub(crate) fn agent_parent_to_wire(parent: AgentParent) -> protocol_wire::AgentParent {
    protocol_wire::AgentParent {
        agent_id: uuid_to_bytes(parent.agent_id),
        host_id: uuid_to_bytes(parent.host_id),
    }
}

pub(crate) fn agent_parent_from_wire(
    parent: protocol_wire::AgentParent,
) -> Result<AgentParent, protocol_wire::DecodeError> {
    Ok(AgentParent {
        agent_id: required_uuid_from_bytes("parent.agent_id", parent.agent_id)?,
        host_id: required_uuid_from_bytes("parent.host_id", parent.host_id)?,
    })
}

fn working_on_to_wire(working_on: &WorkingOn) -> protocol_wire::WorkingOn {
    protocol_wire::WorkingOn {
        text: working_on.text.clone(),
        updated_at_unix_ms: working_on.updated_at.timestamp_millis(),
    }
}

fn working_on_from_wire(
    working_on: protocol_wire::WorkingOn,
) -> Result<WorkingOn, protocol_wire::DecodeError> {
    let updated_at = Utc
        .timestamp_millis_opt(working_on.updated_at_unix_ms)
        .single()
        .ok_or_else(|| {
            protocol_wire::DecodeError::Invalid("invalid working_on.updated_at".into())
        })?;
    Ok(WorkingOn {
        text: working_on.text,
        updated_at,
    })
}

pub(crate) fn path_to_proto_string(
    field: &'static str,
    path: &Path,
) -> Result<String, protocol_wire::EncodeError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| protocol_wire::EncodeError::Invalid(format!("{field} must be valid UTF-8")))
}

fn terminal_size_from_wire(
    size: protocol_wire::TerminalSize,
) -> Result<TerminalSize, protocol_wire::DecodeError> {
    Ok(TerminalSize {
        rows: size.rows.try_into().map_err(|_| {
            protocol_wire::DecodeError::Invalid(format!(
                "terminal rows out of range: {}",
                size.rows
            ))
        })?,
        cols: size.cols.try_into().map_err(|_| {
            protocol_wire::DecodeError::Invalid(format!(
                "terminal cols out of range: {}",
                size.cols
            ))
        })?,
    })
}

fn uuid_to_bytes(uuid: Uuid) -> Vec<u8> {
    uuid.as_bytes().to_vec()
}

fn required_uuid_from_bytes(
    name: &str,
    bytes: Vec<u8>,
) -> Result<Uuid, protocol_wire::DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        protocol_wire::DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_session_close_reason_encodes_to_wire() {
        for (reason, expected) in [
            (
                SessionCloseReason::AgentDeleted,
                protocol_wire::session_closed::Reason::AgentDeleted(protocol_wire::AgentDeleted {}),
            ),
            (
                SessionCloseReason::AgentExited {
                    exit_code: Some(17),
                },
                protocol_wire::session_closed::Reason::AgentExited(protocol_wire::AgentExited {
                    exit_code: Some(17),
                }),
            ),
            (
                SessionCloseReason::HostUnreachable,
                protocol_wire::session_closed::Reason::HostUnreachable(
                    protocol_wire::HostUnreachable {},
                ),
            ),
            (
                SessionCloseReason::Reset,
                protocol_wire::session_closed::Reason::Reset(protocol_wire::Reset {
                    reason: "reset".into(),
                }),
            ),
            (
                SessionCloseReason::InternalError {
                    detail: "stream failed".into(),
                },
                protocol_wire::session_closed::Reason::InternalError(
                    protocol_wire::InternalError {
                        detail: "stream failed".into(),
                    },
                ),
            ),
        ] {
            assert_eq!(session_closed_to_wire(&reason).reason, Some(expected));
        }
    }

    #[test]
    fn agent_kinds_roundtrip_every_variant() {
        for kind in [
            AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
            AgentKind::Codex,
            AgentKind::TestAgent,
        ] {
            assert_eq!(
                agent_kind_from_wire(agent_kind_to_wire(kind)).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn a2a_record_roundtrip() {
        let parent = AgentParent {
            agent_id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
        };
        let updated_at = Utc.timestamp_millis_opt(1_777_777_777_777).unwrap();
        let dto = Agent {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: Some("child".to_string()),
            command: "codex".to_string(),
            working_dir: PathBuf::from("/tmp/work"),
            kind: AgentKind::Codex,
            readonly: false,
            args: vec!["--model".to_string(), "gpt-5.6".to_string()],
            created_at: Utc.timestamp_millis_opt(1_700_000_000_000).unwrap(),
            parent: Some(parent),
            working_on: Some(WorkingOn {
                text: "implement the record".to_string(),
                updated_at,
            }),
            inventory_revision: 7,
        };

        let wire = agent_to_wire(&dto).unwrap();
        let decoded = agent_from_wire(wire).unwrap();

        assert_eq!(decoded, dto);
        assert_eq!(decoded.parent, Some(parent));
        assert_eq!(decoded.working_on.unwrap().updated_at, updated_at);
    }

    #[test]
    fn create_agent_request_decodes_claude_create_config() {
        let agent_id = Uuid::new_v4();
        let parent = AgentParent {
            agent_id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
        };
        let request = protocol_wire::CreateAgentRequest {
            agent_id: uuid_to_bytes(agent_id),
            name: Some("dev".to_string()),
            parent: Some(agent_parent_to_wire(parent)),
            initial_prompt: Some("start here".to_string()),
            agent: Some(protocol_wire::create_agent_request::Agent::Claude(
                protocol_wire::ClaudeCreateConfig {
                    working_dir: "/tmp/work".to_string(),
                    args: vec!["--resume".to_string(), "abc".to_string()],
                    initial_terminal_size: Some(protocol_wire::TerminalSize {
                        rows: 40,
                        cols: 120,
                    }),
                    driver: protocol_wire::ClaudeDriver::Sdk as i32,
                },
            )),
        };

        let decoded = create_agent_request_from_wire(request).unwrap();
        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.name.as_deref(), Some("dev"));
        assert_eq!(decoded.parent, Some(parent));
        assert_eq!(decoded.initial_prompt.as_deref(), Some("start here"));
        let CreateAgentConfig::Claude {
            driver,
            working_dir,
            args,
            terminal_size,
        } = decoded.agent
        else {
            panic!("expected Claude create config");
        };
        assert_eq!(driver, ClaudeDriver::Sdk);
        assert_eq!(working_dir, PathBuf::from("/tmp/work"));
        assert_eq!(args, ["--resume", "abc"]);
        assert_eq!(
            terminal_size,
            Some(TerminalSize {
                rows: 40,
                cols: 120
            }),
        );
    }

    #[test]
    fn create_agent_request_decodes_codex_create_config() {
        let agent_id = Uuid::new_v4();
        let request = protocol_wire::CreateAgentRequest {
            agent_id: uuid_to_bytes(agent_id),
            name: Some("codex-dev".to_string()),
            parent: None,
            initial_prompt: None,
            agent: Some(protocol_wire::create_agent_request::Agent::Codex(
                protocol_wire::CodexCreateConfig {
                    cwd: "/tmp/work".to_string(),
                    model: Some("gpt-5.6-sol".to_string()),
                    approval_policy: Some("on-request".to_string()),
                    sandbox_policy: Some("workspace-write".to_string()),
                    resume_thread_id: Some("thread-7".to_string()),
                },
            )),
        };

        let decoded = create_agent_request_from_wire(request).unwrap();
        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.name.as_deref(), Some("codex-dev"));
        assert!(matches!(
            decoded.agent,
            CreateAgentConfig::Codex {
                ref cwd,
                ref model,
                ref approval_policy,
                ref sandbox_policy,
                ref resume_thread_id,
            } if cwd == Path::new("/tmp/work")
                && model.as_deref() == Some("gpt-5.6-sol")
                && approval_policy.as_deref() == Some("on-request")
                && sandbox_policy.as_deref() == Some("workspace-write")
                && resume_thread_id.as_deref() == Some("thread-7")
        ));
    }

    #[test]
    fn create_agent_request_rejects_empty_agent_id() {
        let request = protocol_wire::CreateAgentRequest {
            agent_id: Vec::new(),
            name: None,
            parent: None,
            initial_prompt: None,
            agent: Some(protocol_wire::create_agent_request::Agent::Claude(
                protocol_wire::ClaudeCreateConfig {
                    working_dir: "/tmp/work".to_string(),
                    args: Vec::new(),
                    initial_terminal_size: None,
                    driver: protocol_wire::ClaudeDriver::Pty as i32,
                },
            )),
        };

        let error = create_agent_request_from_wire(request).unwrap_err();
        assert!(error.to_string().contains("agent_id must be 16 bytes"));
    }

    #[test]
    fn test_agent_request_decodes_to_dispatchable_variant() {
        let request = protocol_wire::CreateAgentRequest {
            agent_id: uuid_to_bytes(Uuid::new_v4()),
            name: None,
            parent: None,
            initial_prompt: None,
            agent: Some(protocol_wire::create_agent_request::Agent::TestAgent(
                protocol_wire::TestAgentCreateConfig {
                    command: "/tmp/test-agent".to_string(),
                    working_dir: "/tmp/work".to_string(),
                    initial_terminal_size: Some(protocol_wire::TerminalSize { rows: 24, cols: 80 }),
                },
            )),
        };

        let decoded = create_agent_request_from_wire(request).unwrap();
        let CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } = decoded.agent
        else {
            panic!("expected test agent config");
        };

        assert_eq!(command, "/tmp/test-agent");
        assert_eq!(working_dir, PathBuf::from("/tmp/work"));
        assert_eq!(terminal_size, Some(TerminalSize { rows: 24, cols: 80 }));
    }

    #[cfg(unix)]
    #[test]
    fn agent_to_wire_rejects_non_utf8_working_dir() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        use chrono::Utc;

        let agent = Agent {
            id: Uuid::new_v4(),
            host_id: Uuid::new_v4(),
            name: None,
            command: "claude".to_string(),
            working_dir: PathBuf::from(OsString::from_vec(vec![0xff])),
            kind: AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            },
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
            parent: None,
            working_on: None,
            inventory_revision: 0,
        };

        let err = agent_to_wire(&agent).unwrap_err();
        assert!(
            err.to_string().contains("working_dir must be valid UTF-8"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rename_and_delete_requests_roundtrip() {
        let rename_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();

        let decoded = rename_agent_request_from_wire(protocol_wire::RenameAgentRequest {
            agent_id: uuid_to_bytes(rename_id),
            name: "renamed".to_string(),
        })
        .unwrap();
        assert_eq!(decoded.agent_id, rename_id);
        assert_eq!(decoded.name, "renamed");

        let agent_id = delete_agent_id_from_wire(DeleteAgentRequest {
            agent_id: uuid_to_bytes(delete_id),
        })
        .unwrap();
        assert_eq!(agent_id, delete_id);
    }

    #[test]
    fn rename_request_rejects_empty_name() {
        let request = protocol_wire::RenameAgentRequest {
            agent_id: uuid_to_bytes(Uuid::new_v4()),
            name: String::new(),
        };

        let error = rename_agent_request_from_wire(request).unwrap_err();

        assert!(error.to_string().contains("name must not be empty"));
    }

    #[test]
    fn delete_request_rejects_invalid_uuid_length() {
        let request = DeleteAgentRequest {
            agent_id: vec![1, 2, 3],
        };

        let err = delete_agent_id_from_wire(request).unwrap_err();
        assert!(
            err.to_string().contains("agent_id must be 16 bytes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn status_request_decodes_optional_work() {
        let agent_id = Uuid::new_v4();
        let decoded = set_agent_status_request_from_wire(protocol_wire::SetAgentStatusRequest {
            agent_id: uuid_to_bytes(agent_id),
            working_on: Some("checking protocol".to_string()),
        })
        .unwrap();

        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.working_on.as_deref(), Some("checking protocol"));
    }
}

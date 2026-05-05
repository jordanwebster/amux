use uuid::Uuid;

use crate::protocol::message::{CreateAgentRequest, ProtocolError, RenameAgentRequest};
use crate::protocol::{Route, method, wire};

#[derive(Debug, thiserror::Error)]
pub enum AgentLifecycleCodecError {
    #[error("failed to encode agent lifecycle frame: {0}")]
    Encode(String),
    #[error("failed to decode agent lifecycle frame: {0}")]
    Decode(String),
}

pub fn encode_create_agent_request(
    request: &CreateAgentRequest,
) -> Result<Vec<u8>, AgentLifecycleCodecError> {
    let request = wire::CreateAgentRpcRequest::from_domain(request)
        .map_err(|error| AgentLifecycleCodecError::Encode(error.to_string()))?;
    wire::encode_agent_lifecycle_request(&wire::AgentLifecycleRequest::Create(request))
        .map_err(|error| AgentLifecycleCodecError::Encode(error.to_string()))
}

pub fn decode_create_agent_response(
    payload: &[u8],
    route: Route,
) -> Result<Result<crate::protocol::Agent, ProtocolError>, AgentLifecycleCodecError> {
    match wire::decode_agent_lifecycle_response(method::AGENT_CREATE_NAME, payload)
        .map_err(|error| AgentLifecycleCodecError::Decode(error.to_string()))?
    {
        wire::AgentLifecycleResponse::Create(Ok(agent)) => agent
            .into_agent(route)
            .map(Ok)
            .map_err(|error| AgentLifecycleCodecError::Decode(error.to_string())),
        wire::AgentLifecycleResponse::Create(Err(error)) => Ok(Err(error)),
        response => Err(AgentLifecycleCodecError::Decode(format!(
            "expected CreateAgent response, got {}",
            response.method_name()
        ))),
    }
}

pub fn encode_rename_agent_request(
    request: &RenameAgentRequest,
) -> Result<Vec<u8>, AgentLifecycleCodecError> {
    wire::encode_agent_lifecycle_request(&wire::AgentLifecycleRequest::Rename(request.clone()))
        .map_err(|error| AgentLifecycleCodecError::Encode(error.to_string()))
}

pub fn decode_rename_agent_response(
    payload: &[u8],
    route: crate::protocol::Route,
) -> Result<Result<crate::protocol::Agent, ProtocolError>, AgentLifecycleCodecError> {
    match wire::decode_agent_lifecycle_response(method::AGENT_RENAME_NAME, payload)
        .map_err(|error| AgentLifecycleCodecError::Decode(error.to_string()))?
    {
        wire::AgentLifecycleResponse::Rename(Ok(agent)) => agent
            .into_agent(route)
            .map(Ok)
            .map_err(|error| AgentLifecycleCodecError::Decode(error.to_string())),
        wire::AgentLifecycleResponse::Rename(Err(error)) => Ok(Err(error)),
        response => Err(AgentLifecycleCodecError::Decode(format!(
            "expected RenameAgent response, got {}",
            response.method_name()
        ))),
    }
}

pub fn encode_delete_agent_request(agent_id: Uuid) -> Result<Vec<u8>, AgentLifecycleCodecError> {
    wire::encode_agent_lifecycle_request(&wire::AgentLifecycleRequest::Delete { agent_id })
        .map_err(|error| AgentLifecycleCodecError::Encode(error.to_string()))
}

pub fn decode_delete_agent_response(
    payload: &[u8],
) -> Result<Result<(), ProtocolError>, AgentLifecycleCodecError> {
    match wire::decode_agent_lifecycle_response(method::AGENT_DELETE_NAME, payload)
        .map_err(|error| AgentLifecycleCodecError::Decode(error.to_string()))?
    {
        wire::AgentLifecycleResponse::Delete(result) => Ok(result),
        response => Err(AgentLifecycleCodecError::Decode(format!(
            "expected DeleteAgent response, got {}",
            response.method_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use uuid::Uuid;

    use super::*;
    use crate::agent::claude::io as claude_io;
    use crate::protocol::message::{AgentType, TerminalSize};
    use crate::protocol::wire::{self, AgentLifecycleRequest, CreateAgentConfig};

    #[test]
    fn create_agent_request_encodes_as_protobuf_lifecycle_payload() {
        let agent_id = Uuid::new_v4();
        let request = CreateAgentRequest {
            agent_id,
            name: Some("worker".to_string()),
            agent_type: AgentType::Claude,
            working_dir: PathBuf::from("/tmp/project"),
            terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
            args: vec!["--continue".to_string()],
        };

        let payload = encode_create_agent_request(&request).unwrap();
        let decoded = wire::decode_agent_lifecycle_request_if_present(&payload)
            .unwrap()
            .expect("agent lifecycle request should be recognized");

        let AgentLifecycleRequest::Create(decoded) = decoded else {
            panic!("expected CreateAgent request");
        };
        assert_eq!(decoded.agent_id, Some(agent_id));
        assert_eq!(decoded.name.as_deref(), Some("worker"));
        let CreateAgentConfig::ClaudePty {
            working_dir,
            args,
            terminal_size,
        } = decoded.agent
        else {
            panic!("expected Claude PTY config");
        };
        assert_eq!(working_dir, PathBuf::from("/tmp/project"));
        assert_eq!(args, vec!["--continue".to_string()]);
        assert_eq!(terminal_size, Some(TerminalSize { rows: 24, cols: 80 }));
    }

    #[test]
    fn create_agent_response_decodes_success_and_error() {
        let success = wire::encode_agent_lifecycle_response(&wire::AgentLifecycleResponse::Create(
            Ok(wire::AgentRecord {
                id: Uuid::new_v4(),
                host_id: Uuid::new_v4(),
                name: Some("worker".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp/project"),
                agent_type: "claude".to_string(),
                io_protocols: vec![claude_io::RAW_V1.to_string()],
                readonly: false,
                args: Vec::new(),
                created_at_unix_ms: 0,
            }),
        ))
        .unwrap();
        let decoded = decode_create_agent_response(&success, Route::empty())
            .unwrap()
            .unwrap();
        assert_eq!(decoded.name.as_deref(), Some("worker"));
        assert_eq!(decoded.route, Route::empty());

        let error = wire::encode_agent_lifecycle_response(&wire::AgentLifecycleResponse::Create(
            Err(ProtocolError::AlreadyExists {
                message: "agent already exists".to_string(),
            }),
        ))
        .unwrap();
        let Err(ProtocolError::AlreadyExists { message }) =
            decode_create_agent_response(&error, Route::empty()).unwrap()
        else {
            panic!("expected AlreadyExists error");
        };
        assert_eq!(message, "agent already exists");
    }

    #[test]
    fn test_agent_create_request_preserves_working_dir() {
        let agent_id = Uuid::new_v4();
        let request = CreateAgentRequest {
            agent_id,
            name: Some("test".to_string()),
            agent_type: AgentType::TestAgent {
                command: "/tmp/test-agent".to_string(),
            },
            working_dir: PathBuf::from("/tmp/test-work"),
            terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
            args: Vec::new(),
        };

        let payload = encode_create_agent_request(&request).unwrap();
        let decoded = wire::decode_agent_lifecycle_request_if_present(&payload)
            .unwrap()
            .expect("agent lifecycle request should be recognized");

        let AgentLifecycleRequest::Create(decoded) = decoded else {
            panic!("expected CreateAgent request");
        };
        let CreateAgentConfig::TestAgent {
            command,
            working_dir,
            terminal_size,
        } = decoded.agent
        else {
            panic!("expected TestAgent config");
        };
        assert_eq!(decoded.agent_id, Some(agent_id));
        assert_eq!(command, "/tmp/test-agent");
        assert_eq!(working_dir, PathBuf::from("/tmp/test-work"));
        assert_eq!(terminal_size, Some(TerminalSize { rows: 24, cols: 80 }));
    }
}

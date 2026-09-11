//! Committed protobuf messages and codecs for amux protocol boundaries.

mod domain;
mod error;
mod provider;

/// Protocol version for the generated `LinkService.Connect` handshake.
pub const PROTOCOL_VERSION: u32 = 1;

pub use domain::{
    agent_from_wire, agent_parent_from_wire, artifact_kind_from_wire, artifact_kind_to_wire,
    artifact_ref_from_wire, artifact_ref_to_wire, capabilities_from_wire, diff_base_from_wire,
    diff_base_to_wire, diff_response_from_wire, diff_response_to_wire, send_input_to_client_wire,
    session_output_payload_from_wire, subscribe_protocol_to_client_wire,
};
pub use error::{
    DecodeError, EncodeError, decode_protocol_error, encode_protocol_error,
    protocol_error_from_status_details, protocol_status, protocol_version_mismatch_error,
};
pub use provider::{
    decode_claude_pty_args, decode_claude_pty_input, decode_claude_pty_output,
    decode_claude_sdk_args, decode_claude_sdk_input, decode_claude_sdk_output,
    decode_codex_sdk_args, decode_codex_sdk_input, decode_codex_sdk_output,
    decode_terminal_args, decode_terminal_control, encode_provider_cursor, encode_provider_output,
    encode_terminal_args,
    encode_claude_pty_args, encode_claude_pty_input, encode_claude_sdk_args,
    encode_claude_sdk_input, encode_codex_sdk_args, encode_codex_sdk_input,
};

pub mod amux {
    pub mod v1 {
        #![allow(dead_code, clippy::enum_variant_names)]
        include!("generated/amux.v1.rs");
    }
}

pub use amux::v1::*;

pub mod pb {
    pub use super::amux::v1::*;
}

pub const MESSAGE_SIZE_LIMIT: usize = 16 * 1024 * 1024;

pub fn agent_service_client(
    channel: tonic::transport::Channel,
) -> agent_service_client::AgentServiceClient<tonic::transport::Channel> {
    agent_service_client::AgentServiceClient::new(channel)
        .max_decoding_message_size(MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(MESSAGE_SIZE_LIMIT)
}

pub fn client_service_client(
    channel: tonic::transport::Channel,
) -> client_service_client::ClientServiceClient<tonic::transport::Channel> {
    client_service_client::ClientServiceClient::new(channel)
        .max_decoding_message_size(MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(MESSAGE_SIZE_LIMIT)
}

pub fn agent_service_server<T>(service: T) -> agent_service_server::AgentServiceServer<T>
where
    T: agent_service_server::AgentService,
{
    agent_service_server::AgentServiceServer::new(service)
        .max_decoding_message_size(MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(MESSAGE_SIZE_LIMIT)
}

pub fn client_service_server<T>(service: T) -> client_service_server::ClientServiceServer<T>
where
    T: client_service_server::ClientService,
{
    client_service_server::ClientServiceServer::new(service)
        .max_decoding_message_size(MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(MESSAGE_SIZE_LIMIT)
}

pub fn agent_kind_to_wire(kind: model::AgentKind) -> AgentKind {
    let kind = match kind {
        model::AgentKind::Claude { driver } => agent_kind::Kind::Claude(ClaudeKind {
            driver: claude_driver_to_wire(driver) as i32,
        }),
        model::AgentKind::Codex => agent_kind::Kind::Codex(CodexKind {}),
        model::AgentKind::TestAgent => agent_kind::Kind::TestAgent(TestAgentKind {}),
    };
    AgentKind { kind: Some(kind) }
}

pub fn agent_kind_from_wire(kind: AgentKind) -> Result<model::AgentKind, DecodeError> {
    let kind = kind
        .kind
        .ok_or_else(|| DecodeError::Invalid("AgentKind missing kind".into()))?;
    Ok(match kind {
        agent_kind::Kind::Claude(claude) => model::AgentKind::Claude {
            driver: claude_driver_from_wire(claude.driver)?,
        },
        agent_kind::Kind::Codex(_) => model::AgentKind::Codex,
        agent_kind::Kind::TestAgent(_) => model::AgentKind::TestAgent,
    })
}

pub const fn claude_driver_to_wire(driver: model::ClaudeDriver) -> ClaudeDriver {
    match driver {
        model::ClaudeDriver::Pty => ClaudeDriver::Pty,
        model::ClaudeDriver::Sdk => ClaudeDriver::Sdk,
    }
}

pub fn claude_driver_from_wire(driver: i32) -> Result<model::ClaudeDriver, DecodeError> {
    match ClaudeDriver::try_from(driver) {
        Ok(ClaudeDriver::Pty) => Ok(model::ClaudeDriver::Pty),
        Ok(ClaudeDriver::Sdk) => Ok(model::ClaudeDriver::Sdk),
        Ok(ClaudeDriver::Unspecified) | Err(_) => Err(DecodeError::Invalid(
            "ClaudeDriver must be specified".into(),
        )),
    }
}

pub const DESCRIPTOR_SET: &[u8] = include_bytes!("generated/amux.v1.bin");

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::DESCRIPTOR_SET;

    #[test]
    fn descriptor_set_contains_declared_services() {
        let descriptor = prost_types::FileDescriptorSet::decode(DESCRIPTOR_SET)
            .expect("descriptor set should decode");
        let services = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.service.iter())
            .filter_map(|service| service.name.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            services,
            std::collections::BTreeSet::from([
                "AgentService",
                "ClientService",
                "InstallationService",
                "LinkService",
                "PairingService",
                "ProfileService",
            ])
        );
    }
}

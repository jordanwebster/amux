//! Committed protobuf messages and codecs for amux protocol boundaries.

mod domain;
mod error;
mod provider;

/// Protocol version for the native-stream link handshake.
pub const PROTOCOL_VERSION: u32 = 2;

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
    decode_codex_sdk_args, decode_codex_sdk_input, decode_codex_sdk_output, decode_provider_cursor,
    decode_terminal_args, decode_terminal_control, encode_claude_pty_args, encode_claude_pty_input,
    encode_claude_sdk_args, encode_claude_sdk_input, encode_codex_sdk_args, encode_codex_sdk_input,
    encode_provider_cursor, encode_provider_output, encode_terminal_args,
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

/// Bound for link-control messages and application-stream prefaces.
pub const MESSAGE_SIZE_LIMIT: usize = 16 * 1024 * 1024;
/// RPC payloads include artifacts plus protobuf framing overhead.
const CHANNEL_MESSAGE_SIZE_LIMIT: usize = 64 * 1024 * 1024;

pub fn agent_service_client(
    channel: tonic::transport::Channel,
) -> agent_service_client::AgentServiceClient<tonic::transport::Channel> {
    agent_service_client::AgentServiceClient::new(channel)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

pub fn client_service_client(
    channel: tonic::transport::Channel,
) -> client_service_client::ClientServiceClient<tonic::transport::Channel> {
    client_service_client::ClientServiceClient::new(channel)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

pub fn agent_service_server<T>(service: T) -> agent_service_server::AgentServiceServer<T>
where
    T: agent_service_server::AgentService,
{
    agent_service_server::AgentServiceServer::new(service)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

pub fn client_service_server<T>(service: T) -> client_service_server::ClientServiceServer<T>
where
    T: client_service_server::ClientService,
{
    client_service_server::ClientServiceServer::new(service)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
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
    fn descriptor_set_contains_core_protocol_messages_and_services() {
        let descriptor = prost_types::FileDescriptorSet::decode(DESCRIPTOR_SET)
            .expect("descriptor set should decode");
        let message_names = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.message_type.iter())
            .filter_map(|message| message.name.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        let service_names = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.service.iter())
            .filter_map(|service| service.name.as_deref())
            .collect::<std::collections::BTreeSet<_>>();

        for message_name in [
            "Message",
            "Hello",
            "HelloAck",
            "NeighborUp",
            "NeighborDown",
            "StreamPreface",
            "BeginPairRequest",
            "PendingPairResponse",
            "TrustSshPeerRequest",
            "PairMessage",
            "PairingComplete",
            "PairingError",
            "PairingIdentity",
            "AgentUpdated",
            "AgentKind",
            "ProtocolNotExposed",
            "ArtifactRef",
            "DiffBase",
            "BaseIdentity",
            "DiffFile",
            "AttachmentMissing",
            "AttachmentTooLarge",
            "ArtifactCorrupt",
            "DiffUnavailable",
            "ClaudeKind",
            "CodexKind",
            "TestAgentKind",
            "TerminalV1Args",
            "TerminalV1Input",
            "TerminalV1Output",
            "ClaudeSdkV1Args",
            "ClaudeSdkV1Input",
            "ClaudeSdkV1Output",
            "CodexCreateConfig",
            "CodexSdkV1Args",
            "CodexSdkV1Input",
            "CodexSdkV1Output",
            "TestEchoV1Args",
            "TestEchoV1Input",
            "TestEchoV1Output",
            "SessionClosed",
            "Reauth",
            "LinkClose",
        ] {
            assert!(
                message_names.contains(message_name),
                "{message_name} should be in the descriptor"
            );
        }

        let expected_services = std::collections::BTreeSet::from([
            "AgentService",
            "ClientService",
            "PairingService",
            "ProfileService",
            "InstallationService",
        ]);
        assert_eq!(service_names, expected_services);

        let service_methods = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.service.iter())
            .map(|service| {
                (
                    service.name.as_deref().unwrap_or_default(),
                    service
                        .method
                        .iter()
                        .filter_map(|method| method.name.as_deref())
                        .collect::<std::collections::BTreeSet<_>>(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            service_methods.get("PairingService").cloned(),
            Some(std::collections::BTreeSet::from(["Pair"]))
        );
        assert_eq!(
            service_methods.get("AgentService").cloned(),
            Some(std::collections::BTreeSet::from([
                "CreateAgent",
                "DeleteAgent",
                "Diff",
                "GetArtifact",
                "PutArtifact",
                "RenameAgent",
                "SendInput",
                "SendMessage",
                "SetAgentStatus",
                "SubscribeAgentEvents",
                "SubscribeSession",
            ]))
        );
        assert_eq!(
            service_methods.get("ClientService").cloned(),
            Some(std::collections::BTreeSet::from([
                "CreateAgent",
                "Debug",
                "DeleteAgent",
                "Diff",
                "GetArtifact",
                "HandleHook",
                "ListAgents",
                "ListHosts",
                "PutArtifact",
                "RenameAgent",
                "SendInput",
                "SendMessage",
                "SetAgentStatus",
                "SubscribeAgents",
                "SubscribeHosts",
                "SubscribeSession",
            ]))
        );

        let message_fields = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.message_type.iter())
            .map(|message| {
                (
                    message.name.as_deref().unwrap_or_default(),
                    message
                        .field
                        .iter()
                        .map(|field| {
                            (
                                field.name.as_deref().unwrap_or_default(),
                                field.number.unwrap_or_default(),
                            )
                        })
                        .collect::<std::collections::BTreeMap<_, _>>(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            message_fields.get("Message").cloned(),
            Some(std::collections::BTreeMap::from([
                ("hello", 1),
                ("hello_ack", 2),
                ("neighbor_up", 3),
                ("neighbor_down", 4),
                ("reauth", 8),
                ("link_close", 9),
            ]))
        );
        assert_eq!(
            message_fields.get("Hello").cloned(),
            Some(std::collections::BTreeMap::from([
                ("supported_protocol_versions", 1),
                ("host", 2),
                ("neighbors", 3),
                ("auth_token", 4),
            ]))
        );
        assert_eq!(
            message_fields.get("StreamPreface").cloned(),
            Some(std::collections::BTreeMap::from([("dst", 1)]))
        );

        let enum_values = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.enum_type.iter())
            .map(|enumeration| {
                (
                    enumeration.name.as_deref().unwrap_or_default(),
                    enumeration
                        .value
                        .iter()
                        .filter_map(|value| value.name.as_deref())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            enum_values.get("StreamRefusal").cloned(),
            Some(vec![
                "STREAM_REFUSAL_UNSPECIFIED",
                "NO_ROUTE",
                "PAYMENT_REQUIRED",
                "RATE_LIMITED",
                "NOT_ADJACENT",
                "SHUTTING_DOWN",
            ])
        );

        let pairing_methods = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.service.iter())
            .find(|service| service.name.as_deref() == Some("PairingService"))
            .expect("PairingService should exist");
        let pair = pairing_methods
            .method
            .iter()
            .find(|method| method.name.as_deref() == Some("Pair"))
            .expect("Pair should exist");
        assert_eq!(pair.input_type.as_deref(), Some(".amux.v1.PairMessage"));
        assert_eq!(pair.output_type.as_deref(), Some(".amux.v1.PairMessage"));
        assert_eq!(pair.client_streaming, Some(true));
        assert_eq!(pair.server_streaming, Some(true));
    }

    #[test]
    fn generated_service_clients_are_available() {
        let clients = [
            std::any::type_name::<super::agent_service_client::AgentServiceClient<()>>(),
            std::any::type_name::<super::client_service_client::ClientServiceClient<()>>(),
            std::any::type_name::<super::pairing_service_client::PairingServiceClient<()>>(),
        ];

        assert!(clients.iter().all(|client| client.contains("Client")));
    }
}

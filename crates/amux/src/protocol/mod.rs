mod error;

/// Protocol version for the generated `LinkService.Connect` handshake.
pub const PROTOCOL_VERSION: u32 = 1;

pub use error::ProtocolError;
pub(crate) use error::{protocol_error_from_status_details, protocol_status};

pub(crate) mod amux {
    pub(crate) mod v1 {
        // The generated `Agent` oneof gained a third variant (`Codex`), which
        // trips `enum_variant_names` on prost's `TestAgent` variant name.
        #![allow(dead_code, clippy::enum_variant_names)]
        include!(concat!(env!("OUT_DIR"), "/amux.v1.rs"));
    }
}

/// Protobuf wire boundary.
///
/// Generated `prost` types stay behind this facade. Domain modules own their
/// conversions and import generated structs only at service/routing edges.
pub(crate) mod wire {
    pub(crate) use super::amux::v1::*;
    pub(crate) use super::error::{
        DecodeError, EncodeError, decode_protocol_error, encode_protocol_error,
    };

    pub(crate) mod pb {
        pub(crate) use super::super::amux::v1::*;
    }
}

#[cfg(test)]
pub(crate) const DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/amux.v1.bin"));

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
            "TunnelOpen",
            "TunnelData",
            "TunnelClose",
            "PairQrCloudPeerRequest",
            "PairQrCloudPeerResponse",
            "PairMessage",
            "PairingComplete",
            "PairingError",
            "PairingIdentity",
            "AgentUpdated",
            "CodexCreateConfig",
            "CodexSdkV1Args",
            "CodexSdkV1Input",
            "CodexSdkV1Output",
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
            "LinkService",
            "PairingService",
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
            service_methods.get("LinkService").cloned(),
            Some(std::collections::BTreeSet::from(["Connect"]))
        );
        assert_eq!(
            service_methods.get("PairingService").cloned(),
            Some(std::collections::BTreeSet::from(["Pair"]))
        );
        assert_eq!(
            service_methods.get("AgentService").cloned(),
            Some(std::collections::BTreeSet::from([
                "CreateAgent",
                "DeleteAgent",
                "RenameAgent",
                "SendInput",
                "SubscribeAgentEvents",
                "SubscribeSession",
            ]))
        );
        assert_eq!(
            service_methods.get("ClientService").cloned(),
            Some(std::collections::BTreeSet::from([
                "CancelPairing",
                "CreateAgent",
                "Debug",
                "DeleteAgent",
                "GetPairingStatus",
                "GetPeer",
                "HandleHook",
                "ListAgents",
                "ListHosts",
                "ListPeers",
                "PairPeer",
                "PairPinCloudPeer",
                "PairQrCloudPeer",
                "RenameAgent",
                "Resume",
                "SendInput",
                "Shutdown",
                "StartPairing",
                "SubscribeAgents",
                "SubscribeHosts",
                "SubscribeSession",
                "Suspend",
                "Unpair",
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
            message_fields.get("TunnelOpen").cloned(),
            Some(std::collections::BTreeMap::from([
                ("tunnel_id", 1),
                ("src", 2),
                ("dst", 3)
            ]))
        );
        assert_eq!(
            message_fields.get("TunnelData").cloned(),
            Some(std::collections::BTreeMap::from([
                ("tunnel_id", 1),
                ("dst", 2),
                ("payload", 3)
            ]))
        );
        assert_eq!(
            message_fields.get("TunnelClose").cloned(),
            Some(std::collections::BTreeMap::from([
                ("tunnel_id", 1),
                ("dst", 2)
            ]))
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
            std::any::type_name::<super::wire::link_service_client::LinkServiceClient<()>>(),
            std::any::type_name::<super::wire::agent_service_client::AgentServiceClient<()>>(),
            std::any::type_name::<super::wire::client_service_client::ClientServiceClient<()>>(),
            std::any::type_name::<super::wire::pairing_service_client::PairingServiceClient<()>>(),
        ];

        assert!(clients.iter().all(|client| client.contains("Client")));
    }
}

mod error;

/// Protocol version for the generated `RoutingService.Connect` handshake.
pub const PROTOCOL_VERSION: u32 = 5;

pub use error::ProtocolError;
pub(crate) use error::protocol_status;

pub(crate) mod amux {
    pub(crate) mod v1 {
        #![allow(dead_code)]
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
            "RoutingEvent",
            "TunnelFrame",
            "PairByTokenRequest",
            "PairByTokenResponse",
            "PairQrCloudPeerRequest",
            "PairQrCloudPeerResponse",
            "PairBySpake2Message",
            "PairingComplete",
            "PairingError",
            "PairingIdentity",
            "AgentUpdated",
            "SessionClosed",
            "Reauth",
            "ReauthAck",
            "GoAway",
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
            "RoutingService",
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
            service_methods.get("RoutingService").cloned(),
            Some(std::collections::BTreeSet::from(["Connect"]))
        );
        assert_eq!(
            service_methods.get("PairingService").cloned(),
            Some(std::collections::BTreeSet::from([
                "PairBySpake2",
                "PairByToken"
            ]))
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
            message_fields.get("TunnelId").cloned(),
            Some(std::collections::BTreeMap::from([
                ("initiator", 1),
                ("nonce", 2)
            ]))
        );

        let pairing_methods = descriptor
            .file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.service.iter())
            .find(|service| service.name.as_deref() == Some("PairingService"))
            .expect("PairingService should exist");
        let pair_by_spake2 = pairing_methods
            .method
            .iter()
            .find(|method| method.name.as_deref() == Some("PairBySpake2"))
            .expect("PairBySpake2 should exist");
        assert_eq!(
            pair_by_spake2.input_type.as_deref(),
            Some(".amux.v1.PairBySpake2Message")
        );
        assert_eq!(
            pair_by_spake2.output_type.as_deref(),
            Some(".amux.v1.PairBySpake2Message")
        );
        assert_eq!(pair_by_spake2.client_streaming, Some(true));
        assert_eq!(pair_by_spake2.server_streaming, Some(true));
    }

    #[test]
    fn generated_service_clients_are_available() {
        let clients = [
            std::any::type_name::<super::wire::routing_service_client::RoutingServiceClient<()>>(),
            std::any::type_name::<super::wire::agent_service_client::AgentServiceClient<()>>(),
            std::any::type_name::<super::wire::client_service_client::ClientServiceClient<()>>(),
            std::any::type_name::<super::wire::pairing_service_client::PairingServiceClient<()>>(),
        ];

        assert!(clients.iter().all(|client| client.contains("Client")));
    }
}

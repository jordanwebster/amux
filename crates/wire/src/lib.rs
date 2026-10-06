//! Committed protobuf messages for every amux protocol boundary: the session
//! records the journal frames and the store keeps, the agent process's spec
//! and control frames, and the client, peer, pairing, profile and
//! installation services.

/// Protocol version for the native-stream link handshake. Bumped only for a
/// deliberate semantic break; the only-add rule keeps it otherwise unused.
pub const PROTOCOL_VERSION: u32 = 4;

pub mod amux {
    pub mod v1 {
        #![allow(dead_code, clippy::enum_variant_names, clippy::large_enum_variant)]
        include!("generated/amux.v1.rs");
    }
}

pub use amux::v1::*;

pub mod pb {
    pub use super::amux::v1::*;
}

/// Bound for link-control messages and application-stream prefaces.
pub const MESSAGE_SIZE_LIMIT: usize = 16 * 1024 * 1024;
/// RPC payloads include blobs plus protobuf framing overhead.
pub const CHANNEL_MESSAGE_SIZE_LIMIT: usize = 64 * 1024 * 1024;

pub fn client_service_client(
    channel: tonic::transport::Channel,
) -> client_service_client::ClientServiceClient<tonic::transport::Channel> {
    client_service_client::ClientServiceClient::new(channel)
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

pub fn peer_service_client(
    channel: tonic::transport::Channel,
) -> peer_service_client::PeerServiceClient<tonic::transport::Channel> {
    peer_service_client::PeerServiceClient::new(channel)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

pub fn peer_service_server<T>(service: T) -> peer_service_server::PeerServiceServer<T>
where
    T: peer_service_server::PeerService,
{
    peer_service_server::PeerServiceServer::new(service)
        .max_decoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(CHANNEL_MESSAGE_SIZE_LIMIT)
}

impl ToolClass {
    /// Whether a call of this class only looks, so views fold a run of
    /// them together.
    pub fn explores(self) -> bool {
        !matches!(self, ToolClass::Unspecified | ToolClass::Consequential)
    }
}

/// Whether a call's class on the wire only looks; an unknown value does not.
pub fn explores(class: i32) -> bool {
    ToolClass::try_from(class).is_ok_and(ToolClass::explores)
}

/// The kind tag an item or snapshot envelope carries for each interpreter.
pub const fn kind_tag(kind: Kind) -> &'static str {
    match kind {
        Kind::Unspecified => "",
        Kind::ClaudePty => "claude_pty",
        Kind::ClaudeSdk => "claude_sdk",
        Kind::Codex => "codex",
    }
}

/// The interpreter an envelope's kind tag names, if any.
pub fn kind_from_tag(tag: &str) -> Option<Kind> {
    match tag {
        "claude_pty" => Some(Kind::ClaudePty),
        "claude_sdk" => Some(Kind::ClaudeSdk),
        "codex" => Some(Kind::Codex),
        _ => None,
    }
}

pub const DESCRIPTOR_SET: &[u8] = include_bytes!("generated/amux.v1.bin");

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use prost::Message as _;
    use prost_types::{DescriptorProto, FileDescriptorSet};

    use super::*;

    fn descriptor() -> FileDescriptorSet {
        FileDescriptorSet::decode(DESCRIPTOR_SET).expect("descriptor set should decode")
    }

    fn messages(set: &FileDescriptorSet) -> BTreeMap<String, DescriptorProto> {
        set.file
            .iter()
            .filter(|file| file.package.as_deref() == Some("amux.v1"))
            .flat_map(|file| file.message_type.iter())
            .map(|message| (message.name().to_owned(), message.clone()))
            .collect()
    }

    fn fields(message: &DescriptorProto) -> BTreeMap<&str, i32> {
        message
            .field
            .iter()
            .map(|field| (field.name(), field.number()))
            .collect()
    }

    fn methods(set: &FileDescriptorSet, service: &str) -> BTreeSet<String> {
        set.file
            .iter()
            .flat_map(|file| file.service.iter())
            .filter(|candidate| candidate.name() == service)
            .flat_map(|candidate| candidate.method.iter())
            .map(|method| method.name().to_owned())
            .collect()
    }

    #[test]
    fn services_are_the_client_peer_pairing_profile_and_installation_surfaces() {
        let set = descriptor();
        let services = set
            .file
            .iter()
            .flat_map(|file| file.service.iter())
            .map(|service| service.name().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            services,
            BTreeSet::from(
                [
                    "ClientService",
                    "InstallationService",
                    "PairingService",
                    "PeerService",
                    "ProfileService"
                ]
                .map(str::to_owned)
            )
        );

        let client = methods(&set, "ClientService");
        assert_eq!(
            client,
            BTreeSet::from(
                [
                    "SubscribeInventory",
                    "ResolveAgent",
                    "Subscribe",
                    "Fetch",
                    "Get",
                    "SendInput",
                    "CreateAgent",
                    "RenameAgent",
                    "StopAgent",
                    "ResumeAgent",
                    "DeleteAgent",
                    "SendMessage",
                    "PutBlob",
                    "GetBlob",
                    "Diff",
                    "ListRepositories",
                    "Dump",
                ]
                .map(str::to_owned)
            )
        );

        // The peer surface is the client surface minus name resolution, over
        // the same request and response messages. A peer's Dump answers
        // for the agents the called host runs, so a dump taken elsewhere
        // holds their host-side parts.
        let mut peer_expected = client.clone();
        peer_expected.remove("ResolveAgent");
        assert_eq!(methods(&set, "PeerService"), peer_expected);

        let installation = methods(&set, "InstallationService");
        assert_eq!(
            installation,
            BTreeSet::from(["GetInfo", "Shutdown"].map(str::to_owned))
        );
    }

    #[test]
    fn envelope_numbers_match_the_record_schema() {
        let set = descriptor();
        let messages = messages(&set);
        let expect = |name: &str, pairs: &[(&str, i32)]| {
            assert_eq!(
                fields(&messages[name]),
                pairs.iter().copied().collect::<BTreeMap<_, _>>(),
                "{name}"
            );
        };
        expect(
            "Step",
            &[
                ("items", 1),
                ("appends", 2),
                ("snapshot", 3),
                ("turn_end", 5),
            ],
        );
        expect(
            "Item",
            &[
                ("agent", 1),
                ("key", 2),
                ("order", 3),
                ("revision", 4),
                ("producer_version", 5),
                ("input_id", 6),
                ("text", 7),
                ("attachments", 8),
                ("kind", 9),
                ("body", 10),
                ("at_ms", 11),
            ],
        );
        expect(
            "Append",
            &[
                ("agent", 1),
                ("key", 2),
                ("base_revision", 3),
                ("revision", 4),
                ("text", 5),
            ],
        );
        expect(
            "Snapshot",
            &[
                ("agent", 1),
                ("revision", 2),
                ("queue", 3),
                ("kind", 4),
                ("body", 5),
                ("phase", 6),
                ("working_on", 7),
                ("at_ms", 8),
                ("phase_since_ms", 10),
                ("git", 11),
            ],
        );
        expect(
            "CtlFrame",
            &[
                ("hello", 1),
                ("nudge", 2),
                ("input", 3),
                ("stop", 4),
                ("dump", 5),
                ("reply", 6),
            ],
        );
        expect(
            "SessionEvent",
            &[
                ("snapshot", 1),
                ("item", 2),
                ("append", 3),
                ("caught_up", 5),
                ("lagged", 6),
                ("reset", 7),
                ("detached", 8),
                ("opening", 9),
            ],
        );
    }

    #[test]
    fn a_spec_names_its_parent_by_host_and_id() {
        let set = descriptor();
        let messages = messages(&set);
        let parent = messages["AgentSpec"]
            .field
            .iter()
            .find(|field| field.name() == "parent")
            .expect("AgentSpec.parent");
        assert_eq!(parent.number(), 6);
        assert_eq!(parent.type_name(), ".amux.v1.AgentParent");
        assert_eq!(
            fields(&messages["AgentParent"]),
            BTreeMap::from([("host_id", 1), ("agent_id", 2)])
        );
    }

    #[test]
    fn every_snapshot_body_carries_the_four_strip_facts() {
        let set = descriptor();
        let messages = messages(&set);
        for snapshot in ["ClaudePtySnapshot", "ClaudeSdkSnapshot", "CodexSnapshot"] {
            let types = messages[snapshot]
                .field
                .iter()
                .map(|field| field.type_name())
                .collect::<BTreeSet<_>>();
            for fact in [
                ".amux.v1.UsageLimits",
                ".amux.v1.ToolServerHealth",
                ".amux.v1.SignIn",
                ".amux.v1.BackgroundProcesses",
            ] {
                assert!(types.contains(fact), "{snapshot} lacks {fact}");
            }
        }
    }

    #[test]
    fn a_step_round_trips_with_an_opaque_body() {
        let body = ClaudeSdkItem {
            kind: Some(claude_sdk_item::Kind::Message(Text { complete: true })),
        }
        .encode_to_vec();
        let step = Step {
            items: vec![Item {
                key: "msg:1".into(),
                text: "hello".into(),
                kind: kind_tag(Kind::ClaudeSdk).into(),
                body: body.clone(),
                at_ms: 7,
                ..Default::default()
            }],
            turn_end: Some(TurnEnd {
                turn_id: 1,
                last_message_key: "msg:1".into(),
            }),
            ..Default::default()
        };
        let decoded = Step::decode(step.encode_to_vec().as_slice()).expect("decode");
        assert_eq!(decoded, step);
        assert_eq!(kind_from_tag(&decoded.items[0].kind), Some(Kind::ClaudeSdk));
        assert_eq!(
            ClaudeSdkItem::decode(decoded.items[0].body.as_slice()).expect("body"),
            ClaudeSdkItem::decode(body.as_slice()).expect("body")
        );
    }
}

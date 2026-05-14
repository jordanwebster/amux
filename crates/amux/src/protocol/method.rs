#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MethodSpec {
    pub(crate) name: &'static str,
    pub(crate) access: MethodAccess,
    pub(crate) kind: MethodKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodAccess {
    Local,
    Peer,
    Routed,
}

impl MethodAccess {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MethodAccess::Local => "local",
            MethodAccess::Peer => "peer",
            MethodAccess::Routed => "routed",
        }
    }

    /// Scopes form a containment hierarchy: `Routed < Peer < Local`. A
    /// connection at scope `self` may call methods requiring scope `required`
    /// iff `self.rank() >= required.rank()`.
    pub(crate) const fn rank(self) -> u8 {
        match self {
            MethodAccess::Routed => 0,
            MethodAccess::Peer => 1,
            MethodAccess::Local => 2,
        }
    }

    /// Returns true if a connection at this scope may call a method declared
    /// at the `required` scope.
    pub(crate) const fn allows(self, required: MethodAccess) -> bool {
        self.rank() >= required.rank()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodKind {
    Unary,
    ServerStreaming,
}

pub(crate) const ROUTING_SUBSCRIBE_EVENTS_NAME: &str =
    "/amux.v1.RoutingService/SubscribeRoutingEvents";

pub(crate) const AGENT_SUBSCRIBE_EVENTS_NAME: &str = "/amux.v1.AgentService/SubscribeAgentEvents";
pub(crate) const AGENT_LIST_NAME: &str = "/amux.v1.AgentService/ListAgents";
pub(crate) const AGENT_RESOLVE_NAME: &str = "/amux.v1.AgentService/ResolveAgent";
pub(crate) const AGENT_CREATE_NAME: &str = "/amux.v1.AgentService/CreateAgent";
pub(crate) const AGENT_RENAME_NAME: &str = "/amux.v1.AgentService/RenameAgent";
pub(crate) const AGENT_DELETE_NAME: &str = "/amux.v1.AgentService/DeleteAgent";
pub(crate) const AGENT_SUBSCRIBE_SESSION_NAME: &str = "/amux.v1.AgentService/SubscribeSession";
pub(crate) const AGENT_SEND_INPUT_NAME: &str = "/amux.v1.AgentService/SendInput";

pub(crate) const HOOK_HANDLE_NAME: &str = "/amux.v1.HookService/HandleHook";

pub(crate) const ADMIN_DEBUG_NAME: &str = "/amux.v1.AdminService/Debug";
pub(crate) const ADMIN_SHUTDOWN_NAME: &str = "/amux.v1.AdminService/Shutdown";
pub(crate) const ADMIN_SUSPEND_NAME: &str = "/amux.v1.AdminService/Suspend";
pub(crate) const ADMIN_RESUME_NAME: &str = "/amux.v1.AdminService/Resume";
pub(crate) const ADMIN_CONNECT_TO_SERVER_NAME: &str = "/amux.v1.AdminService/ConnectToServer";

pub(crate) const ROUTING_SUBSCRIBE_EVENTS: MethodSpec = MethodSpec {
    name: ROUTING_SUBSCRIBE_EVENTS_NAME,
    access: MethodAccess::Peer,
    kind: MethodKind::ServerStreaming,
};

pub(crate) const AGENT_LIST: MethodSpec = MethodSpec {
    name: AGENT_LIST_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_SUBSCRIBE_EVENTS: MethodSpec = MethodSpec {
    name: AGENT_SUBSCRIBE_EVENTS_NAME,
    access: MethodAccess::Routed,
    kind: MethodKind::ServerStreaming,
};

pub(crate) const AGENT_RESOLVE: MethodSpec = MethodSpec {
    name: AGENT_RESOLVE_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_CREATE: MethodSpec = MethodSpec {
    name: AGENT_CREATE_NAME,
    access: MethodAccess::Routed,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_RENAME: MethodSpec = MethodSpec {
    name: AGENT_RENAME_NAME,
    access: MethodAccess::Routed,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_DELETE: MethodSpec = MethodSpec {
    name: AGENT_DELETE_NAME,
    access: MethodAccess::Routed,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_SUBSCRIBE_SESSION: MethodSpec = MethodSpec {
    name: AGENT_SUBSCRIBE_SESSION_NAME,
    access: MethodAccess::Routed,
    kind: MethodKind::ServerStreaming,
};

pub(crate) const AGENT_SEND_INPUT: MethodSpec = MethodSpec {
    name: AGENT_SEND_INPUT_NAME,
    access: MethodAccess::Routed,
    kind: MethodKind::Unary,
};

pub(crate) const HOOK_HANDLE: MethodSpec = MethodSpec {
    name: HOOK_HANDLE_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_DEBUG: MethodSpec = MethodSpec {
    name: ADMIN_DEBUG_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_SHUTDOWN: MethodSpec = MethodSpec {
    name: ADMIN_SHUTDOWN_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_SUSPEND: MethodSpec = MethodSpec {
    name: ADMIN_SUSPEND_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_RESUME: MethodSpec = MethodSpec {
    name: ADMIN_RESUME_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_CONNECT_TO_SERVER: MethodSpec = MethodSpec {
    name: ADMIN_CONNECT_TO_SERVER_NAME,
    access: MethodAccess::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ALL: &[MethodSpec] = &[
    ROUTING_SUBSCRIBE_EVENTS,
    AGENT_SUBSCRIBE_EVENTS,
    AGENT_LIST,
    AGENT_RESOLVE,
    AGENT_CREATE,
    AGENT_RENAME,
    AGENT_DELETE,
    AGENT_SUBSCRIBE_SESSION,
    AGENT_SEND_INPUT,
    HOOK_HANDLE,
    ADMIN_DEBUG,
    ADMIN_SHUTDOWN,
    ADMIN_SUSPEND,
    ADMIN_RESUME,
    ADMIN_CONNECT_TO_SERVER,
];

pub(crate) fn find(name: &str) -> Option<MethodSpec> {
    ALL.iter().copied().find(|spec| spec.name == name)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodLookupError {
    Unknown,
    InsufficientScope {
        spec: MethodSpec,
        connection_scope: MethodAccess,
    },
}

/// Look up a method by name and verify that a connection at
/// `connection_scope` may call it. Scopes are a containment hierarchy
/// (`Local > Peer > Routed`); higher scopes can call any method at a lower
/// or equal scope.
pub(crate) fn find_for_connection_scope(
    name: &str,
    connection_scope: MethodAccess,
) -> Result<MethodSpec, MethodLookupError> {
    match find(name) {
        Some(spec) if connection_scope.allows(spec.access) => Ok(spec),
        Some(spec) => Err(MethodLookupError::InsufficientScope {
            spec,
            connection_scope,
        }),
        None => Err(MethodLookupError::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use prost::Message as _;

    use super::*;

    #[test]
    fn method_specs_match_proto_descriptor() {
        let descriptor =
            prost_types::FileDescriptorSet::decode(crate::protocol::wire::DESCRIPTOR_SET)
                .expect("descriptor set should decode");
        let file = descriptor
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("amux.v1"))
            .expect("amux.v1 descriptor should be present");
        let package = file.package.as_deref().expect("package should be present");

        let descriptor_methods = file
            .service
            .iter()
            .flat_map(|service| {
                let service_name = service.name.as_deref().expect("service should be named");
                service.method.iter().map(move |method| {
                    let method_name = method.name.as_deref().expect("method should be named");
                    let name = format!("/{package}.{service_name}/{method_name}");
                    let kind = match (
                        method.client_streaming.unwrap_or(false),
                        method.server_streaming.unwrap_or(false),
                    ) {
                        (false, false) => MethodKind::Unary,
                        (false, true) => MethodKind::ServerStreaming,
                        (true, true) => {
                            panic!("{name} is bidi-streaming; bidi RPCs are not supported")
                        }
                        (true, false) => {
                            panic!("{name} is client-streaming only; add a MethodKind if needed")
                        }
                    };
                    (name, kind)
                })
            })
            .collect::<BTreeMap<_, _>>();

        let mut spec_names = BTreeSet::new();
        for spec in ALL {
            assert!(
                spec_names.insert(spec.name),
                "duplicate MethodSpec {}",
                spec.name
            );
            let descriptor_kind = descriptor_methods
                .get(spec.name)
                .unwrap_or_else(|| panic!("{} is missing from protobuf descriptor", spec.name));
            assert_eq!(
                *descriptor_kind, spec.kind,
                "{} MethodKind should match descriptor streaming flags",
                spec.name
            );
        }

        let descriptor_names = descriptor_methods
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            spec_names, descriptor_names,
            "every proto RPC should have exactly one MethodSpec"
        );

        assert_eq!(find(AGENT_CREATE_NAME), Some(AGENT_CREATE));
        assert_eq!(find("/amux.v1.Missing/Nope"), None);
    }

    #[test]
    fn scope_hierarchy_allows_higher_to_call_lower() {
        use MethodAccess::*;
        // Each scope allows itself and anything below it.
        assert!(Local.allows(Local));
        assert!(Local.allows(Peer));
        assert!(Local.allows(Routed));
        assert!(Peer.allows(Peer));
        assert!(Peer.allows(Routed));
        assert!(Routed.allows(Routed));
        // No scope allows anything above it.
        assert!(!Peer.allows(Local));
        assert!(!Routed.allows(Local));
        assert!(!Routed.allows(Peer));
    }

    #[test]
    fn method_lookup_distinguishes_unknown_from_insufficient_scope() {
        // Local connections can call methods at any scope.
        assert_eq!(
            find_for_connection_scope(AGENT_LIST_NAME, MethodAccess::Local),
            Ok(AGENT_LIST)
        );
        assert_eq!(
            find_for_connection_scope(ROUTING_SUBSCRIBE_EVENTS_NAME, MethodAccess::Local),
            Ok(ROUTING_SUBSCRIBE_EVENTS)
        );
        assert_eq!(
            find_for_connection_scope(AGENT_SUBSCRIBE_EVENTS_NAME, MethodAccess::Local),
            Ok(AGENT_SUBSCRIBE_EVENTS)
        );

        // Peer connections cannot reach Local methods.
        assert_eq!(
            find_for_connection_scope(AGENT_LIST_NAME, MethodAccess::Peer),
            Err(MethodLookupError::InsufficientScope {
                spec: AGENT_LIST,
                connection_scope: MethodAccess::Peer,
            })
        );

        // Routed scope can only reach Routed methods.
        assert_eq!(
            find_for_connection_scope(ROUTING_SUBSCRIBE_EVENTS_NAME, MethodAccess::Routed),
            Err(MethodLookupError::InsufficientScope {
                spec: ROUTING_SUBSCRIBE_EVENTS,
                connection_scope: MethodAccess::Routed,
            })
        );
        assert_eq!(
            find_for_connection_scope(AGENT_SUBSCRIBE_EVENTS_NAME, MethodAccess::Routed),
            Ok(AGENT_SUBSCRIBE_EVENTS)
        );

        assert_eq!(
            find_for_connection_scope("/amux.v1.Missing/Nope", MethodAccess::Routed),
            Err(MethodLookupError::Unknown)
        );
    }

    #[test]
    fn method_accesses_match_protocol_contract() {
        let expected_accesses = [
            (ROUTING_SUBSCRIBE_EVENTS_NAME, MethodAccess::Peer),
            (AGENT_SUBSCRIBE_EVENTS_NAME, MethodAccess::Routed),
            (AGENT_LIST_NAME, MethodAccess::Local),
            (AGENT_RESOLVE_NAME, MethodAccess::Local),
            (AGENT_CREATE_NAME, MethodAccess::Routed),
            (AGENT_RENAME_NAME, MethodAccess::Routed),
            (AGENT_DELETE_NAME, MethodAccess::Routed),
            (AGENT_SUBSCRIBE_SESSION_NAME, MethodAccess::Routed),
            (AGENT_SEND_INPUT_NAME, MethodAccess::Routed),
            (HOOK_HANDLE_NAME, MethodAccess::Local),
            (ADMIN_DEBUG_NAME, MethodAccess::Local),
            (ADMIN_SHUTDOWN_NAME, MethodAccess::Local),
            (ADMIN_SUSPEND_NAME, MethodAccess::Local),
            (ADMIN_RESUME_NAME, MethodAccess::Local),
            (ADMIN_CONNECT_TO_SERVER_NAME, MethodAccess::Local),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();

        let actual_accesses = ALL
            .iter()
            .map(|spec| (spec.name, spec.access))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(actual_accesses, expected_accesses);
    }
}

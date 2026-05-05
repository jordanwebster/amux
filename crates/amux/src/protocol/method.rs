#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MethodSpec {
    pub(crate) name: &'static str,
    pub(crate) scope: MethodScope,
    pub(crate) kind: MethodKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodScope {
    Local,
    Peer,
    Routed,
}

impl MethodScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MethodScope::Local => "local",
            MethodScope::Peer => "peer",
            MethodScope::Routed => "routed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodKind {
    Unary,
    ServerStreaming,
    BidiStreaming,
}

pub(crate) const ROUTING_SUBSCRIBE_EVENTS_NAME: &str =
    "/amux.v1.RoutingService/SubscribeRoutingEvents";

pub(crate) const AGENT_LIST_NAME: &str = "/amux.v1.AgentService/ListAgents";
pub(crate) const AGENT_RESOLVE_NAME: &str = "/amux.v1.AgentService/ResolveAgent";
pub(crate) const AGENT_CREATE_NAME: &str = "/amux.v1.AgentService/CreateAgent";
pub(crate) const AGENT_RENAME_NAME: &str = "/amux.v1.AgentService/RenameAgent";
pub(crate) const AGENT_DELETE_NAME: &str = "/amux.v1.AgentService/DeleteAgent";
pub(crate) const AGENT_OPEN_SESSION_NAME: &str = "/amux.v1.AgentService/OpenSession";

pub(crate) const HOOK_HANDLE_NAME: &str = "/amux.v1.HookService/HandleHook";

pub(crate) const ADMIN_DEBUG_NAME: &str = "/amux.v1.AdminService/Debug";
pub(crate) const ADMIN_SHUTDOWN_NAME: &str = "/amux.v1.AdminService/Shutdown";
pub(crate) const ADMIN_SUSPEND_NAME: &str = "/amux.v1.AdminService/Suspend";
pub(crate) const ADMIN_RESUME_NAME: &str = "/amux.v1.AdminService/Resume";
pub(crate) const ADMIN_CONNECT_TO_SERVER_NAME: &str = "/amux.v1.AdminService/ConnectToServer";

pub(crate) const ROUTING_SUBSCRIBE_EVENTS: MethodSpec = MethodSpec {
    name: ROUTING_SUBSCRIBE_EVENTS_NAME,
    scope: MethodScope::Peer,
    kind: MethodKind::ServerStreaming,
};

pub(crate) const AGENT_LIST: MethodSpec = MethodSpec {
    name: AGENT_LIST_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_RESOLVE: MethodSpec = MethodSpec {
    name: AGENT_RESOLVE_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_CREATE: MethodSpec = MethodSpec {
    name: AGENT_CREATE_NAME,
    scope: MethodScope::Routed,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_RENAME: MethodSpec = MethodSpec {
    name: AGENT_RENAME_NAME,
    scope: MethodScope::Routed,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_DELETE: MethodSpec = MethodSpec {
    name: AGENT_DELETE_NAME,
    scope: MethodScope::Routed,
    kind: MethodKind::Unary,
};

pub(crate) const AGENT_OPEN_SESSION: MethodSpec = MethodSpec {
    name: AGENT_OPEN_SESSION_NAME,
    scope: MethodScope::Routed,
    kind: MethodKind::BidiStreaming,
};

pub(crate) const HOOK_HANDLE: MethodSpec = MethodSpec {
    name: HOOK_HANDLE_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_DEBUG: MethodSpec = MethodSpec {
    name: ADMIN_DEBUG_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_SHUTDOWN: MethodSpec = MethodSpec {
    name: ADMIN_SHUTDOWN_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_SUSPEND: MethodSpec = MethodSpec {
    name: ADMIN_SUSPEND_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_RESUME: MethodSpec = MethodSpec {
    name: ADMIN_RESUME_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ADMIN_CONNECT_TO_SERVER: MethodSpec = MethodSpec {
    name: ADMIN_CONNECT_TO_SERVER_NAME,
    scope: MethodScope::Local,
    kind: MethodKind::Unary,
};

pub(crate) const ALL: &[MethodSpec] = &[
    ROUTING_SUBSCRIBE_EVENTS,
    AGENT_LIST,
    AGENT_RESOLVE,
    AGENT_CREATE,
    AGENT_RENAME,
    AGENT_DELETE,
    AGENT_OPEN_SESSION,
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
    WrongScope {
        spec: MethodSpec,
        requested_scope: MethodScope,
    },
}

pub(crate) fn find_for_scope(
    name: &str,
    requested_scope: MethodScope,
) -> Result<MethodSpec, MethodLookupError> {
    match find(name) {
        Some(spec) if spec.scope == requested_scope => Ok(spec),
        Some(spec) => Err(MethodLookupError::WrongScope {
            spec,
            requested_scope,
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
                        (true, true) => MethodKind::BidiStreaming,
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
    fn method_lookup_distinguishes_unknown_from_wrong_scope() {
        assert_eq!(
            find_for_scope(AGENT_LIST_NAME, MethodScope::Local),
            Ok(AGENT_LIST)
        );
        assert_eq!(
            find_for_scope(AGENT_LIST_NAME, MethodScope::Routed),
            Err(MethodLookupError::WrongScope {
                spec: AGENT_LIST,
                requested_scope: MethodScope::Routed,
            })
        );
        assert_eq!(
            find_for_scope("/amux.v1.Missing/Nope", MethodScope::Routed),
            Err(MethodLookupError::Unknown)
        );
    }

    #[test]
    fn method_scopes_match_protocol_contract() {
        let expected_scopes = [
            (ROUTING_SUBSCRIBE_EVENTS_NAME, MethodScope::Peer),
            (AGENT_LIST_NAME, MethodScope::Local),
            (AGENT_RESOLVE_NAME, MethodScope::Local),
            (AGENT_CREATE_NAME, MethodScope::Routed),
            (AGENT_RENAME_NAME, MethodScope::Routed),
            (AGENT_DELETE_NAME, MethodScope::Routed),
            (AGENT_OPEN_SESSION_NAME, MethodScope::Routed),
            (HOOK_HANDLE_NAME, MethodScope::Local),
            (ADMIN_DEBUG_NAME, MethodScope::Local),
            (ADMIN_SHUTDOWN_NAME, MethodScope::Local),
            (ADMIN_SUSPEND_NAME, MethodScope::Local),
            (ADMIN_RESUME_NAME, MethodScope::Local),
            (ADMIN_CONNECT_TO_SERVER_NAME, MethodScope::Local),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();

        let actual_scopes = ALL
            .iter()
            .map(|spec| (spec.name, spec.scope))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(actual_scopes, expected_scopes);
    }
}

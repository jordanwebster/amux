use uuid::Uuid;

use crate::protocol::message::{AGENT_TYPE_CLAUDE, Capabilities, Host, SupportedAgentType};

pub(crate) const MAX_SUPPORTED_AGENT_TYPES: usize = 64;

pub(crate) fn local_host(host_id: Uuid, host_name: &str, is_cloud_server: bool) -> Host {
    Host {
        id: host_id,
        name: host_name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: local_capabilities(is_cloud_server),
    }
}

pub(in crate::server) fn validate_remote_host(host: &Host) -> std::result::Result<(), String> {
    if host.id == Uuid::nil() {
        return Err("host_id must be non-zero".to_string());
    }
    if host.name.is_empty() {
        return Err("host name must be non-empty".to_string());
    }
    if host.version.is_empty() {
        return Err("host version must be non-empty".to_string());
    }
    if host.capabilities.supported_agent_types.len() > MAX_SUPPORTED_AGENT_TYPES {
        return Err(format!(
            "supported agent_type count must be at most {MAX_SUPPORTED_AGENT_TYPES}"
        ));
    }
    if host
        .capabilities
        .supported_agent_types
        .iter()
        .any(|agent| agent.agent_type.is_empty())
    {
        return Err("supported agent_type must be non-empty".to_string());
    }
    Ok(())
}

pub(crate) fn local_capabilities(is_cloud_server: bool) -> Capabilities {
    if is_cloud_server {
        return Capabilities {
            features: Vec::new(),
            supported_agent_types: Vec::new(),
        };
    }

    #[cfg(any(debug_assertions, test))]
    let supported_agent_types = vec![
        SupportedAgentType {
            agent_type: AGENT_TYPE_CLAUDE.to_string(),
        },
        SupportedAgentType {
            agent_type: crate::protocol::message::AGENT_TYPE_TEST_AGENT.to_string(),
        },
    ];

    #[cfg(not(any(debug_assertions, test)))]
    let supported_agent_types = vec![SupportedAgentType {
        agent_type: AGENT_TYPE_CLAUDE.to_string(),
    }];

    Capabilities {
        features: Vec::new(),
        supported_agent_types,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_remote_host_rejects_too_many_supported_agent_types() {
        let mut host = local_host(Uuid::from_u128(1), "peer", false);
        host.capabilities.supported_agent_types = (0..=MAX_SUPPORTED_AGENT_TYPES)
            .map(|idx| SupportedAgentType {
                agent_type: format!("agent-{idx}"),
            })
            .collect();

        let error = validate_remote_host(&host).expect_err("host should exceed the cap");

        assert!(error.contains("supported agent_type count"));
    }

    #[test]
    fn cloud_local_host_advertises_no_supported_agent_types() {
        let host = local_host(Uuid::from_u128(1), "cloud", true);

        assert!(host.capabilities.supported_agent_types.is_empty());
    }

    #[test]
    fn non_cloud_local_host_advertises_claude_agent_type() {
        let host = local_host(Uuid::from_u128(1), "host", false);

        assert!(
            host.capabilities
                .supported_agent_types
                .iter()
                .any(|agent| agent.agent_type == AGENT_TYPE_CLAUDE)
        );
    }
}

use uuid::Uuid;

use crate::routing::{Capabilities, Host};

pub(crate) const FEATURE_CLOUD_RELAY: &str = "amux.cloud_relay";
pub(crate) const MAX_SUPPORTED_AGENT_TYPES: usize = 64;
pub(crate) const MAX_HOST_NAME_BYTES: usize = 256;

pub(crate) fn local_host(host_id: Uuid, host_name: &str, capabilities: Capabilities) -> Host {
    Host {
        id: host_id,
        name: host_name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities,
    }
}

pub(crate) fn validate_remote_host(host: &Host) -> std::result::Result<(), String> {
    if host.id == Uuid::nil() {
        return Err("host_id must be non-zero".to_string());
    }
    if host.name.is_empty() {
        return Err("host name must be non-empty".to_string());
    }
    if host.name.len() > MAX_HOST_NAME_BYTES {
        return Err(format!(
            "host name must be at most {MAX_HOST_NAME_BYTES} bytes"
        ));
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
    fn validate_remote_host_rejects_oversized_name() {
        let host = local_host(
            Uuid::from_u128(1),
            &"a".repeat(MAX_HOST_NAME_BYTES + 1),
            false,
        );

        let error = validate_remote_host(&host).expect_err("host name should exceed the cap");

        assert!(error.contains("host name"));
    }

    #[test]
    fn cloud_local_host_advertises_no_supported_agent_types() {
        let host = local_host(Uuid::from_u128(1), "cloud", true);

        assert!(host.capabilities.supported_agent_types.is_empty());
        assert!(
            host.capabilities
                .features
                .iter()
                .any(|feature| feature == FEATURE_CLOUD_RELAY)
        );
    }

    #[cfg(feature = "local-agents")]
    #[test]
    fn non_cloud_local_host_advertises_claude_agent_type() {
        let host = local_host(Uuid::from_u128(1), "host", false);

        assert!(
            host.capabilities
                .supported_agent_types
                .iter()
                .any(|agent| agent.agent_type == AGENT_TYPE_CLAUDE)
        );
        #[cfg(unix)]
        assert!(
            host.capabilities
                .supported_agent_types
                .iter()
                .any(|agent| agent.agent_type == AGENT_TYPE_CODEX)
        );
    }

    #[cfg(all(feature = "local-agents", not(unix)))]
    #[test]
    fn non_unix_local_host_does_not_advertise_codex_agent_type() {
        let host = local_host(Uuid::from_u128(1), "host", false);

        assert!(
            host.capabilities
                .supported_agent_types
                .iter()
                .all(|agent| agent.agent_type != "codex")
        );
    }

    #[cfg(not(feature = "local-agents"))]
    #[test]
    fn non_cloud_local_host_advertises_no_agent_types_without_local_agents() {
        let host = local_host(Uuid::from_u128(1), "host", false);

        assert!(host.capabilities.supported_agent_types.is_empty());
    }
}

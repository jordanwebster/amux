use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use uuid::Uuid;

use crate::routing::{Capabilities, Host};

pub(crate) const FEATURE_CLOUD_RELAY: &str = "amux.cloud_relay";
pub(crate) const MAX_SUPPORTED_AGENT_TYPES: usize = 64;
pub(crate) const MAX_HOST_NAME_BYTES: usize = 256;

/// The stable local identity plus the profile binding fact read by each new
/// link handshake. Credential changes do not restart routing services, so the
/// binding bit must remain live while the rest of the host description stays
/// immutable.
#[derive(Clone)]
pub(crate) struct LiveLocalHost {
    host: Host,
    signed_in: Arc<AtomicBool>,
}

impl LiveLocalHost {
    pub(crate) fn new(host: Host) -> Self {
        Self {
            signed_in: Arc::new(AtomicBool::new(host.signed_in.unwrap_or(false))),
            host,
        }
    }

    pub(crate) fn id(&self) -> Uuid {
        self.host.id
    }

    pub(crate) fn snapshot(&self) -> Host {
        let mut host = self.host.clone();
        host.signed_in = Some(self.signed_in.load(Ordering::Acquire));
        host
    }

    pub(crate) fn set_signed_in(&self, signed_in: bool) {
        self.signed_in.store(signed_in, Ordering::Release);
    }
}

pub(crate) fn local_host(
    host_id: Uuid,
    host_name: &str,
    capabilities: Capabilities,
    signed_in: bool,
) -> Host {
    Host {
        id: host_id,
        name: host_name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities,
        signed_in: Some(signed_in),
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
    use crate::routing::SupportedAgentType;

    #[test]
    fn validate_remote_host_rejects_too_many_supported_agent_types() {
        let mut host = local_host(Uuid::from_u128(1), "peer", Capabilities::default(), false);
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
            Capabilities::default(),
            false,
        );

        let error = validate_remote_host(&host).expect_err("host name should exceed the cap");

        assert!(error.contains("host name"));
    }

    #[test]
    fn cloud_local_host_advertises_no_supported_agent_types() {
        let host = local_host(
            Uuid::from_u128(1),
            "cloud",
            Capabilities {
                features: vec![FEATURE_CLOUD_RELAY.to_string()],
                ..Default::default()
            },
            false,
        );

        assert!(host.capabilities.supported_agent_types.is_empty());
        assert!(
            host.capabilities
                .features
                .iter()
                .any(|feature| feature == FEATURE_CLOUD_RELAY)
        );
    }

    #[test]
    fn supplied_capabilities_are_advertised_verbatim() {
        let host = local_host(Uuid::from_u128(1), "host", Capabilities::default(), false);

        assert!(host.capabilities.supported_agent_types.is_empty());
    }
}

use std::sync::Arc;

use uuid::Uuid;

#[cfg(feature = "local-agents")]
use crate::agents::McpLaunchRoute;
use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::services::LocalAgentHost;
#[cfg(feature = "local-agents")]
use crate::services::PtyAgentHost;
use crate::subscription::SubscriptionReporter;
use crate::update::UpdateReporter;

pub(crate) struct ServerState {
    pub(crate) config: Config,
    pub(crate) host_id: Uuid,
    pub(crate) credentials: Option<Arc<dyn CredentialProvider>>,
    pub(crate) subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
    pub(crate) update_reporter: Option<Arc<dyn UpdateReporter>>,
    pub(crate) is_cloud_server: bool,
    pub(crate) jwt_validator: Option<Arc<JwtValidator>>,
    pub(crate) local_agent_host: Option<Arc<dyn LocalAgentHost>>,
}

#[cfg(feature = "local-agents")]
pub(crate) type LocalAgentHostHandle = Arc<PtyAgentHost>;
#[cfg(not(feature = "local-agents"))]
pub(crate) type LocalAgentHostHandle = Arc<dyn LocalAgentHost>;

/// Build the local agent host for this build: `Some` whenever `local-agents`
/// is compiled in (cloud-vs-device is decided by runtime guards, not host
/// presence), `None` for the embedded client. Spawns the host's session-event
/// loop, so it must be called from within a tokio runtime.
pub(crate) fn new_local_agent_host(
    host_id: Uuid,
    config: &Config,
    keymap_dir: std::path::PathBuf,
    data_dir: std::path::PathBuf,
) -> std::io::Result<Option<LocalAgentHostHandle>> {
    #[cfg(feature = "local-agents")]
    {
        let route = McpLaunchRoute::for_current_process(config, host_id)?;
        PtyAgentHost::new_with_mcp_launch_route(
            route,
            keymap_dir,
            data_dir,
            config.repository_roots.clone(),
        )
        .map(Some)
    }
    #[cfg(not(feature = "local-agents"))]
    {
        let _ = (host_id, config, keymap_dir, data_dir);
        Ok(None)
    }
}

impl ServerState {
    pub(crate) fn new(
        config: Config,
        host_id: Uuid,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
    ) -> Self {
        Self {
            config,
            host_id,
            credentials,
            subscription_reporter: None,
            update_reporter,
            is_cloud_server: false,
            jwt_validator: None,
            // Device startup injects the host after entering an async runtime;
            // cloud relays and service-level tests intentionally keep `None`.
            local_agent_host: None,
        }
    }

    pub(crate) fn host_id(&self) -> Uuid {
        self.host_id
    }

    pub(crate) fn host_name(&self) -> &str {
        &self.config.host_name
    }

    pub(crate) fn tcp_port(&self) -> Option<u16> {
        self.config.tcp_port
    }

    pub(crate) fn jwt_validator(&self) -> Option<Arc<JwtValidator>> {
        self.jwt_validator.clone()
    }

    pub(crate) fn minimum_client_version(&self, client_id: &str) -> Option<String> {
        self.config.minimum_client_versions.get(client_id).cloned()
    }

    pub(crate) fn is_cloud_server(&self) -> bool {
        self.is_cloud_server
    }

    pub(crate) fn local_agent_host(&self) -> Option<Arc<dyn LocalAgentHost>> {
        self.local_agent_host.clone()
    }
}

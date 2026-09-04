use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[cfg(feature = "local-agents")]
use crate::agents::McpLaunchRoute;
use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::protocol::ProtocolError;
use crate::server::ShutdownReason;
use crate::services::LocalAgentHost;
#[cfg(feature = "local-agents")]
use crate::services::PtyAgentHost;
use crate::subscription::SubscriptionReporter;
use crate::update::UpdateReporter;

/// Request from a service handler to shut down or suspend the server.
pub(crate) enum ShutdownRequest {
    Shutdown {
        reply: oneshot::Sender<Result<(), ProtocolError>>,
    },
    Suspend {
        reason: ShutdownReason,
        reply: oneshot::Sender<Result<u64, ProtocolError>>,
    },
}

pub(crate) struct ServerState {
    pub(crate) config: Config,
    pub(crate) host_id: Uuid,
    pub(crate) credentials: Option<Arc<dyn CredentialProvider>>,
    pub(crate) subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
    pub(crate) update_reporter: Option<Arc<dyn UpdateReporter>>,
    pub(crate) is_cloud_server: bool,
    pub(crate) jwt_validator: Option<Arc<JwtValidator>>,
    pub(crate) local_agent_host: Option<Arc<dyn LocalAgentHost>>,
    pub(crate) shutdown_tx: mpsc::Sender<ShutdownRequest>,
}

/// Build the local agent host for this build: `Some` whenever `local-agents`
/// is compiled in (cloud-vs-device is decided by runtime guards, not host
/// presence), `None` for the embedded client. Spawns the host's session-event
/// loop, so it must be called from within a tokio runtime.
pub(crate) fn new_local_agent_host(
    host_id: Uuid,
    config: &Config,
    keymap_dir: std::path::PathBuf,
    data_dir: std::path::PathBuf,
) -> std::io::Result<Option<Arc<dyn LocalAgentHost>>> {
    #[cfg(feature = "local-agents")]
    {
        let route = McpLaunchRoute::for_current_process(config, host_id)?;
        PtyAgentHost::new_with_mcp_launch_route(route, keymap_dir, data_dir)
            .map(|host| Some(host as Arc<dyn LocalAgentHost>))
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
        shutdown_tx: mpsc::Sender<ShutdownRequest>,
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
            shutdown_tx,
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

    pub(crate) fn state_path(&self) -> std::path::PathBuf {
        self.config.state_path.clone()
    }

    pub(crate) fn shutdown_tx(&self) -> mpsc::Sender<ShutdownRequest> {
        self.shutdown_tx.clone()
    }
    pub(crate) fn local_agent_host(&self) -> Option<Arc<dyn LocalAgentHost>> {
        self.local_agent_host.clone()
    }
}

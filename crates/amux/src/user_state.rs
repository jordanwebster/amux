use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, oneshot};
use uuid::Uuid;

use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::protocol::ProtocolError;
use crate::server::ShutdownReason;
use crate::services::{AgentServiceState, SharedAgentServiceState};
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
    pub(crate) update_reporter: Option<Arc<dyn UpdateReporter>>,
    pub(crate) is_cloud_server: bool,
    pub(crate) jwt_validator: Option<Arc<JwtValidator>>,
    pub(crate) local_agent_service: SharedAgentServiceState,
    pub(crate) shutdown_tx: mpsc::Sender<ShutdownRequest>,
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
            update_reporter,
            is_cloud_server: false,
            jwt_validator: None,
            local_agent_service: Arc::new(RwLock::new(AgentServiceState::new())),
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
    pub(crate) fn local_agent_service(&self) -> SharedAgentServiceState {
        self.local_agent_service.clone()
    }
}

pub(crate) async fn get_local_agent_service_state(
    state: &Arc<RwLock<ServerState>>,
) -> SharedAgentServiceState {
    state.read().await.local_agent_service()
}

use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, oneshot};
use uuid::Uuid;

use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
use crate::config::Config;
use crate::protocol::ProtocolError;
use crate::server::ShutdownReason;
use crate::services::LocalAgentHost;
#[cfg(feature = "local-agents")]
use crate::services::PtyAgentHost;
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
    pub(crate) local_agent_host: Option<Arc<dyn LocalAgentHost>>,
    pub(crate) shutdown_tx: mpsc::Sender<ShutdownRequest>,
}

/// Build the local agent host for this build: `Some` whenever `local-agents`
/// is compiled in (cloud-vs-device is decided by runtime guards, not host
/// presence), `None` for the embedded client. Spawns the host's session-event
/// loop, so it must be called from within a tokio runtime.
fn new_local_agent_host(
    host_id: Uuid,
    socket_path: &std::path::Path,
) -> std::io::Result<Option<Arc<dyn LocalAgentHost>>> {
    #[cfg(feature = "local-agents")]
    {
        PtyAgentHost::new_with_socket_path(host_id, socket_path)
            .map(|host| Some(host as Arc<dyn LocalAgentHost>))
    }
    #[cfg(not(feature = "local-agents"))]
    {
        let _ = (host_id, socket_path);
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
            update_reporter,
            is_cloud_server: false,
            jwt_validator: None,
            // Constructed lazily in `ensure_local_agent_host` from within the
            // async startup path: building it spawns a tokio task, which
            // `ServerState::new` (sync, sometimes off-runtime) cannot do.
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

/// Return this daemon's local agent host, constructing (and storing) it on
/// first call. Must run inside a tokio runtime — building the host spawns its
/// session-event loop. Cloud relays never call this, so they keep `None`.
pub(crate) async fn ensure_local_agent_host(
    state: &Arc<RwLock<ServerState>>,
) -> std::io::Result<Option<Arc<dyn LocalAgentHost>>> {
    let mut guard = state.write().await;
    if guard.local_agent_host.is_none() {
        let host_id = guard.host_id;
        let socket_path = guard.config.socket_path.clone();
        guard.local_agent_host = new_local_agent_host(host_id, &socket_path)?;
    }
    Ok(guard.local_agent_host.clone())
}

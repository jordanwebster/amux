//! A client-only embedded runtime authenticated against the loopback relay.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::identity::load_or_create_device_identity_in;
use crate::routing::spawn_connector_to_channel_with_bearer_token;
use crate::services::{DeviceRuntimeSecurity, StartedUserServices, start_user_services};
use crate::trust::TrustStore;

struct UserClient {
    _services: StartedUserServices,
    _root: tempfile::TempDir,
    tasks: Vec<tokio::task::AbortHandle>,
    sockets: crate::dispatcher::TrackedTcpConnections,
}

impl Drop for UserClient {
    fn drop(&mut self) {
        for socket in self.sockets.lock().unwrap().drain(..) {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Opens the production embedded client services without a local agent host,
/// using a supplied test relay token instead of the production token exchange.
pub async fn connect_user(relay: SocketAddr, token: String) -> anyhow::Result<crate::Client> {
    anyhow::ensure!(relay.ip().is_loopback(), "testnet relay must be loopback");
    let root = tempfile::tempdir()?;
    let identity = load_or_create_device_identity_in(root.path())?;
    let trust = TrustStore::load_or_create_in(root.path())?;
    let (state, _shutdown) =
        super::net::testnet_server_state("testnet-client", identity.host_id, None, true);
    {
        let mut state = state.write().await;
        state.config.data_dir = root.path().to_owned();
        state.config.state_path = root.path().join("state.yaml");
        state.config.socket_path = root.path().join("amux.sock");
        state.config.cloud_url = format!("http://{relay}");
    }
    let services = start_user_services(
        state,
        None,
        DeviceRuntimeSecurity::new(identity, trust, root.path().to_owned()),
    )
    .await?;
    let sockets: crate::dispatcher::TrackedTcpConnections = Arc::default();
    let channel = super::daemon::tracked_cloud_channel(relay, sockets.clone());
    let link =
        spawn_connector_to_channel_with_bearer_token(services.link_connector_ctx(), channel, token);
    let (channel, server) = services.open_in_process_client_channel();
    Ok(crate::Client::from_client_service_channel(
        channel,
        Some(Arc::new(UserClient {
            _services: services,
            _root: root,
            tasks: vec![link.abort_handle(), server.abort_handle()],
            sockets,
        })),
    ))
}

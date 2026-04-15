mod agent_registry;
mod agents;
mod buffer;
mod claude;
mod cloud;
mod config;
mod connect;
mod connection;
mod debug;
mod error;
mod handshake;
mod jwt;
mod message;
mod oauth;
pub mod protocol;
mod route;
mod server;
pub mod setup;
mod sleep_inhibitor;
mod state;
mod transport;
pub mod update;

pub use agents::{SuspendedAgent, SuspendedServerState};
pub use config::{Config, Keybinds, LeaderKey, default_data_dir, default_log_path};
pub use connect::{ConnectPolicy, DaemonOptions, ServerMode, connect, spawn_daemon};
pub use connection::Connection;
pub use error::{AmuxError, Result};
pub use route::Route;

/// Run the amux server with the provided config.
pub async fn run_server(config: Config, cloud: bool) -> Result<()> {
    let mut server = server::Server::with_config(config)?;
    server.run(cloud).await
}

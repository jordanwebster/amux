mod agent;
mod auth;
mod buffer;
mod client;
mod config;
mod debug;
pub mod protocol;
mod server;
pub mod setup;
mod sleep_inhibitor;
mod state;
mod suspend;
mod transport;
pub mod update;

pub use agent::claude::hooks::ClaudeHook;
pub use client::{
    ConnectError, ConnectPolicy, Connection, DaemonOptions, ServerMode, connect, spawn_daemon,
};
pub use config::{Config, ConfigError, Keybinds, LeaderKey, default_data_dir, default_log_path};
pub use protocol::Route;
pub use server::ServerError;
pub use suspend::{SuspendedAgent, SuspendedServerState};
pub use transport::TransportError;

/// Run the amux server with the provided config.
pub async fn run_server(config: Config, cloud: bool) -> std::result::Result<(), ServerError> {
    let mut server = server::Server::with_config(config)?;
    server.run(cloud).await
}

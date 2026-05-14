pub mod agent;
mod auth;
mod buffer;
mod client;
mod config;
mod debug;
mod paths;
pub mod protocol;
mod rpc;
mod server;
mod services;
pub mod setup;
mod sleep_inhibitor;
mod state;
mod suspend;
mod transport;
pub mod update;

pub use client::{
    ConnectError, ConnectPolicy, Connection, DaemonOptions, ResumeSummary, RpcClient,
    RpcClientError, ServerMode, SubscribeSessionClient, SuspendSummary, connect, spawn_daemon,
};
pub use config::{Config, ConfigError, Keybinds, LeaderKey};
pub use paths::{default_data_dir, default_log_path};
pub use server::ServerError;
pub use transport::TransportError;

/// Run the amux server with the provided config.
pub async fn run_server(config: Config, cloud: bool) -> std::result::Result<(), ServerError> {
    let mut server = server::Server::with_config(config)?;
    server.run(cloud).await
}

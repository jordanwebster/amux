mod agent_registry;
mod buffer;
mod claude;
mod cloud;
mod config;
mod connect;
mod connection;
mod error;
mod handshake;
mod jwt;
mod message;
mod oauth;
pub mod protocol;
mod route;
mod server;
mod session;
pub mod setup;
mod state;
mod transport;

pub use config::{Config, default_log_path};
pub use connect::{ConnectPolicy, connect};
pub use connection::Connection;
pub use error::{AmuxError, Result};
pub use route::Route;

/// Run the amux server with the provided config.
pub async fn run_server(config: Config, cloud: bool) -> Result<()> {
    let mut server = server::Server::with_config(config);
    server.run(cloud).await
}

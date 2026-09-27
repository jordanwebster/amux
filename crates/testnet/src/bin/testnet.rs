//! `testnet serve <topology.json>`: starts a topology on wall time, prints
//! one readiness JSON line once it is ready, and serves its door until a
//! driver sends Shutdown or the process is interrupted.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use testnet::Topology;

#[derive(Parser)]
#[command(name = "testnet", about = "The many-daemons harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a topology and serve its door.
    Serve {
        topology: PathBuf,
        /// Where the control socket listens.
        #[arg(long, default_value = "127.0.0.1:0")]
        control: SocketAddr,
        /// Put the net's root at DIR/net instead of a random temporary
        /// directory, so every path a client draws is the same each run.
        #[arg(long, value_name = "DIR")]
        root_in: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let Command::Serve {
        topology,
        control,
        root_in,
    } = Cli::parse().command;
    let topology = Topology::load(&topology)?;
    let options = testnet::NetOptions {
        clock: testnet::ClockMode::Wall,
        root_in,
        ..testnet::NetOptions::default()
    };
    let mut served = testnet::door::serve_with(topology, control, options)
        .await
        .context("serving the topology")?;
    println!("{}", serde_json::to_string(&served.readiness)?);
    tokio::select! {
        () = served.closed() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    served.shutdown().await?;
    Ok(())
}

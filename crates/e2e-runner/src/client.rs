//! A consumer of the public protobuf API, with no amux library dependency.

use std::path::PathBuf;

use clap::Subcommand;
#[cfg(unix)]
use hyper_util::rt::TokioIo;
use tonic::transport::Channel;
#[cfg(unix)]
use tonic::transport::Endpoint;
#[cfg(unix)]
use tower::service_fn;

#[allow(dead_code, clippy::enum_variant_names)]
mod rpc {
    tonic::include_proto!("amux.v1");
}

#[derive(Debug, Subcommand)]
pub enum ClientCommand {
    /// List profiles from the installation's plain gRPC front door
    ListProfiles { front_door: PathBuf },
    /// Discover a profile socket and list its agents through ClientService
    ListAgents {
        front_door: PathBuf,
        profile: String,
    },
}

type Error = Box<dyn std::error::Error + Send + Sync>;

#[cfg(unix)]
async fn connect(socket: PathBuf) -> Result<Channel, Error> {
    Ok(Endpoint::from_static("http://localhost")
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(5))
        .connect_with_connector(service_fn(move |_| {
            let socket = socket.clone();
            async move {
                tokio::net::UnixStream::connect(socket)
                    .await
                    .map(TokioIo::new)
            }
        }))
        .await?)
}

#[cfg(not(unix))]
async fn connect(_socket: PathBuf) -> Result<Channel, Error> {
    Err("Installation sockets require Unix".into())
}

pub async fn run(command: ClientCommand) -> Result<(), Error> {
    let front_door = match &command {
        ClientCommand::ListProfiles { front_door }
        | ClientCommand::ListAgents { front_door, .. } => front_door,
    };
    let mut directory =
        rpc::profile_service_client::ProfileServiceClient::new(connect(front_door.clone()).await?);
    let mut profiles = directory
        .list_profiles(rpc::ListProfilesRequest {})
        .await?
        .into_inner()
        .profiles;
    profiles.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
    match command {
        ClientCommand::ListProfiles { .. } => {
            println!("Profiles:");
            for profile in profiles {
                println!(
                    "  {}  {}  {}  {}",
                    profile.id, profile.label, profile.email, profile.socket_path
                );
            }
        }
        ClientCommand::ListAgents {
            profile: selector, ..
        } => {
            let matches: Vec<_> = profiles
                .iter()
                .filter(|p| p.id == selector || p.label == selector)
                .collect();
            let [profile] = matches.as_slice() else {
                return Err(format!(
                    "Expected one profile matching {selector:?}, found {}",
                    matches.len()
                )
                .into());
            };
            let mut client = rpc::client_service_client::ClientServiceClient::new(
                connect(PathBuf::from(&profile.socket_path)).await?,
            );
            let mut agents = client
                .list_agents(rpc::ListAgentsRequest {})
                .await?
                .into_inner()
                .agents;
            agents.sort_by(|a, b| a.name.cmp(&b.name));
            println!("Agents for {}:", profile.label);
            for agent in agents {
                println!(
                    "  {}  {}",
                    agent.name.as_deref().unwrap_or("(unnamed)"),
                    agent.working_dir
                );
            }
        }
    }
    Ok(())
}

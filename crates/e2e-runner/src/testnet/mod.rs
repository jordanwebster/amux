//! A loopback process boundary around the same TestNet used by the Rust specs.

use std::collections::HashSet;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use amux::testnet::{TestNet, Via};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use uuid::Uuid;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Start a topology and print one readiness JSON line.
    Serve {
        #[arg(long)]
        topology: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topology {
    pub users: Vec<String>,
    pub daemons: Vec<DaemonDecl>,
    pub paired: Vec<(String, String, PairVia)>,
    pub agents: Vec<AgentDecl>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonDecl {
    pub name: String,
    pub user: String,
    pub repository_roots: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub enum PairVia {
    Tcp,
    Cloud,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDecl {
    pub name: String,
    pub daemon: String,
    pub working_dir: PathBuf,
    pub provider: ScriptedProvider,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ScriptedProvider {
    Claude { script: PathBuf },
    Codex { recording: PathBuf },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Readiness {
    pub relay: SocketAddr,
    pub control: SocketAddr,
    pub users: Vec<UserCredential>,
    pub daemons: Vec<DaemonIdentity>,
    pub agents: Vec<AgentIdentity>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserCredential {
    pub label: String,
    pub user_id: Uuid,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonIdentity {
    pub name: String,
    pub host_id: Uuid,
    pub fingerprint: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub name: String,
    pub daemon: String,
    pub agent_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub enum Control {
    Shutdown,
}

#[derive(Debug, Serialize)]
pub enum Reply {
    Ack {
        pin: Option<String>,
        qr: Option<String>,
        observed: Vec<serde_json::Value>,
        connections: Option<u32>,
    },
    Error {
        message: String,
    },
}

impl Reply {
    fn ack() -> Self {
        Self::Ack {
            pin: None,
            qr: None,
            observed: Vec::new(),
            connections: None,
        }
    }
}

impl Topology {
    fn load(path: &Path) -> Result<Self> {
        let mut topology: Self = serde_json::from_slice(
            &std::fs::read(path).with_context(|| format!("read topology {}", path.display()))?,
        )
        .context("parse topology")?;
        let base = path
            .canonicalize()?
            .parent()
            .context("topology directory")?
            .to_owned();
        let mut users = HashSet::new();
        for user in &topology.users {
            ensure!(
                !user.is_empty() && users.insert(user),
                "empty or duplicate user: {user}"
            );
        }
        let mut daemons = HashSet::new();
        for daemon in &mut topology.daemons {
            // Daemon names become directory components inside TestNet's temporary root.
            ensure!(
                valid_name(&daemon.name) && daemons.insert(daemon.name.clone()),
                "invalid or duplicate daemon: {}",
                daemon.name
            );
            ensure!(
                users.contains(&daemon.user),
                "unknown user: {}",
                daemon.user
            );
            for root in &mut daemon.repository_roots {
                *root = resolve_directory(&base, root)?;
            }
        }
        for (a, b, via) in &topology.paired {
            ensure!(
                a != b && daemons.contains(a) && daemons.contains(b),
                "invalid pair: {a}, {b}"
            );
            if matches!(via, PairVia::Cloud) {
                let user = |name: &str| {
                    &topology
                        .daemons
                        .iter()
                        .find(|d| d.name == name)
                        .unwrap()
                        .user
                };
                ensure!(user(a) == user(b), "cloud pair crosses users: {a}, {b}");
            }
        }
        let mut agents = HashSet::new();
        for agent in &mut topology.agents {
            ensure!(
                valid_name(&agent.name) && agents.insert(agent.name.clone()),
                "invalid or duplicate agent: {}",
                agent.name
            );
            ensure!(
                daemons.contains(&agent.daemon),
                "unknown agent daemon: {}",
                agent.daemon
            );
            agent.working_dir = resolve_directory(&base, &agent.working_dir)?;
            match &mut agent.provider {
                ScriptedProvider::Claude { script } => {
                    *script = base.join(&*script);
                    let value: serde_json::Value = serde_json::from_slice(
                        &std::fs::read(&*script)
                            .with_context(|| format!("read script {}", script.display()))?,
                    )?;
                    ensure!(
                        value
                            .get("reactions")
                            .and_then(|v| v.as_array())
                            .is_some_and(|v| v.is_empty()),
                        "script reaction playback is not installed yet"
                    );
                }
                ScriptedProvider::Codex { recording } => bail!(
                    "Codex recording playback is not installed yet: {}",
                    recording.display()
                ),
            }
        }
        Ok(topology)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn resolve_directory(base: &Path, path: &Path) -> Result<PathBuf> {
    let path = base
        .join(path)
        .canonicalize()
        .with_context(|| format!("resolve directory {}", path.display()))?;
    ensure!(path.is_dir(), "not a directory: {}", path.display());
    Ok(path)
}

async fn start(topology: &Topology, control: SocketAddr) -> (TestNet, Readiness) {
    let mut builder = TestNet::builder().cloud();
    for daemon in &topology.daemons {
        builder = builder.daemon(&daemon.name).cloud_user(&daemon.user);
    }
    for (a, b, via) in &topology.paired {
        builder = builder.paired(
            a,
            b,
            match via {
                PairVia::Tcp => Via::Tcp,
                PairVia::Cloud => Via::Cloud,
            },
        );
    }
    let net = builder.start().await;
    let users = topology
        .users
        .iter()
        .map(|label| {
            let (user_id, token) = net.user_credentials(label);
            UserCredential {
                label: label.clone(),
                user_id,
                token,
            }
        })
        .collect();
    let daemons = topology
        .daemons
        .iter()
        .map(|decl| {
            let daemon = net.daemon(&decl.name);
            let (host_id, public_key) = daemon.identity_on_disk();
            DaemonIdentity {
                name: decl.name.clone(),
                host_id,
                fingerprint: format!("{:x}", Sha256::digest(public_key)),
            }
        })
        .collect();
    let mut agents = Vec::new();
    for decl in &topology.agents {
        let agent = net
            .daemon(&decl.daemon)
            .register_scripted_claude_agent(Uuid::new_v4(), &decl.name, &decl.working_dir)
            .await;
        agents.push(AgentIdentity {
            name: decl.name.clone(),
            daemon: decl.daemon.clone(),
            agent_id: agent.id,
        });
    }
    let readiness = Readiness {
        relay: net.relay_addr(),
        control,
        users,
        daemons,
        agents,
    };
    (net, readiness)
}

struct Request {
    control: Control,
    reply: oneshot::Sender<Reply>,
    flushed: oneshot::Receiver<()>,
}

async fn connection(stream: TcpStream, requests: mpsc::Sender<Request>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let (reply, flushed) = match serde_json::from_str(&line) {
            Ok(control) => {
                let (send, receive) = oneshot::channel();
                let (flushed, flushed_rx) = oneshot::channel();
                requests
                    .send(Request {
                        control,
                        reply: send,
                        flushed: flushed_rx,
                    })
                    .await?;
                (receive.await?, Some(flushed))
            }
            Err(error) => (
                Reply::Error {
                    message: error.to_string(),
                },
                None,
            ),
        };
        let mut encoded = serde_json::to_vec(&reply)?;
        encoded.push(b'\n');
        write.write_all(&encoded).await?;
        if let Some(flushed) = flushed {
            let _ = flushed.send(());
        }
    }
    Ok(())
}

async fn serve(topology: Topology) -> Result<()> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let termination = async {
        #[cfg(unix)]
        terminate.recv().await;
        #[cfg(not(unix))]
        std::future::pending::<()>().await;
    };
    tokio::pin!(termination);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let (net, readiness) = tokio::time::timeout(
        Duration::from_secs(30),
        start(&topology, listener.local_addr()?),
    )
    .await
    .context("topology did not become ready within 30 seconds")?;
    println!("{}", serde_json::to_string(&readiness)?);
    std::io::stdout().flush()?;
    let (send, mut receive) = mpsc::channel::<Request>(32);
    let mut connections = JoinSet::new();
    let outcome: Result<Option<Request>> = loop {
        tokio::select! {
            _ = &mut termination => break Ok(None),
            result = tokio::signal::ctrl_c() => break result.map(|()| None).map_err(Into::into),
            result = listener.accept(), if connections.len() < 64 => match result {
                Ok((stream, _)) => { connections.spawn(connection(stream, send.clone())); }
                Err(error) => break Err(error.into()),
            },
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result { eprintln!("control task: {error}"); }
            }
            Some(request) = receive.recv() => match request.control {
                Control::Shutdown => break Ok(Some(request)),
            }
        }
    };
    drop(listener);
    net.shutdown().await;
    let outcome = match outcome {
        Ok(Some(request)) => {
            let _ = request.reply.send(Reply::ack());
            let _ = tokio::time::timeout(Duration::from_secs(1), request.flushed).await;
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => Err(error),
    };
    connections.shutdown().await;
    outcome
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Serve { topology } => {
            let topology = Topology::load(&topology)?;
            // Drop the executor before returning: detached transport tasks cannot
            // retain listeners or outlive a successfully terminated runner.
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?
                .block_on(serve(topology))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn testnet_serve_starts_real_pairings_agents_and_user_credentials() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e-tests/topologies/two-hosts.json");
        let topology = Topology::load(&path).unwrap();
        let (net, ready) = start(&topology, "127.0.0.1:1".parse().unwrap()).await;
        let [laptop, desktop] = net.daemons(["laptop", "desktop"]);
        laptop.trusts(&desktop).await;
        desktop.trusts(&laptop).await;
        laptop.can_call(&desktop).await;
        assert_eq!(laptop.lists_agents_on(&desktop).await.unwrap(), ["helper"]);
        assert_eq!(ready.agents.len(), 1);
        for user in &ready.users {
            assert_eq!(
                net.user_credentials(&user.label),
                (user.user_id, user.token.clone())
            );
        }
        assert_eq!(
            ready
                .users
                .iter()
                .map(|u| u.user_id)
                .collect::<HashSet<_>>()
                .len(),
            3
        );
        assert_eq!(
            ready
                .users
                .iter()
                .map(|u| &u.token)
                .collect::<HashSet<_>>()
                .len(),
            3
        );
        let (_, key) = desktop.identity_on_disk();
        assert_eq!(
            ready.daemons[1].fingerprint,
            format!("{:x}", Sha256::digest(key))
        );
        drop((laptop, desktop));
        net.shutdown().await;
    }

    #[test]
    fn testnet_serve_rejects_invalid_topology_before_starting() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bad.json");
        for (users, daemons, paired) in [
            (
                serde_json::json!(["u", "u"]),
                serde_json::json!([]),
                serde_json::json!([]),
            ),
            (
                serde_json::json!(["u"]),
                serde_json::json!([{"name":"../escape","user":"u","repository_roots":[]}]),
                serde_json::json!([]),
            ),
            (
                serde_json::json!(["u"]),
                serde_json::json!([{"name":"host","user":"missing","repository_roots":[]}]),
                serde_json::json!([]),
            ),
            (
                serde_json::json!(["u"]),
                serde_json::json!([]),
                serde_json::json!([["a", "b", "Tcp"]]),
            ),
        ] {
            std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"users":users,"daemons":daemons,"paired":paired,"agents":[]})).unwrap()).unwrap();
            assert!(Topology::load(&path).is_err());
        }
    }
}

//! A loopback process boundary around the same TestNet used by the Rust specs.

#[cfg(unix)]
mod codex_recording;
mod report_script;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::Duration;

use amux::testnet::script::{ObservedInput, Provider, Script, ScriptAsk, Step};
use amux::testnet::{Daemon, TestNet, Via};
use anyhow::{Context, Result, bail, ensure};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use uuid::Uuid;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Convert a complete Claude report transcript into a playback script.
    ScriptFromReport { msgs: PathBuf },
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
    #[serde(skip)]
    scripts: HashMap<String, Script>,
    #[serde(skip)]
    sdk_scripts: HashMap<String, amux::testnet::sdk::Script>,
    #[serde(skip)]
    #[cfg(unix)]
    recordings: HashMap<String, codex_recording::Prepared>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonDecl {
    pub name: String,
    pub user: String,
    pub repository_roots: Vec<PathBuf>,
    /// Provider transport for every SDK session this host creates, including
    /// requests that arrive later from a paired client.
    #[serde(default)]
    pub sdk_script: Option<PathBuf>,
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
    ClaudeSdk { model: String },
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

#[derive(Debug, Serialize, Deserialize)]
pub enum Control {
    CloudOffline,
    CloudOnline,
    SeverDirect {
        a: String,
        b: String,
    },
    EstablishDirect {
        a: String,
        b: String,
    },
    RestartDaemon {
        name: String,
    },
    Unpair {
        daemon: String,
        peer: String,
    },
    StartPinPairing {
        daemon: String,
        ttl_secs: u64,
    },
    StartQrPairing {
        daemon: String,
    },
    Latency {
        millis: u64,
    },
    AgentEmit {
        agent: String,
        rows: Vec<serde_json::Value>,
    },
    /// Plays a sequence of provider steps at an agent, whatever they are.
    ///
    /// The verbs beside this one each name one thing a script can do, which is
    /// what a caller reaching for that thing wants to say. A driver proving
    /// that a client renders every kind of step there is wants the opposite:
    /// the whole vocabulary, so that adding a step kind to the script makes
    /// that driver's claim incomplete rather than silently narrower.
    AgentPlay {
        agent: String,
        steps: Vec<Step>,
    },
    AgentRaiseAsk {
        agent: String,
        ask: ScriptAsk,
    },
    AgentEndTurn {
        agent: String,
    },
    AgentExit {
        agent: String,
        code: i32,
    },
    AgentSpawnChild {
        agent: String,
        child: String,
    },
    AgentVerifyReplay {
        agent: String,
    },
    AgentObserve {
        agent: String,
    },
    /// What is connected to what, as the far side sees it.
    ///
    /// A machine, by name, answers with the links it is holding. A cloud user,
    /// by label, answers with the relay's own view of that account: one entry
    /// per host connected to it, and how many links each one holds — which is
    /// where a client multiplexing everything over one connection is told
    /// apart from one opening a connection per thing it watches, and where a
    /// client that has gone away stops appearing.
    Connections {
        #[serde(default)]
        daemon: Option<String>,
        #[serde(default)]
        user: Option<String>,
    },
    /// What a machine itself says it is holding: every agent on it with the
    /// kind and driver that machine recorded, and every device it trusts.
    ///
    /// This is the far side's own account, not a client's. A driver proving
    /// what an agent it started really is has to read it here: a phone
    /// reporting the kind it asked for would be quoting its own request back,
    /// and the whole question is whether the machine agrees.
    Inventory {
        daemon: String,
    },
    Shutdown,
}

/// One agent as the machine running it describes it.
#[derive(Debug, Serialize, Deserialize)]
pub struct InventoryAgent {
    pub id: Uuid,
    pub name: String,
    pub working_dir: String,
    /// `claude`, `codex` or `test-agent`.
    pub kind: String,
    /// `pty` or `sdk` for Claude, and nothing for the layers that have no
    /// driver to choose. The machine refuses a create request that leaves this
    /// unspecified, so what stands here is what the request named.
    pub driver: Option<String>,
}

/// One device the machine holds a key for.
#[derive(Debug, Serialize, Deserialize)]
pub struct InventoryDevice {
    pub host: Uuid,
    pub name: String,
    pub fingerprint: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Ack {
        pin: Option<String>,
        qr: Option<String>,
        observed: Vec<ObservedInput>,
        /// Raw stdin seen by the SDK transport, addressed by agent identity.
        sdk_inputs: Vec<serde_json::Value>,
        connections: Option<u32>,
        /// One entry per host, as `"<host id>: <links>"`, sorted. Present for
        /// a `Connections` about a cloud user.
        links: Vec<String>,
        /// What a machine says it is running, for an `Inventory`.
        agents: Vec<InventoryAgent>,
        /// What a machine says it trusts, for an `Inventory`.
        devices: Vec<InventoryDevice>,
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
            sdk_inputs: Vec::new(),
            connections: None,
            links: Vec::new(),
            agents: Vec::new(),
            devices: Vec::new(),
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
            if let Some(script) = &daemon.sdk_script {
                let script = base.join(script);
                topology.sdk_scripts.insert(
                    daemon.name.clone(),
                    serde_json::from_slice(
                        &std::fs::read(&script)
                            .with_context(|| format!("read SDK script {}", script.display()))?,
                    )
                    .context("parse SDK script")?,
                );
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
                ScriptedProvider::ClaudeSdk { .. } => {
                    ensure!(
                        topology.sdk_scripts.contains_key(&agent.daemon),
                        "SDK agent {} needs its host's sdk_script",
                        agent.name
                    );
                }
                ScriptedProvider::Claude { script } => {
                    *script = base.join(&*script);
                    let script: Script = serde_json::from_slice(
                        &std::fs::read(&*script)
                            .with_context(|| format!("read script {}", script.display()))?,
                    )
                    .context("parse Claude script")?;
                    topology.scripts.insert(agent.name.clone(), script);
                }
                #[cfg(unix)]
                ScriptedProvider::Codex { recording } => {
                    *recording = base.join(&*recording);
                    topology.recordings.insert(
                        agent.name.clone(),
                        codex_recording::Prepared::load(recording)?,
                    );
                }
                #[cfg(not(unix))]
                ScriptedProvider::Codex { .. } => {
                    bail!("Codex recordings require a Unix host")
                }
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

struct ScriptedAgent {
    daemon: String,
    agent: amux::Agent,
    provider: AgentProvider,
    /// The script this agent answers with, kept so a child spawned from it
    /// answers the same way. A child raised through this control channel is
    /// driven by the same test as its parent, and one that refused every
    /// answer because it was given an empty script would make answering a
    /// child's ask impossible to drive.
    script: Script,
}

enum AgentProvider {
    Claude(Provider),
    ClaudeSdk,
    #[cfg(unix)]
    Codex(codex_recording::Recorded),
}

impl AgentProvider {
    fn claude(&self) -> Result<&Provider> {
        match self {
            Self::Claude(provider) => Ok(provider),
            Self::ClaudeSdk => {
                bail!("SDK sessions accept stream-JSON inputs through their own layer")
            }
            #[cfg(unix)]
            Self::Codex(_) => bail!("Codex recordings accept only recorded client interactions"),
        }
    }

    async fn play(&self, steps: Vec<Step>) -> Result<()> {
        self.claude()?.play(steps).await?;
        Ok(())
    }

    async fn close(&mut self) {
        #[cfg(unix)]
        if let Self::Codex(recorded) = self {
            recorded.close().await;
        }
    }
}

type Agents = HashMap<String, ScriptedAgent>;

async fn start(topology: &Topology, control: SocketAddr) -> Result<(TestNet, Readiness, Agents)> {
    let mut builder = TestNet::builder().cloud();
    for daemon in &topology.daemons {
        builder = builder
            .daemon(&daemon.name)
            .cloud_user(&daemon.user)
            .repository_roots(daemon.repository_roots.clone());
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
    for (name, script) in &topology.sdk_scripts {
        net.daemon(name).script_sdk_sessions(script.clone()).await;
    }
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
    let mut scripted = HashMap::new();
    for decl in &topology.agents {
        let daemon = net.daemon(&decl.daemon);
        let (agent, provider) = match &decl.provider {
            ScriptedProvider::ClaudeSdk { model } => {
                let agent = daemon
                    .admin_client()
                    .await
                    .create_agent(amux::CreateAgentRequest {
                        agent_id: Uuid::new_v4(),
                        host_id: None,
                        name: Some(decl.name.clone()),
                        agent_type: amux::AgentType::Claude {
                            driver: amux::ClaudeDriver::Sdk,
                        },
                        working_dir: decl.working_dir.clone(),
                        terminal_size: None,
                        args: vec!["--model".into(), model.clone()],
                        parent: None,
                        initial_prompt: None,
                    })
                    .await?;
                (agent, AgentProvider::ClaudeSdk)
            }
            ScriptedProvider::Claude { .. } => {
                let (agent, provider) = daemon
                    .spawn_scripted_agent(
                        &decl.name,
                        &decl.working_dir,
                        topology.scripts[&decl.name].clone(),
                        None,
                    )
                    .await?;
                (agent, AgentProvider::Claude(provider))
            }
            #[cfg(unix)]
            ScriptedProvider::Codex { .. } => {
                let (session, recorded) = topology.recordings[&decl.name].open().await?;
                let agent = daemon
                    .spawn_recorded_codex(&decl.name, &decl.working_dir, session)
                    .await?;
                (agent, AgentProvider::Codex(recorded))
            }
            #[cfg(not(unix))]
            ScriptedProvider::Codex { .. } => bail!("Codex recordings require a Unix host"),
        };
        agents.push(AgentIdentity {
            name: decl.name.clone(),
            daemon: decl.daemon.clone(),
            agent_id: agent.id,
        });
        scripted.insert(
            decl.name.clone(),
            ScriptedAgent {
                daemon: decl.daemon.clone(),
                agent,
                provider,
                script: topology
                    .scripts
                    .get(&decl.name)
                    .cloned()
                    .unwrap_or_default(),
            },
        );
    }
    let readiness = Readiness {
        relay: net.relay_addr(),
        control,
        users,
        daemons,
        agents,
    };
    Ok((net, readiness, scripted))
}

struct Request {
    control: Control,
    reply: oneshot::Sender<Reply>,
    flushed: oneshot::Receiver<()>,
}

async fn apply(
    net: &TestNet,
    names: &HashSet<String>,
    agents: &mut Agents,
    control: Control,
) -> Result<Reply> {
    let daemon = |name: &str| -> Result<Daemon> {
        ensure!(names.contains(name), "unknown daemon: {name}");
        Ok(net.daemon(name))
    };
    let pair = |a: &str, b: &str| -> Result<(Daemon, Daemon)> {
        ensure!(a != b, "a host cannot be its own peer");
        Ok((daemon(a)?, daemon(b)?))
    };
    let scripted = |name: &str| -> Result<&ScriptedAgent> {
        agents
            .get(name)
            .with_context(|| format!("unknown or stopped scripted agent: {name}"))
    };
    let mut reply = Reply::ack();
    match control {
        Control::CloudOffline => net.cloud_offline().await,
        Control::CloudOnline => net.cloud_online().await,
        Control::SeverDirect { a, b } => {
            let (a, b) = pair(&a, &b)?;
            net.sever_direct(&a, &b).await;
        }
        Control::EstablishDirect { a, b } => {
            let (a, b) = pair(&a, &b)?;
            net.try_establish_direct(&a, &b).await?;
        }
        Control::RestartDaemon { name } => {
            net.restart_daemon(&daemon(&name)?).await;
            for agent in agents.values_mut().filter(|agent| agent.daemon == name) {
                agent.provider.close().await;
            }
            agents.retain(|_, agent| agent.daemon != name);
        }
        Control::Unpair { daemon, peer } => {
            let (daemon, peer) = pair(&daemon, &peer)?;
            daemon
                .pairing_admin()
                .await
                .unpair(peer.host_id(), "testnet control revocation")
                .await?;
            daemon.does_not_trust(&peer).await;
        }
        Control::StartPinPairing {
            daemon: name,
            ttl_secs,
        } => {
            ensure!(ttl_secs <= 3600, "pairing TTL must not exceed one hour");
            let pin = daemon(&name)?
                .try_start_pairing_with_ttl(Duration::from_secs(ttl_secs))
                .await?;
            if let Reply::Ack { pin: output, .. } = &mut reply {
                *output = Some(pin.to_string());
            }
        }
        Control::StartQrPairing { daemon: name } => {
            let mut start = daemon(&name)?
                .pairing_admin()
                .await
                .start_qr_pairing()
                .await?;
            start.cloud_url = format!("http://{}", net.relay_addr());
            let amux::PairingSecret::QrSecret(secret) = &start.secret else {
                bail!("QR pairing returned a PIN");
            };
            if let Reply::Ack { qr, .. } = &mut reply {
                *qr = Some(amux::encode_qr_pairing_payload(&start, secret)?);
            }
        }
        Control::Latency { millis } => {
            ensure!(millis <= 1000, "relay latency must not exceed 1000 ms");
            net.set_relay_latency(millis);
        }
        Control::AgentEmit { agent, rows } => {
            scripted(&agent)?
                .provider
                .play(vec![Step::Rows { jsonl: rows }])
                .await?;
        }
        Control::AgentPlay { agent, steps } => {
            ensure!(!steps.is_empty(), "a play must carry at least one step");
            scripted(&agent)?.provider.play(steps).await?;
        }
        Control::AgentRaiseAsk { agent, ask } => {
            scripted(&agent)?
                .provider
                .play(vec![Step::Ask(ask)])
                .await?;
        }
        Control::AgentEndTurn { agent } => {
            scripted(&agent)?.provider.play(vec![Step::EndTurn]).await?;
        }
        Control::AgentExit { agent, code } => {
            ensure!(code >= 0, "exit code must be nonnegative");
            scripted(&agent)?
                .provider
                .play(vec![Step::Exit { code }])
                .await?;
        }
        Control::AgentVerifyReplay { agent } => match &scripted(&agent)?.provider {
            #[cfg(unix)]
            AgentProvider::Codex(recorded) => {
                recorded.verify()?;
            }
            AgentProvider::Claude(_) | AgentProvider::ClaudeSdk => {
                bail!("agent has no Codex recording")
            }
        },
        Control::AgentObserve { agent } => {
            if let Reply::Ack {
                observed,
                sdk_inputs,
                ..
            } = &mut reply
            {
                let scripted = agents.get(&agent).or_else(|| {
                    let id: Uuid = agent.parse().ok()?;
                    agents.values().find(|scripted| scripted.agent.id == id)
                });
                if let Some(ScriptedAgent {
                    provider: AgentProvider::Claude(provider),
                    ..
                }) = scripted
                {
                    *observed = provider.observed();
                } else {
                    let id = scripted
                        .map(|agent| agent.agent.id)
                        .or_else(|| agent.parse().ok())
                        .context("SDK observation needs an agent name or UUID")?;
                    let mut found = None;
                    for name in names {
                        if let Some(inputs) = daemon(name)?.observed_sdk_inputs(id).await {
                            ensure!(found.is_none(), "SDK identity exists on more than one host");
                            found = Some(inputs);
                        }
                    }
                    *sdk_inputs = found.context("no scripted SDK session has this identity")?;
                }
            }
        }
        Control::AgentSpawnChild { agent, child } => {
            ensure!(
                valid_name(&child) && !agents.contains_key(&child),
                "invalid or duplicate child: {child}"
            );
            let parent = scripted(&agent)?;
            ensure!(
                parent.provider.claude()?.error().is_none(),
                "parent provider has stopped"
            );
            let host = daemon(&parent.daemon)?;
            let script = parent.script.clone();
            let (agent, provider) = host
                .spawn_scripted_agent(
                    &child,
                    &parent.agent.working_dir,
                    script.clone(),
                    Some(amux::AgentParent {
                        agent_id: parent.agent.id,
                        host_id: parent.agent.host_id,
                    }),
                )
                .await?;
            agents.insert(
                child,
                ScriptedAgent {
                    daemon: parent.daemon.clone(),
                    agent,
                    provider: AgentProvider::Claude(provider),
                    script,
                },
            );
        }
        Control::Connections {
            daemon: name,
            user: label,
        } => {
            let mut counted = 0usize;
            let mut per_host = Vec::new();
            if let Some(name) = name {
                let dump = daemon(&name)?.debug_dump(false).await;
                counted += dump["links"]
                    .as_array()
                    .context("daemon diagnostics omitted links")?
                    .len();
            }
            if let Some(label) = label {
                for (host, links) in net.cloud_links(&label).await {
                    counted += links;
                    per_host.push(format!("{host}: {links}"));
                }
            }
            if let Reply::Ack {
                connections, links, ..
            } = &mut reply
            {
                *connections = Some(counted.try_into()?);
                *links = per_host;
            }
        }
        Control::Inventory { daemon: name } => {
            let machine = daemon(&name)?;
            let running = machine.admin_client().await.list_agents().await?;
            let trusted = machine.pairing_admin().await.list_peers().await?;
            if let Reply::Ack {
                agents, devices, ..
            } = &mut reply
            {
                *agents = running
                    .into_iter()
                    .map(|agent| InventoryAgent {
                        id: agent.id,
                        name: agent.name.unwrap_or_default(),
                        working_dir: agent.working_dir.display().to_string(),
                        kind: match agent.kind {
                            amux::AgentKind::Claude { .. } => "claude",
                            amux::AgentKind::Codex => "codex",
                            amux::AgentKind::TestAgent => "test-agent",
                        }
                        .to_string(),
                        driver: match agent.kind {
                            amux::AgentKind::Claude {
                                driver: amux::ClaudeDriver::Pty,
                            } => Some("pty".to_string()),
                            amux::AgentKind::Claude {
                                driver: amux::ClaudeDriver::Sdk,
                            } => Some("sdk".to_string()),
                            _ => None,
                        },
                    })
                    .collect();
                *devices = trusted
                    .into_iter()
                    .map(|peer| InventoryDevice {
                        host: peer.host_id,
                        name: peer.name,
                        fingerprint: peer.fingerprint,
                    })
                    .collect();
            }
        }
        Control::Shutdown => unreachable!("shutdown is handled by the server loop"),
    }
    Ok(reply)
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
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let (net, readiness, agents) = tokio::time::timeout(
        Duration::from_secs(30),
        start(&topology, listener.local_addr()?),
    )
    .await
    .context("topology did not become ready within 30 seconds")??;
    println!("{}", serde_json::to_string(&readiness)?);
    std::io::stdout().flush()?;
    serve_net(
        net,
        listener,
        topology.daemons.into_iter().map(|d| d.name).collect(),
        agents,
    )
    .await
}

async fn serve_net(
    net: TestNet,
    listener: TcpListener,
    names: HashSet<String>,
    mut agents: Agents,
) -> Result<()> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let termination = async {
        #[cfg(unix)]
        terminate.recv().await;
        #[cfg(not(unix))]
        std::future::pending::<()>().await;
    };
    tokio::pin!(termination);
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
                control => {
                    // TestNet's assertion verbs panic with topology diagnostics.
                    // Preserve those diagnostics as a control failure for the caller.
                    let operation = AssertUnwindSafe(apply(&net, &names, &mut agents, control)).catch_unwind();
                    let reply = match tokio::time::timeout(Duration::from_secs(30), operation).await {
                        Ok(Ok(Ok(reply))) => reply,
                        Ok(Ok(Err(error))) => Reply::Error { message: error.to_string() },
                        Ok(Err(error)) => Reply::Error { message: error.downcast_ref::<String>().cloned()
                            .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_else(|| "network assertion failed".into()) },
                        Err(_) => Reply::Error { message: "network did not settle within 30 seconds".into() },
                    };
                    let _ = request.reply.send(reply);
                }
            }
        }
    };
    drop(listener);
    net.shutdown().await;
    for agent in agents.values_mut() {
        agent.provider.close().await;
    }
    drop(agents);
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
        Command::ScriptFromReport { msgs } => {
            let snapshot = report_script::read_snapshot(&msgs)?;
            let script = report_script::script_from_report(&snapshot)?;
            println!("{}", serde_json::to_string_pretty(&script)?);
            Ok(())
        }
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
mod agents_tests;
#[cfg(all(test, unix))]
mod codex_tests;
#[cfg(test)]
mod sdk_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) struct ControlClient(BufReader<TcpStream>);

    impl ControlClient {
        pub(super) async fn connect(address: SocketAddr) -> Self {
            Self(BufReader::new(TcpStream::connect(address).await.unwrap()))
        }

        pub(super) async fn request(&mut self, control: serde_json::Value) -> serde_json::Value {
            let mut bytes = serde_json::to_vec(&control).unwrap();
            bytes.push(b'\n');
            self.0.get_mut().write_all(&bytes).await.unwrap();
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(35), self.0.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let reply = serde_json::from_str(&line).unwrap();
            eprintln!("control {control} => {reply}");
            reply
        }

        pub(super) async fn ack(&mut self, control: serde_json::Value) -> serde_json::Value {
            let reply = self.request(control).await;
            assert!(reply.get("Ack").is_some(), "{reply}");
            reply["Ack"].clone()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn testnet_control_every_network_verb_is_observed_by_another_client() {
        use serde_json::json;
        let net = TestNet::builder()
            .cloud()
            .daemon("a")
            .daemon("b")
            .daemon("c")
            .paired("a", "b", Via::Tcp)
            .start()
            .await;
        let [a, b, c] = net.daemons(["a", "b", "c"]);
        let identity = a.identity_on_disk();
        let relay = net.relay_addr();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = serve_net(
            net,
            listener,
            ["a", "b", "c"].map(String::from).into(),
            HashMap::new(),
        );
        let exercise = async {
            let mut control = ControlClient::connect(address).await;
            let mut second = ControlClient::connect(address).await;
            let observer = b.pairing_admin().await;
            async fn count(client: &mut ControlClient, name: &str) -> u64 {
                client.ack(json!({"Connections":{"daemon":name}})).await["connections"]
                    .as_u64()
                    .unwrap()
            }
            assert_eq!(count(&mut second, "b").await, 2);
            control.ack(json!("CloudOffline")).await;
            assert_eq!(count(&mut second, "b").await, 1);
            assert!(TcpStream::connect(relay).await.is_err());
            assert!(
                b.admin_client()
                    .await
                    .list_hosts()
                    .await
                    .unwrap()
                    .iter()
                    .any(|h| h.id == a.host_id() && h.online)
            );
            assert!(b.lists_agents_on(&a).await.is_ok());

            control.ack(json!("CloudOnline")).await;
            assert_eq!(count(&mut second, "b").await, 2);
            // A repeated online command must not create a second relay connection.
            control.ack(json!("CloudOnline")).await;
            assert_eq!(count(&mut second, "b").await, 2);
            control.ack(json!({"SeverDirect":{"a":"a","b":"b"}})).await;
            assert_eq!(count(&mut second, "b").await, 1);
            assert!(b.lists_agents_on(&a).await.is_ok());

            control.ack(json!({"Latency":{"millis":100}})).await;
            let start = tokio::time::Instant::now();
            assert!(b.lists_agents_on(&a).await.is_ok());
            assert!(
                start.elapsed() >= Duration::from_millis(100),
                "real routed call must traverse delayed relay bytes"
            );
            eprintln!(
                "routed call with 100 ms relay latency: {:?}",
                start.elapsed()
            );
            control.ack(json!({"Latency":{"millis":0}})).await;
            assert!(b.lists_agents_on(&a).await.is_ok());
            control
                .ack(json!({"EstablishDirect":{"a":"a","b":"b"}}))
                .await;
            assert_eq!(count(&mut second, "b").await, 2);
            let stream = b.open_event_stream_to(&a).await;
            control.ack(json!({"RestartDaemon":{"name":"a"}})).await;
            stream.expect_disconnect().await;
            assert_eq!(a.identity_on_disk(), identity);
            assert_eq!(count(&mut second, "a").await, 2);
            assert!(b.lists_agents_on(&a).await.is_ok());

            control
                .ack(json!({"Unpair":{"daemon":"a","peer":"b"}}))
                .await;
            assert!(a.pairing_admin().await.get_peer(b.host_id()).await.is_err());
            assert!(b.lists_agents_on(&a).await.is_err());
            let pin = control
                .ack(json!({"StartPinPairing":{"daemon":"b","ttl_secs":1}}))
                .await["pin"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(pin.len(), 6);
            assert!(observer.pairing_is_active().await.unwrap());
            assert!(
                control
                    .request(json!({"StartPinPairing":{"daemon":"b","ttl_secs":30}}))
                    .await
                    .get("Error")
                    .is_some()
            );
            b.pair_mode_ends().await;
            assert!(c.pair(&b).with_pin(&pin).await.is_err());
            let pin = control
                .ack(json!({"StartPinPairing":{"daemon":"b","ttl_secs":30}}))
                .await["pin"]
                .as_str()
                .unwrap()
                .to_owned();
            c.pair(&b).with_pin(&pin).await.unwrap();
            assert!(!observer.pairing_is_active().await.unwrap());
            c.can_call(&b).await;

            // What 'b' itself says it is holding, which is where a driver
            // reads the far side's own account of a pairing or a creation
            // rather than the asking client's. Both peers are there: 'a',
            // because revocation is local — 'a' removed 'b' from its own
            // trust store and closed the link, which leaves 'b' unable to
            // call 'a' but still holding the pin it granted 'a' itself — and
            // 'c', which 'b' has just let in by code.
            let held = control.ack(json!({"Inventory":{"daemon":"b"}})).await;
            let mut holding = held["devices"]
                .as_array()
                .unwrap()
                .iter()
                .map(|device| device["host"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            holding.sort();
            let mut both = vec![a.host_id().to_string(), c.host_id().to_string()];
            both.sort();
            assert_eq!(holding, both);
            assert!(held["agents"].as_array().unwrap().is_empty());

            let qr = control.ack(json!({"StartQrPairing":{"daemon":"a"}})).await["qr"]
                .as_str()
                .unwrap()
                .to_owned();
            let qr =
                amux::parse_qr_pairing_payload_for_cloud(&qr, &format!("http://{relay}")).unwrap();
            assert_eq!(qr.host_id, a.host_id());
            c.pairing_admin()
                .await
                .pair_qr_cloud_peer(qr.host_id, qr.secret)
                .await
                .unwrap();
            c.can_call(&a).await;

            for invalid in [
                json!({"Connections":{"daemon":"missing"}}),
                json!({"SeverDirect":{"a":"a","b":"a"}}),
                json!({"EstablishDirect":{"a":"a","b":"b"}}),
                json!({"StartPinPairing":{"daemon":"b","ttl_secs":0}}),
                json!({"Latency":{"millis":1001}}),
                json!({"Inventory":{"daemon":"missing"}}),
            ] {
                assert!(control.request(invalid).await.get("Error").is_some());
            }
            control.ack(json!("Shutdown")).await;
        };
        let (result, ()) = tokio::join!(server, exercise);
        result.unwrap();
        assert!(TcpStream::connect(relay).await.is_err());
        assert!(TcpStream::connect(address).await.is_err());
        eprintln!("Network control cleanup verified: relay and control refuse connections");
    }

    #[tokio::test]
    async fn testnet_serve_starts_real_pairings_agents_and_user_credentials() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e-tests/topologies/two-hosts.json");
        let topology = Topology::load(&path).unwrap();
        let (net, ready, _agents) = start(&topology, "127.0.0.1:1".parse().unwrap())
            .await
            .unwrap();
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

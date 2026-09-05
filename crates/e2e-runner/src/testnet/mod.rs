//! A loopback process boundary around the same TestNet used by the Rust specs.

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
    AgentObserve {
        agent: String,
    },
    Connections {
        daemon: String,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Ack {
        pin: Option<String>,
        qr: Option<String>,
        observed: Vec<ObservedInput>,
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
                    let script: Script = serde_json::from_slice(
                        &std::fs::read(&*script)
                            .with_context(|| format!("read script {}", script.display()))?,
                    )
                    .context("parse Claude script")?;
                    topology.scripts.insert(agent.name.clone(), script);
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

struct ScriptedAgent {
    daemon: String,
    agent: amux::Agent,
    provider: Provider,
}

type Agents = HashMap<String, ScriptedAgent>;

async fn start(topology: &Topology, control: SocketAddr) -> Result<(TestNet, Readiness, Agents)> {
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
    let mut scripted = HashMap::new();
    for decl in &topology.agents {
        let (agent, provider) = net
            .daemon(&decl.daemon)
            .spawn_scripted_agent(
                &decl.name,
                &decl.working_dir,
                topology.scripts[&decl.name].clone(),
                None,
            )
            .await?;
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
            agents.retain(|_, agent| agent.daemon != name);
        }
        Control::Unpair { daemon, peer } => {
            let (daemon, peer) = pair(&daemon, &peer)?;
            daemon
                .admin_client()
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
                .admin_client()
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
        Control::AgentObserve { agent } => {
            if let Reply::Ack { observed, .. } = &mut reply {
                *observed = scripted(&agent)?.provider.observed();
            }
        }
        Control::AgentSpawnChild { agent, child } => {
            ensure!(
                valid_name(&child) && !agents.contains_key(&child),
                "invalid or duplicate child: {child}"
            );
            let parent = scripted(&agent)?;
            ensure!(
                parent.provider.error().is_none(),
                "parent provider has stopped"
            );
            let host = daemon(&parent.daemon)?;
            let (agent, provider) = host
                .spawn_scripted_agent(
                    &child,
                    &parent.agent.working_dir,
                    Script::default(),
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
                    provider,
                },
            );
        }
        Control::Connections { daemon: name } => {
            let dump = daemon(&name)?.debug_dump(false).await;
            let count = dump["links"]
                .as_array()
                .context("daemon diagnostics omitted links")?
                .len();
            if let Reply::Ack { connections, .. } = &mut reply {
                *connections = Some(count.try_into()?);
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
            let observer = b.admin_client().await;
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
                observer
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
            assert!(a.admin_client().await.get_peer(b.host_id()).await.is_err());
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

            let qr = control.ack(json!({"StartQrPairing":{"daemon":"a"}})).await["qr"]
                .as_str()
                .unwrap()
                .to_owned();
            let qr =
                amux::parse_qr_pairing_payload_for_cloud(&qr, &format!("http://{relay}")).unwrap();
            assert_eq!(qr.host_id, a.host_id());
            c.admin_client()
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
            ] {
                assert!(control.request(invalid).await.get("Error").is_some());
            }
            control.ack(json!("Shutdown")).await;
        };
        let (result, ()) = tokio::join!(server, exercise);
        result.unwrap();
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

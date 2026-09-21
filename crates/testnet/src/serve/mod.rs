//! The served control door: a loopback process boundary around the same
//! [`TestNet`] the in-process specs use.
//!
//! A driver outside this process (a phone journey, a Python script) starts
//! `testnet serve --topology <file>`, reads one readiness line from stdout and
//! then sends control requests, one JSON value per line, to the control
//! address. Every request is a [`Control`] verb, and every verb is a method
//! or an explicit composition of harness capabilities, so an in-process spec
//! and a phone journey use the same behavior vocabulary.

#[cfg(unix)]
mod codex_recording;
mod report_script;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use futures_util::FutureExt;
use node::discovery::Advertisement;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use uuid::Uuid;

#[cfg(test)]
use crate::Via;
use crate::script::{ObservedInput, Provider, Script, ScriptAsk, Step};
use crate::{Daemon, TestNet};

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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topology {
    #[serde(default = "default_cloud_url")]
    pub cloud_url: String,
    pub users: Vec<String>,
    /// What each account buys, where it is not the default. An account left
    /// out is on the paid tier, which is what every topology written before
    /// tiers existed assumed.
    #[serde(default)]
    pub tiers: HashMap<String, node::Tier>,
    pub daemons: Vec<DaemonDecl>,
    pub paired: Vec<(String, String, PairVia)>,
    pub agents: Vec<AgentDecl>,
    #[serde(skip)]
    scripts: HashMap<String, Script>,
    #[serde(skip)]
    sdk_scripts: HashMap<String, crate::sdk::Script>,
    #[serde(skip)]
    #[cfg(unix)]
    recordings: HashMap<String, codex_recording::Prepared>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonDecl {
    pub name: String,
    /// The cloud account this machine is signed in to, where it is signed in
    /// to one. A declaration with no user is a device nobody has signed in on
    /// — a phone before its first account — which still pairs with and
    /// reaches the machines on its own network.
    #[serde(default)]
    pub user: Option<String>,
    pub repository_roots: Vec<PathBuf>,
    /// Run this host as a profile behind the production installation front door.
    #[serde(default)]
    pub installation: bool,
    /// Whether this machine is on the network when the topology starts: an
    /// advertisement a browsing device resolves, as if it had just been
    /// switched on beside it.
    #[serde(default)]
    pub lan: bool,
    /// Provider transport for every SDK session this host creates, including
    /// requests that arrive later from a paired client.
    #[serde(default)]
    pub sdk_script: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairVia {
    /// Pair over a direct link. Named for the fact, not the carrier: direct
    /// pairing runs over QUIC now, and said "Tcp" only while it did not.
    Direct,
    Cloud,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDecl {
    pub name: String,
    pub daemon: String,
    pub working_dir: PathBuf,
    pub provider: ScriptedProvider,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ScriptedProvider {
    Claude { script: PathBuf },
    ClaudeSdk { model: String },
    Codex { recording: PathBuf },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Readiness {
    /// The cloud identity every daemon was configured with.
    pub cloud_url: String,
    /// Where the fake identity service answers. A client that signs in
    /// through it receives a token the relay below accepts; the phone
    /// journeys instead hand the app one of the static `users` tokens.
    pub identity: String,
    /// The relay carrying device traffic, addressed separately from the
    /// identity service that named it.
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
    /// Direct profile configuration for local client boundary tests.
    pub profile_config: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub name: String,
    pub daemon: String,
    pub agent_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    StopDaemon {
        name: String,
    },
    RestartSdkDaemon {
        name: String,
    },
    SuspendRestart {
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
    /// Puts a machine on this network, as an advertisement a device browsing
    /// would resolve. Nothing is trusted by it: what it offers a browser is a
    /// name, an identity claim and addresses to try.
    ///
    /// Published twice: to the daemons of this network, and over real mDNS on
    /// this Mac, where a simulator's own browser resolves it the way a phone's
    /// resolves a real machine. A device test that handed the app a decoded
    /// advertisement instead would never read the record the daemon writes.
    ///
    /// Only this verb reaches the Mac's network. A daemon declared `lan` is
    /// announced to the other daemons when the topology starts, but a phone
    /// finds it only once a test puts it there, so a story can begin with a
    /// phone that has found nothing.
    Announce {
        daemon: String,
    },
    /// Takes it off again, the way a machine going away says goodbye, on both
    /// of the networks it was announced on.
    Withdraw {
        daemon: String,
    },
    /// Changes what one cloud account buys, from the next token it is issued.
    /// Links already up keep the tier they were admitted on until they
    /// re-authenticate, which is what makes a flip observable rather than
    /// instantaneous.
    Tier {
        user: String,
        tier: node::Tier,
    },
    /// Eats or restores every direct UDP datagram involving a machine, which
    /// is the network a phone on a hotel connection is on.
    UdpBlocked {
        daemon: String,
        blocked: bool,
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
    DebugDump {
        daemon: String,
        verbose: bool,
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

/// The in-process capability reached by one served control verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlCapability {
    pub variant: &'static str,
    pub capability: &'static str,
}

/// Capability parity between the served door and the in-process harness.
///
/// This is deliberately data rather than a prose convention. Tests compare
/// it with Serde's complete variant list and with the rendered documentation.
pub const CONTROL_CAPABILITIES: &[ControlCapability] = &[
    ControlCapability {
        variant: "CloudOffline",
        capability: "`TestNet::cloud_offline`",
    },
    ControlCapability {
        variant: "CloudOnline",
        capability: "`TestNet::cloud_online`",
    },
    ControlCapability {
        variant: "SeverDirect",
        capability: "`TestNet::sever_direct`",
    },
    ControlCapability {
        variant: "EstablishDirect",
        capability: "`TestNet::try_establish_direct`",
    },
    ControlCapability {
        variant: "RestartDaemon",
        capability: "`TestNet::restart_daemon` + `Provider::close`",
    },
    ControlCapability {
        variant: "StopDaemon",
        capability: "`Daemon::stop`",
    },
    ControlCapability {
        variant: "RestartSdkDaemon",
        capability: "`TestNet::restart_daemon` + `Daemon::create_agent`",
    },
    ControlCapability {
        variant: "SuspendRestart",
        capability: "`Daemon::suspend_restart_agents` + `Provider::close`",
    },
    ControlCapability {
        variant: "Unpair",
        capability: "`Daemon::unpair`",
    },
    ControlCapability {
        variant: "StartPinPairing",
        capability: "`Daemon::start_pin_pairing`",
    },
    ControlCapability {
        variant: "StartQrPairing",
        capability: "`Daemon::try_start_qr_pairing`",
    },
    ControlCapability {
        variant: "Latency",
        capability: "`TestNet::relay_latency`",
    },
    ControlCapability {
        variant: "Announce",
        capability: "`TestNet::announce` + host mDNS publication",
    },
    ControlCapability {
        variant: "Withdraw",
        capability: "`TestNet::withdraw` + host mDNS withdrawal",
    },
    ControlCapability {
        variant: "Tier",
        capability: "`TestNet::cloud_user_tier`",
    },
    ControlCapability {
        variant: "UdpBlocked",
        capability: "`TestNet::udp_blocked`",
    },
    ControlCapability {
        variant: "AgentEmit",
        capability: "`script::Provider::emit`",
    },
    ControlCapability {
        variant: "AgentPlay",
        capability: "`script::Provider::play`",
    },
    ControlCapability {
        variant: "AgentRaiseAsk",
        capability: "`script::Provider::raise_ask`",
    },
    ControlCapability {
        variant: "AgentEndTurn",
        capability: "`script::Provider::end_turn`",
    },
    ControlCapability {
        variant: "AgentExit",
        capability: "`script::Provider::exit`",
    },
    ControlCapability {
        variant: "AgentSpawnChild",
        capability: "`Daemon::spawn_child`",
    },
    ControlCapability {
        variant: "AgentVerifyReplay",
        capability: "`Recorded::verify_replay`",
    },
    ControlCapability {
        variant: "AgentObserve",
        capability: "`script::Provider::observe` or `Daemon::observed_sdk_inputs`",
    },
    ControlCapability {
        variant: "DebugDump",
        capability: "`Daemon::debug_dump`",
    },
    ControlCapability {
        variant: "Connections",
        capability: "`Daemon::connections` or `TestNet::connections`",
    },
    ControlCapability {
        variant: "Inventory",
        capability: "`Daemon::inventory`",
    },
    ControlCapability {
        variant: "Shutdown",
        capability: "`TestNet::shutdown`",
    },
];

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
        diagnostics: Option<serde_json::Value>,
        /// What a browser on this network would resolve, for an `Announce`.
        ///
        /// A device that cannot browse this network itself is told what is on
        /// it from here: the name, the identity claim and the addresses the
        /// machine just put up. Absent for every other verb.
        ///
        /// Boxed because every other acknowledgement carries none of it, and
        /// an advertisement inline would widen every reply to its size.
        found: Option<Box<FoundHost>>,
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
            diagnostics: None,
            found: None,
        }
    }
}

/// One machine on this network, as an advertisement resolves it.
#[derive(Debug, Serialize, Deserialize)]
pub struct FoundHost {
    pub host: Uuid,
    pub name: String,
    pub version: u32,
    pub addrs: Vec<String>,
}

fn default_cloud_url() -> String {
    crate::default_cloud_url()
}

impl Topology {
    pub(crate) fn empty() -> Self {
        Self {
            cloud_url: default_cloud_url(),
            users: Vec::new(),
            tiers: HashMap::new(),
            daemons: Vec::new(),
            paired: Vec::new(),
            agents: Vec::new(),
            scripts: HashMap::new(),
            sdk_scripts: HashMap::new(),
            #[cfg(unix)]
            recordings: HashMap::new(),
        }
    }

    pub(crate) fn network_declaration(&self) -> Self {
        Self {
            cloud_url: self.cloud_url.clone(),
            users: self.users.clone(),
            tiers: self.tiers.clone(),
            daemons: self.daemons.clone(),
            paired: self.paired.clone(),
            agents: self.agents.clone(),
            scripts: HashMap::new(),
            sdk_scripts: HashMap::new(),
            #[cfg(unix)]
            recordings: HashMap::new(),
        }
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
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
        for user in topology.tiers.keys() {
            ensure!(users.contains(user), "unknown user in tiers: {user}");
        }
        let mut daemons = HashSet::new();
        for daemon in &mut topology.daemons {
            // Daemon names become directory components inside TestNet's temporary root.
            ensure!(
                valid_name(&daemon.name) && daemons.insert(daemon.name.clone()),
                "invalid or duplicate daemon: {}",
                daemon.name
            );
            if let Some(user) = &daemon.user {
                ensure!(users.contains(user), "unknown user: {user}");
            }
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
                    topology
                        .daemons
                        .iter()
                        .find(|d| d.name == name)
                        .unwrap()
                        .user
                        .as_deref()
                };
                // A machine nobody is signed in on has no cloud identity to
                // pair through, so two of them are not a pair through the
                // cloud however alike their absent accounts look.
                match (user(a), user(b)) {
                    (Some(x), Some(y)) if x == y => {}
                    _ => bail!("cloud pair crosses users: {a}, {b}"),
                }
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
    agent: node::Agent,
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

/// The advertisement each daemon announced so far has on this machine's own
/// network, by name. Dropping one withdraws it.
type Advertised = HashMap<String, Published>;

/// One advertisement registered with macOS's own mDNS responder, which
/// answers for it on every interface for as long as the registering process
/// lives.
///
/// The system responder rather than the daemon's own mDNS publisher, because a
/// simulator browses through the Mac's responder and does not report services
/// seen only on the loopback interface — which is the only interface a
/// publisher advertising a loopback address answers on. The system responder
/// advertises the loopback address on every interface. The record it carries
/// is built by `node::discovery::txt_properties`, the function the daemon's
/// own publisher uses.
struct Published {
    #[cfg(target_os = "macos")]
    _registration: tokio::process::Child,
}

impl Published {
    #[cfg(target_os = "macos")]
    fn register(advertisement: &Advertisement) -> Result<Self> {
        let addr = advertisement
            .addrs
            .first()
            .context("an advertisement needs a listener address")?;
        let mut command = tokio::process::Command::new("/usr/bin/dns-sd");
        command
            .arg("-P")
            .arg(&advertisement.name)
            .arg("_amux._udp")
            .arg("local")
            .arg(addr.port().to_string())
            .arg(format!("amux-{}.local", advertisement.host_id.simple()))
            .arg(addr.ip().to_string());
        for (key, value) in node::discovery::txt_properties(advertisement) {
            command.arg(format!("{key}={value}"));
        }
        let registration = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("cannot register with this Mac's mDNS responder")?;
        Ok(Self {
            _registration: registration,
        })
    }

    /// Only a Mac runs a simulator, so nowhere else has a browser to reach.
    #[cfg(not(target_os = "macos"))]
    fn register(_advertisement: &Advertisement) -> Result<Self> {
        Ok(Self {})
    }
}

/// Publishes one daemon's advertisement on this machine's network, replacing
/// whatever that daemon published before.
fn publish(advertised: &mut Advertised, name: String, advertisement: Advertisement) -> Result<()> {
    // The old registration goes first: the responder would otherwise rename
    // the new one to avoid a clash with a record that is about to disappear.
    advertised.remove(&name);
    advertised.insert(name, Published::register(&advertisement)?);
    Ok(())
}

async fn start(topology: &Topology, control: SocketAddr) -> Result<(TestNet, Readiness, Agents)> {
    let net = crate::TestNetBuilder::from_topology(topology).start().await;
    let daemon_names = topology
        .daemons
        .iter()
        .map(|daemon| {
            (
                daemon.name.clone(),
                if daemon.installation {
                    format!("{}/{}", daemon.name, daemon.name)
                } else {
                    daemon.name.clone()
                },
            )
        })
        .collect::<HashMap<_, _>>();
    // Before anything outside this process can ask for a token: what an
    // account buys has to be settled before the first device signs in with it.
    for (user, tier) in &topology.tiers {
        net.cloud_user_tier(user, *tier);
    }
    // A machine declared to be on this network is on it from the start, so a
    // device that browses before sending any control verb finds it there.
    for daemon in topology.daemons.iter().filter(|daemon| daemon.lan) {
        let _ = net.announce(&net.daemon(&daemon.name));
    }
    for (name, script) in &topology.sdk_scripts {
        net.daemon(&daemon_names[name])
            .script_sdk_sessions(script.clone())
            .await;
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
            let daemon = net.daemon(&daemon_names[&decl.name]);
            let (host_id, public_key) = daemon.identity_on_disk();
            DaemonIdentity {
                name: decl.name.clone(),
                host_id,
                fingerprint: format!("{:x}", Sha256::digest(public_key)),
                profile_config: daemon.profile_config_path(),
            }
        })
        .collect();
    let mut agents = Vec::new();
    let mut scripted = HashMap::new();
    for decl in &topology.agents {
        let daemon = net.daemon(&daemon_names[&decl.daemon]);
        let (agent, provider) = match &decl.provider {
            ScriptedProvider::ClaudeSdk { model } => {
                let agent = daemon
                    .admin_client()
                    .await
                    .create_agent(node::CreateAgentRequest {
                        agent_id: Uuid::new_v4(),
                        host_id: None,
                        name: Some(decl.name.clone()),
                        agent_type: node::AgentType::Claude {
                            driver: model::ClaudeDriver::Sdk,
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
        cloud_url: net.cloud_url().into(),
        identity: net
            .identity_url()
            .context("the served topology starts the identity fixture")?,
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
    names: &HashMap<String, String>,
    users: &HashSet<String>,
    agents: &mut Agents,
    advertised: &mut Advertised,
    control: Control,
) -> Result<Reply> {
    let daemon = |name: &str| -> Result<Daemon> {
        let runtime_name = names
            .get(name)
            .with_context(|| format!("unknown daemon: {name}"))?;
        Ok(net.daemon(runtime_name))
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
        Control::StopDaemon { name } => {
            daemon(&name)?.stop().await;
        }
        Control::RestartSdkDaemon { name } => {
            let host = daemon(&name)?;
            net.restart_daemon(&host).await;
            if host.is_installation_profile() {
                return Ok(reply);
            }
            let names = agents
                .iter()
                .filter(|(_, agent)| agent.daemon == name)
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            for agent_name in names {
                let prior = &agents[&agent_name];
                ensure!(
                    matches!(prior.provider, AgentProvider::ClaudeSdk),
                    "RestartSdkDaemon requires only SDK agents on the host"
                );
                let record = prior.agent.clone();
                let recreated = host
                    .admin_client()
                    .await
                    .create_agent(node::CreateAgentRequest {
                        agent_id: record.id,
                        host_id: None,
                        name: record.name,
                        agent_type: node::AgentType::Claude {
                            driver: model::ClaudeDriver::Sdk,
                        },
                        working_dir: record.working_dir,
                        terminal_size: None,
                        args: record.args,
                        parent: record.parent,
                        initial_prompt: None,
                    })
                    .await?;
                agents.get_mut(&agent_name).unwrap().agent = recreated;
            }
        }
        Control::SuspendRestart { name } => {
            let (resumed, failed) = daemon(&name)?.suspend_restart_agents().await?;
            if let Reply::Ack { diagnostics, .. } = &mut reply {
                *diagnostics = Some(serde_json::json!({
                    "resumed": resumed,
                    "failed": failed,
                }));
            }
            for agent in agents.values_mut().filter(|agent| agent.daemon == name) {
                agent.provider.close().await;
            }
            agents.retain(|_, agent| agent.daemon != name);
        }
        Control::Unpair { daemon, peer } => {
            let (daemon, peer) = pair(&daemon, &peer)?;
            daemon.unpair(&peer).await;
        }
        Control::StartPinPairing {
            daemon: name,
            ttl_secs,
        } => {
            ensure!(ttl_secs <= 3600, "pairing TTL must not exceed one hour");
            let pin = daemon(&name)?
                .start_pin_pairing(Duration::from_secs(ttl_secs))
                .await?;
            if let Reply::Ack { pin: output, .. } = &mut reply {
                *output = Some(pin.to_string());
            }
        }
        Control::StartQrPairing { daemon: name } => {
            let payload = daemon(&name)?.try_start_qr_pairing().await?;
            if let Reply::Ack { qr, .. } = &mut reply {
                *qr = Some(payload.encoded());
            }
        }
        Control::Latency { millis } => {
            ensure!(millis <= 1000, "relay latency must not exceed 1000 ms");
            net.relay_latency(millis);
        }
        Control::Announce { daemon: name } => {
            let advertisement = net.announce(&daemon(&name)?);
            publish(advertised, name, advertisement.clone())?;
            if let Reply::Ack { found, .. } = &mut reply {
                *found = Some(Box::new(FoundHost {
                    host: advertisement.host_id,
                    name: advertisement.name,
                    version: advertisement.version,
                    addrs: advertisement
                        .addrs
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                }));
            }
        }
        Control::Withdraw { daemon: name } => {
            net.withdraw(&daemon(&name)?);
            advertised.remove(&name);
        }
        Control::Tier { user, tier } => {
            // An account the topology never declared would otherwise be
            // invented here and bought a tier no device ever asks about, so a
            // misspelled label would look like it worked.
            ensure!(users.contains(&user), "unknown user: {user}");
            net.cloud_user_tier(&user, tier);
        }
        Control::UdpBlocked {
            daemon: name,
            blocked,
        } => net.udp_blocked(&daemon(&name)?, blocked),
        Control::AgentEmit { agent, rows } => {
            scripted(&agent)?.provider.claude()?.emit(rows).await?;
        }
        Control::AgentPlay { agent, steps } => {
            ensure!(!steps.is_empty(), "a play must carry at least one step");
            scripted(&agent)?.provider.play(steps).await?;
        }
        Control::AgentRaiseAsk { agent, ask } => {
            scripted(&agent)?.provider.claude()?.raise_ask(ask).await?;
        }
        Control::AgentEndTurn { agent } => {
            scripted(&agent)?.provider.claude()?.end_turn().await?;
        }
        Control::AgentExit { agent, code } => {
            ensure!(code >= 0, "exit code must be nonnegative");
            scripted(&agent)?.provider.claude()?.exit(code).await?;
        }
        Control::AgentVerifyReplay { agent } => match &scripted(&agent)?.provider {
            #[cfg(unix)]
            AgentProvider::Codex(recorded) => {
                recorded.verify_replay()?;
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
                    *observed = provider.observe();
                } else {
                    let id = scripted
                        .map(|agent| agent.agent.id)
                        .or_else(|| agent.parse().ok())
                        .context("SDK observation needs an agent name or UUID")?;
                    let mut found = None;
                    for name in names.keys() {
                        if let Some(inputs) = daemon(name)?.observed_sdk_inputs(id).await {
                            ensure!(found.is_none(), "SDK identity exists on more than one host");
                            found = Some(inputs);
                        }
                    }
                    *sdk_inputs = found.context("no scripted SDK session has this identity")?;
                }
            }
        }
        Control::DebugDump {
            daemon: name,
            verbose,
        } => {
            if let Reply::Ack { diagnostics, .. } = &mut reply {
                *diagnostics = Some(daemon(&name)?.debug_dump(verbose).await);
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
                .spawn_child(&parent.agent, &child, script.clone())
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
            ensure!(
                name.is_some() ^ label.is_some(),
                "Connections requires exactly one of daemon or user"
            );
            let mut counted = 0usize;
            let mut per_host = Vec::new();
            if let Some(name) = name {
                counted += daemon(&name)?.connections().await;
            }
            if let Some(label) = label {
                for (host, links) in net.connections(&label).await {
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
            let (running, trusted) = daemon(&name)?.inventory().await?;
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
                            node::AgentKind::Claude { .. } => "claude",
                            node::AgentKind::Codex => "codex",
                            node::AgentKind::TestAgent => "test-agent",
                        }
                        .to_string(),
                        driver: match agent.kind {
                            node::AgentKind::Claude {
                                driver: model::ClaudeDriver::Pty,
                            } => Some("pty".to_string()),
                            node::AgentKind::Claude {
                                driver: model::ClaudeDriver::Sdk,
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
        topology
            .daemons
            .into_iter()
            .map(|daemon| {
                let runtime = if daemon.installation {
                    format!("{}/{}", daemon.name, daemon.name)
                } else {
                    daemon.name.clone()
                };
                (daemon.name, runtime)
            })
            .collect(),
        topology.users.into_iter().collect(),
        agents,
    )
    .await
}

async fn serve_net(
    net: TestNet,
    listener: TcpListener,
    names: HashMap<String, String>,
    users: HashSet<String>,
    mut agents: Agents,
) -> Result<()> {
    let mut advertised = Advertised::new();
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
                    let operation = AssertUnwindSafe(apply(&net, &names, &users, &mut agents, &mut advertised, control)).catch_unwind();
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
    drop(advertised);
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
            // A served network's daemons are ordinary runtimes with ordinary
            // tracing, and a driver outside this process has no other way to
            // watch them decide. Silent unless RUST_LOG asks, so an ordinary
            // run stays quiet and a diagnosis costs one environment variable.
            if std::env::var_os("RUST_LOG").is_some() {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                    .with_writer(std::io::stderr)
                    .try_init();
            }
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
            self.request_line(&serde_json::to_string(&control).unwrap())
                .await
        }

        async fn request_line(&mut self, request: &str) -> serde_json::Value {
            self.0
                .get_mut()
                .write_all(request.as_bytes())
                .await
                .unwrap();
            self.0.get_mut().write_all(b"\n").await.unwrap();
            self.read_reply().await
        }

        async fn read_reply(&mut self) -> serde_json::Value {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(35), self.0.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let reply = serde_json::from_str(&line).unwrap();
            eprintln!("control reply => {reply}");
            reply
        }

        pub(super) async fn ack(&mut self, control: serde_json::Value) -> serde_json::Value {
            let reply = self.request(control).await;
            assert!(reply.get("Ack").is_some(), "{reply}");
            reply["Ack"].clone()
        }
    }

    thread_local! {
        static DESERIALIZED_CONTROL_VARIANTS: std::cell::RefCell<Vec<&'static str>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    struct CollectControlVariants;

    impl<'de> serde::Deserializer<'de> for CollectControlVariants {
        type Error = serde::de::value::Error;

        fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            Err(serde::de::Error::custom("variant list collected"))
        }

        fn deserialize_enum<V>(
            self,
            _name: &'static str,
            variants: &'static [&'static str],
            _visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            DESERIALIZED_CONTROL_VARIANTS.with(|collected| {
                collected.borrow_mut().extend_from_slice(variants);
            });
            Err(serde::de::Error::custom("variant list collected"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct
            map struct identifier ignored_any
        }
    }

    fn rendered_capability_table() -> String {
        let mut rendered =
            String::from("| Door verb (`serve::Control`) | Harness capability |\n| --- | --- |\n");
        for entry in CONTROL_CAPABILITIES {
            rendered.push_str(&format!("| `{}` | {} |\n", entry.variant, entry.capability));
        }
        rendered
    }

    fn marked_table(source: &str, line_prefix: &str) -> String {
        let mut inside = false;
        let mut rendered = String::new();
        for line in source.lines() {
            if line.contains("control-capabilities:start") {
                inside = true;
                continue;
            }
            if line.contains("control-capabilities:end") {
                break;
            }
            if inside {
                rendered.push_str(line.strip_prefix(line_prefix).unwrap_or(line));
                rendered.push('\n');
            }
        }
        assert!(inside, "control capability table markers are missing");
        rendered
    }

    #[test]
    fn testnet_control_capabilities_cover_the_enum_and_documentation() {
        DESERIALIZED_CONTROL_VARIANTS.with(|collected| collected.borrow_mut().clear());
        let _ = Control::deserialize(CollectControlVariants);
        let variants = DESERIALIZED_CONTROL_VARIANTS.with(|collected| collected.borrow().clone());
        let table = CONTROL_CAPABILITIES
            .iter()
            .map(|entry| entry.variant)
            .collect::<Vec<_>>();
        assert_eq!(table.len(), table.iter().collect::<HashSet<_>>().len());
        assert_eq!(table, variants, "capability table must follow the enum");

        let rendered = rendered_capability_table();
        assert_eq!(
            marked_table(include_str!("../lib.rs"), "//! "),
            rendered,
            "crate documentation must be rendered from CONTROL_CAPABILITIES"
        );
        assert_eq!(
            marked_table(include_str!("../../../../docs/TESTNET.md"), ""),
            rendered,
            "TESTNET documentation must be rendered from CONTROL_CAPABILITIES"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn testnet_control_protocol_conforms_across_errors_ordering_and_shutdown() {
        use serde_json::json;

        let net = TestNet::builder().cloud().daemon("a").start().await;
        let relay = net.relay_addr();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let control_address = listener.local_addr().unwrap();
        let server = serve_net(
            net,
            listener,
            [("a".to_owned(), "a".to_owned())].into(),
            ["default".to_owned()].into(),
            HashMap::new(),
        );
        let exercise = async {
            let mut control = ControlClient::connect(control_address).await;

            let malformed = control
                .request_line(r#"{"Latency":{"millis":"soon"}}"#)
                .await;
            assert!(malformed.get("Error").is_some(), "{malformed}");
            let extra = control
                .request_line(r#"{"Latency":{"millis":0,"extra":true}}"#)
                .await;
            assert!(extra.get("Error").is_some(), "{extra}");
            for invalid in [
                json!({"Connections":{}}),
                json!({"Connections":{"daemon":"a","user":"default"}}),
            ] {
                let reply = control.request(invalid).await;
                assert_eq!(
                    reply["Error"]["message"],
                    "Connections requires exactly one of daemon or user"
                );
            }

            let unknown = control
                .request(json!({"Inventory":{"daemon":"missing"}}))
                .await;
            assert_eq!(unknown["Error"]["message"], "unknown daemon: missing");

            control
                .ack(json!({"StartPinPairing":{"daemon":"a","ttl_secs":30}}))
                .await;
            let failed_operation = control
                .request(json!({"StartPinPairing":{"daemon":"a","ttl_secs":30}}))
                .await;
            assert!(
                failed_operation.get("Error").is_some(),
                "{failed_operation}"
            );
            assert!(
                control.ack(json!({"Connections":{"daemon":"a"}})).await["connections"].is_number(),
                "an operation error must not close the control connection"
            );

            control
                .0
                .get_mut()
                .write_all(b"\"CloudOffline\"\n\"CloudOnline\"\n")
                .await
                .unwrap();
            for expected in ["CloudOffline", "CloudOnline"] {
                let reply = control.read_reply().await;
                assert!(reply.get("Ack").is_some(), "{expected}: {reply}");
            }
            TcpStream::connect(relay)
                .await
                .expect("the second queued request runs after the first and restores the relay");

            control.ack(json!("Shutdown")).await;
        };
        let (result, ()) = tokio::join!(server, exercise);
        result.unwrap();
        assert!(
            TcpStream::connect(relay).await.is_err(),
            "shutdown releases the relay listener"
        );
        assert!(
            TcpStream::connect(control_address).await.is_err(),
            "shutdown releases the control listener"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn testnet_control_every_network_verb_is_observed_by_another_client() {
        use serde_json::json;
        let net = TestNet::builder()
            .cloud_url("https://cloud.testnet.example")
            .daemon("a")
            .daemon("b")
            .daemon("c")
            .paired("a", "b", Via::Direct)
            .start()
            .await;
        let [a, b, c] = net.daemons(["a", "b", "c"]);
        let identity = a.identity_on_disk();
        let relay = net.relay_addr();
        let cloud_url = net.cloud_url().to_owned();
        assert_ne!(cloud_url, format!("http://{relay}"));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = serve_net(
            net,
            listener,
            ["a", "b", "c"]
                .map(|name| (name.to_owned(), name.to_owned()))
                .into(),
            ["default"].map(String::from).into(),
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
            // What a verb does to links is eventual: its acknowledgement says
            // the daemon was told, and the link it takes or restores is seen a
            // moment later. So the count is waited for rather than read once,
            // which on a loaded machine reads the moment before the change.
            // The claim is unchanged — a count that settles anywhere else,
            // including one link too many, still fails.
            async fn settles(
                client: &mut ControlClient,
                name: &str,
                want: impl Fn(u64) -> bool,
                claim: &str,
            ) {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let seen = count(client, name).await;
                    if want(seen) {
                        return;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "{claim}: '{name}' holds {seen} connections"
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            let linked = count(&mut second, "b").await;
            assert_eq!(
                linked, 2,
                "a directly paired daemon holds one peer link and one relay link"
            );
            control.ack(json!("CloudOffline")).await;
            settles(
                &mut second,
                "b",
                |seen| seen == 1,
                "going offline takes the relay link and leaves one direct link",
            )
            .await;
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
            settles(
                &mut second,
                "b",
                |seen| seen == linked,
                "coming back restores the relay link",
            )
            .await;
            // A repeated online command must not create a second relay connection.
            control.ack(json!("CloudOnline")).await;
            settles(
                &mut second,
                "b",
                |seen| seen == linked,
                "coming back twice is still one relay link",
            )
            .await;
            control.ack(json!({"SeverDirect":{"a":"a","b":"b"}})).await;
            settles(
                &mut second,
                "b",
                |seen| seen == 1,
                "severing the direct path leaves only the relay link",
            )
            .await;
            b.can_call(&a).await;
            b.connects_to(&a).via_cloud().await;
            b.uses_quic_relay().await;

            control.ack(json!({"Latency":{"millis":100}})).await;
            let start = tokio::time::Instant::now();
            let delayed_stream = b.open_event_stream_to(&a).await;
            let elapsed = start.elapsed();
            assert!(
                elapsed >= Duration::from_millis(100),
                "real routed call must traverse delayed relay bytes; completed in {elapsed:?}"
            );
            eprintln!("routed call with 100 ms relay latency: {elapsed:?}");
            drop(delayed_stream);
            control.ack(json!({"Latency":{"millis":0}})).await;
            assert!(b.lists_agents_on(&a).await.is_ok());
            control
                .ack(json!({"EstablishDirect":{"a":"a","b":"b"}}))
                .await;
            settles(
                &mut second,
                "b",
                |seen| seen == 2,
                "establishing the direct path restores exactly one direct link beside the relay",
            )
            .await;
            let stream = b.open_event_stream_to(&a).await;
            control.ack(json!({"StopDaemon":{"name":"a"}})).await;
            assert!(
                control
                    .request(json!({"DebugDump":{"daemon":"a","verbose":false}}))
                    .await
                    .get("Error")
                    .is_some()
            );
            control.ack(json!({"RestartDaemon":{"name":"a"}})).await;
            stream.expect_disconnect().await;
            assert_eq!(a.identity_on_disk(), identity);
            settles(
                &mut second,
                "a",
                |seen| seen == linked,
                "a restarted daemon comes back with the same links",
            )
            .await;
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
            c.pair(&b).with_cloud_pin(&pin).await.unwrap();
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
            // Validate the host-produced invitation against this topology
            // before passing its secret to the accepting device.
            let qr = node::parse_qr_pairing_payload(&qr).unwrap();
            assert_eq!(qr.cloud_url.as_deref(), Some(cloud_url.as_str()));
            assert_eq!(qr.host_id, a.host_id());
            let admin = c.pairing_admin().await;
            let pending = admin.begin_pair_qr(&qr).await.unwrap();
            admin.confirm_pair(pending).await.unwrap();
            c.can_call(&a).await;

            // A machine can be put on this network and taken off it again,
            // and the datagrams a direct link runs on can be eaten. What each
            // does is the harness's own claim, proved in the spec suite; what
            // is proved here is that the door names them and reaches them.
            control.ack(json!({"Announce":{"daemon":"c"}})).await;
            control.ack(json!({"Withdraw":{"daemon":"c"}})).await;
            control
                .ack(json!({"UdpBlocked":{"daemon":"c","blocked":true}}))
                .await;
            control
                .ack(json!({"UdpBlocked":{"daemon":"c","blocked":false}}))
                .await;
            control
                .ack(json!({"Tier":{"user":"default","tier":"free"}}))
                .await;

            for invalid in [
                json!({"Connections":{"daemon":"missing"}}),
                json!({"Announce":{"daemon":"missing"}}),
                json!({"Withdraw":{"daemon":"missing"}}),
                json!({"UdpBlocked":{"daemon":"missing","blocked":true}}),
                json!({"Tier":{"user":"missing","tier":"pro"}}),
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

    fn topology(name: &str) -> Topology {
        Topology::load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../journeys/topologies/{name}")),
        )
        .unwrap()
    }

    /// A phone's first network: one machine on it, nobody signed in anywhere,
    /// and the machine already announcing itself when the topology comes up.
    #[tokio::test]
    async fn testnet_serve_starts_a_network_with_nobody_signed_in() {
        let topology = topology("onramp.json");
        assert!(topology.users.is_empty());
        let (net, ready, _agents) = start(&topology, "127.0.0.1:1".parse().unwrap())
            .await
            .unwrap();
        assert!(ready.users.is_empty(), "nobody is signed in here");
        assert_eq!(
            net.daemon("workstation").connections().await,
            0,
            "a machine nobody signed in on has no relay link"
        );
        let workstation = net.daemon("workstation");
        assert!(
            net.discovery_events().iter().any(|event| matches!(
                event,
                node::discovery::DiscoveryEvent::Found(advert)
                    if advert.host_id == workstation.host_id()
            )),
            "a machine declared to be on this network is on it from the start"
        );
    }

    /// An account that has not paid for the relay. Its machine's own link is
    /// admitted on that tier rather than on the default.
    #[tokio::test]
    async fn testnet_serve_starts_an_account_on_the_tier_its_topology_declares() {
        let topology = topology("free-tier.json");
        let (net, _ready, _agents) = start(&topology, "127.0.0.1:1".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            net.daemon("workstation").refresh_entitlement().await,
            node::Tier::Free
        );
    }

    #[tokio::test]
    async fn testnet_serve_starts_real_pairings_agents_and_user_credentials() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../journeys/topologies/two-hosts.json");
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
        for bad in [
            // A tier is what one named account buys; naming no such account
            // is a topology that means nothing.
            serde_json::json!({
                "users": ["u"],
                "tiers": {"other": "free"},
                "daemons": [],
                "paired": [],
                "agents": [],
            }),
            // Two machines nobody is signed in on have no shared account to
            // pair through, so this is not a pairing through the cloud.
            serde_json::json!({
                "users": [],
                "daemons": [
                    {"name":"a","repository_roots":[]},
                    {"name":"b","repository_roots":[]},
                ],
                "paired": [["a", "b", "Cloud"]],
                "agents": [],
            }),
        ] {
            std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(Topology::load(&path).is_err());
        }
    }
}

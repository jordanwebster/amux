//! A running topology: one production daemon per host, in this process,
//! each on its own temporary installation, with real agent processes on
//! the fake providers.
//!
//! Verbs change the world and acknowledge that the change is installed;
//! observations wait for consequences and return what they saw. The net
//! distinguishes the three ways a daemon comes back: [`Net::stop_daemon`]
//! then [`Net::restart_daemon`] is a clean restart; [`Net::kill_daemon`]
//! then [`Net::restart_daemon`] is a crash with the agents still running;
//! [`Net::rewind_host`] is power loss, the store and journals back to what
//! reached the drive and the machine up under a new boot id.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agent_dir::Clock;
use node::harness::{HostVia, ScriptedDiscovery, scripted_discovery};
use node::{
    ClientApi, Daemon, Edge, EdgeOptions, LanOptions, Launch, LoopbackLink, ProfileId,
    ProfileRuntime,
};
use provider_fakes::script::{SCRIPT_ENV, Script, Step};
use serde::{Deserialize, Serialize};
use store::AgentKey;
use tokio::sync::mpsc;
use uuid::Uuid;
use wire::client_service_server::ClientService as _;
use wire::profile_service_client::ProfileServiceClient;
use wire::{
    AgentParent, ClaudeCreateConfig, ClaudePtyInput, ClaudeSdkInput, CodexCreateConfig, CodexInput,
    CreateAgentRequest, DeleteAgentRequest, DeleteAgentResponse, Input, PromptInput, StopMode,
    WithdrawQueued, claude_pty_input, claude_sdk_input, codex_input, create_agent_request, input,
};

use crate::binaries::Binaries;
use crate::clock::DrivenClock;
use crate::invariant::{self, BlockViolation};
use crate::observe::{self, InventoryObserver, Observer, ObserverOf, PATIENCE, Stuck};
use crate::relay::{Relay, UdpGate};
use crate::topology::{AgentDecl, FakeKind, HostDecl, Topology, TopologyError, link_key};

/// Lets a caller adjust a host's edge beyond what the topology declares,
/// such as pointing its cloud link at a stand-in relay.
pub type EdgeHook = Arc<dyn Fn(&str, &mut EdgeOptions) + Send + Sync>;

/// Lets a caller change a host's parameters, such as the tail size K or a
/// retention budget, on everything the host starts with.
pub type LaunchHook = Arc<dyn Fn(&str, &mut Launch) + Send + Sync>;

/// Which clock the daemons' policy timers run on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClockMode {
    /// A [`DrivenClock`] the specification advances.
    #[default]
    Driven,
    /// Wall time, for a net served to a person or a journey.
    Wall,
}

#[derive(Clone, Default)]
pub struct NetOptions {
    pub clock: ClockMode,
    /// The driven clock to use, when something outside the net, such as a
    /// stand-in relay, must share it; a fresh one otherwise.
    pub driven: Option<DrivenClock>,
    pub edge: Option<EdgeHook>,
    pub launch: Option<LaunchHook>,
}

/// A verb's acknowledgement: what was installed, at what policy time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub installed: String,
    pub at_ms: i64,
}

/// Where a host's journal is cut when it loses power.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalCut {
    pub agent: String,
    /// The journal's global byte offset the drive kept up to.
    pub byte: u64,
}

/// An agent the net knows by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub name: String,
    pub host: String,
    pub host_id: Uuid,
    pub id: Uuid,
    pub kind: FakeKind,
}

impl AgentRef {
    pub fn key(&self) -> AgentKey {
        AgentKey::new(
            self.host_id.as_bytes().to_vec(),
            self.id.as_bytes().to_vec(),
        )
    }

    pub fn parent(&self) -> AgentParent {
        AgentParent {
            host_id: self.host_id.as_bytes().to_vec(),
            agent_id: self.id.as_bytes().to_vec(),
        }
    }
}

/// Everything a host is on disk and in process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostInfo {
    pub name: String,
    pub host_id: Uuid,
    pub profile: ProfileId,
    pub data_dir: PathBuf,
    /// The installation front door a client dials.
    pub front_door: PathBuf,
    /// An installation config naming the front door, for `amux --config`.
    pub config: PathBuf,
    /// Where this host's agents work.
    pub work: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error("no host {0:?}")]
    NoHost(String),
    #[error("no agent {0:?}")]
    NoAgent(String),
    #[error("agent {0:?} exists")]
    AgentExists(String),
    #[error("hosts {0:?} and {1:?} are not linked in this topology")]
    NoLink(String, String),
    #[error("host {0:?} is down")]
    Down(String),
    #[error("host {0:?} is running")]
    Running(String),
    #[error("host {0:?} has no checkpoint to lose power back to; take one first")]
    NoCheckpoint(String),
    #[error("the net runs on wall time; only a driven net advances its clock")]
    WallClock,
    #[error("host {host:?} failed to start: {error}")]
    Start { host: String, error: String },
    #[error("host {host:?}: {error}")]
    Host { host: String, error: String },
    #[error(transparent)]
    Registry(#[from] node::RegistryError),
    #[error(transparent)]
    Serve(#[from] node::ServeError),
    #[error("refused: {}", .0.message())]
    Refused(Box<tonic::Status>),
    #[error(transparent)]
    Stuck(#[from] Stuck),
    #[error(transparent)]
    Block(#[from] BlockViolation),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is not supported on this platform")]
    Unsupported(&'static str),
    #[error("the topology declares no relay")]
    NoRelay,
}

struct Host {
    decl: HostDecl,
    info: HostInfo,
    dir: PathBuf,
    /// Behind a lock only so the net can be shared across tasks: a daemon
    /// holding its supervisor pipe is not Sync.
    daemon: Option<std::sync::Mutex<Daemon>>,
    /// The running daemon's profile runtime, held weakly so a killed
    /// daemon's run can be seen to end.
    runtime: Weak<ProfileRuntime>,
    boot: u64,
    /// The store file as of the last checkpoint: what reached the drive.
    checkpoint: Option<Vec<u8>>,
}

struct Link {
    /// Cut by a fault verb, as opposed to down because an end is.
    severed: bool,
    /// Behind a lock only so the net can be shared across tasks: a
    /// loopback carrier is not Sync.
    live: Option<std::sync::Mutex<LoopbackLink>>,
}

pub struct Net {
    root: tempfile::TempDir,
    topology: Topology,
    clock: Arc<dyn Clock>,
    driven: Option<DrivenClock>,
    binaries: Binaries,
    bus: ScriptedDiscovery,
    edge_hook: Option<EdgeHook>,
    launch_hook: Option<LaunchHook>,
    hosts: BTreeMap<String, Host>,
    links: BTreeMap<(String, String), Link>,
    agents: BTreeMap<String, AgentRef>,
    relay: Option<Relay>,
    /// Each host's way to the relay's QUIC carrier, kept across restarts.
    udp: BTreeMap<String, UdpGate>,
}

impl Net {
    /// Starts `topology` on a driven clock.
    pub async fn start(topology: Topology) -> Result<Self, NetError> {
        Self::start_with(topology, NetOptions::default()).await
    }

    /// Starts every host, trusts and links the declared pairs, waits for
    /// each link to carry traffic both ways, and spawns the declared agents
    /// in order, each returning once its process has said hello. When this
    /// returns the net is ready.
    pub async fn start_with(topology: Topology, options: NetOptions) -> Result<Self, NetError> {
        topology.validate()?;
        let driven = match options.clock {
            ClockMode::Driven => Some(options.driven.unwrap_or_default()),
            ClockMode::Wall => None,
        };
        let clock: Arc<dyn Clock> = match &driven {
            Some(driven) => Arc::new(driven.clone()),
            None => Arc::new(agent_dir::SystemClock),
        };
        let root = tempfile::Builder::new().prefix("testnet").tempdir()?;
        std::fs::create_dir_all(root.path().join("gates"))?;
        std::fs::create_dir_all(root.path().join("scripts"))?;
        let mut net = Self {
            root,
            topology: topology.clone(),
            clock,
            driven,
            binaries: Binaries::built().clone(),
            bus: ScriptedDiscovery::new(),
            edge_hook: options.edge,
            launch_hook: options.launch,
            hosts: BTreeMap::new(),
            links: BTreeMap::new(),
            agents: BTreeMap::new(),
            relay: None,
            udp: BTreeMap::new(),
        };
        if let Some(relay) = &topology.relay {
            net.relay = Some(Relay::start(relay, net.clock.clone()).await?);
        }
        for decl in &topology.hosts {
            net.create_host(decl)?;
            net.start_host(&decl.name).await?;
        }
        for decl in &topology.hosts {
            if let Some(account) = &decl.account {
                net.sign_in(&decl.name, account).await?;
            }
        }
        for link in &topology.links {
            let a = net.edge(&link.a)?;
            let b = net.edge(&link.b)?;
            let trust = |error: tonic::Status| NetError::Host {
                host: link.a.clone(),
                error: error.message().to_owned(),
            };
            a.trust(&b).await.map_err(trust)?;
            b.trust(&a).await.map_err(trust)?;
            net.links.insert(
                link_key(&link.a, &link.b),
                Link {
                    severed: false,
                    live: None,
                },
            );
        }
        net.link_all()?;
        for link in &topology.links {
            net.wait_link(&link.a, &link.b, true).await?;
        }
        for agent in &topology.agents {
            net.spawn(agent.clone()).await?;
        }
        Ok(net)
    }

    // --- resources ---------------------------------------------------------

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// The directory everything in the net lives under.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The directory a script's relative wait paths resolve into.
    pub fn gates(&self) -> PathBuf {
        self.root.path().join("gates")
    }

    /// Creates the gate file `name`, releasing every step waiting on it.
    pub fn open_gate(&self, name: &str) -> Result<Ack, NetError> {
        std::fs::write(self.gates().join(name), b"")?;
        Ok(self.ack(format!("gate {name} open")))
    }

    /// The relay the topology declares.
    pub fn relay(&self) -> Result<&Relay, NetError> {
        self.relay.as_ref().ok_or(NetError::NoRelay)
    }

    /// The scripted local network the hosts with discovery advertise and
    /// browse on, for announcing what no host would.
    pub fn discovery(&self) -> &ScriptedDiscovery {
        &self.bus
    }

    pub fn binaries(&self) -> &Binaries {
        &self.binaries
    }

    /// The policy clock, when the net is driven.
    pub fn clock(&self) -> Option<&DrivenClock> {
        self.driven.as_ref()
    }

    /// Moves policy time on every host.
    pub fn advance(&self, by: Duration) -> Result<Ack, NetError> {
        let driven = self.driven.as_ref().ok_or(NetError::WallClock)?;
        driven.advance(by);
        Ok(self.ack(format!("policy time +{}ms", by.as_millis())))
    }

    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    pub fn host_names(&self) -> Vec<String> {
        self.hosts.keys().cloned().collect()
    }

    pub fn host(&self, name: &str) -> Result<&HostInfo, NetError> {
        self.hosts
            .get(name)
            .map(|host| &host.info)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))
    }

    pub fn is_up(&self, name: &str) -> bool {
        self.hosts
            .get(name)
            .is_some_and(|host| host.daemon.is_some())
    }

    /// The host's profile runtime. Hold it only as long as a call needs:
    /// a held runtime outlives a killed daemon.
    pub fn runtime(&self, name: &str) -> Result<Arc<ProfileRuntime>, NetError> {
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        if host.daemon.is_none() {
            return Err(NetError::Down(name.to_owned()));
        }
        host.runtime
            .upgrade()
            .ok_or_else(|| NetError::Down(name.to_owned()))
    }

    /// The host's generation as of its daemon's start.
    pub fn generation(&self, name: &str) -> Result<u64, NetError> {
        Ok(self.runtime(name)?.generation())
    }

    pub fn edge(&self, name: &str) -> Result<Arc<Edge>, NetError> {
        self.runtime(name)?.edge().ok_or_else(|| NetError::Host {
            host: name.to_owned(),
            error: "its edge is not running".to_owned(),
        })
    }

    /// Another profile the host's installation serves, such as one
    /// [`Net::create_profile`] made.
    pub fn profile_runtime(
        &self,
        name: &str,
        profile: ProfileId,
    ) -> Result<Arc<ProfileRuntime>, NetError> {
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        let daemon = host
            .daemon
            .as_ref()
            .ok_or_else(|| NetError::Down(name.to_owned()))?;
        daemon
            .lock()
            .unwrap()
            .profile(profile)
            .ok_or_else(|| NetError::Host {
                host: name.to_owned(),
                error: format!("no profile {profile}"),
            })
    }

    pub fn profile_edge(&self, name: &str, profile: ProfileId) -> Result<Arc<Edge>, NetError> {
        self.profile_runtime(name, profile)?
            .edge()
            .ok_or_else(|| NetError::Host {
                host: name.to_owned(),
                error: format!("profile {profile}'s edge is not running"),
            })
    }

    /// The installation's front door on `name`: profiles, pairing, peers
    /// and accounts, as a person's client calls them.
    pub async fn front_door(
        &self,
        name: &str,
    ) -> Result<ProfileServiceClient<tonic::transport::Channel>, NetError> {
        let path = self.host(name)?.front_door.clone();
        let channel = tonic::transport::Endpoint::from_static("http://amux.test")
            .connect_with_connector(tower::service_fn(move |_| {
                let path = path.clone();
                async move {
                    agent_dir::local_socket::connect(&path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .map_err(|error| NetError::Host {
                host: name.to_owned(),
                error: error.to_string(),
            })?;
        Ok(ProfileServiceClient::new(channel))
    }

    pub fn agent(&self, name: &str) -> Result<&AgentRef, NetError> {
        self.agents
            .get(name)
            .ok_or_else(|| NetError::NoAgent(name.to_owned()))
    }

    pub fn agents(&self) -> impl Iterator<Item = &AgentRef> {
        self.agents.values()
    }

    /// The agent's directory on its host.
    pub fn agent_dir(&self, name: &str) -> Result<PathBuf, NetError> {
        let agent = self.agent(name)?;
        let host = self.host(&agent.host)?;
        Ok(node::profile_dir(&host.data_dir, host.profile)
            .join(node::AGENTS)
            .join(agent.id.to_string()))
    }

    /// Where the agent's journal ends now.
    pub fn journal_end(&self, name: &str) -> Result<u64, NetError> {
        Ok(journal::end(
            &self.agent_dir(name)?.join(agent_dir::JOURNAL),
        )?)
    }

    fn ack(&self, installed: String) -> Ack {
        Ack {
            installed,
            at_ms: self.clock.now_ms(),
        }
    }

    // --- agents ------------------------------------------------------------

    /// Spawns an agent on its host: a real `amux agent` process whose
    /// provider is the fake of its kind playing its script. Relative wait
    /// paths in the script name files in [`Net::gates`].
    pub async fn spawn(&mut self, decl: AgentDecl) -> Result<wire::Agent, NetError> {
        if self.agents.contains_key(&decl.name) {
            return Err(NetError::AgentExists(decl.name));
        }
        let host = self.host(&decl.host)?.clone();
        let parent = match &decl.parent {
            Some(parent) => Some(self.agent(parent)?.parent()),
            None => None,
        };
        let script = self.write_script(&decl.name, decl.script.clone().unwrap_or_default())?;
        let id = Uuid::new_v4();
        let request = CreateAgentRequest {
            agent_id: id.as_bytes().to_vec(),
            host_id: None,
            name: Some(decl.name.clone()),
            parent,
            initial_prompt: decl
                .prompt
                .as_deref()
                .map(|text| prompt(decl.kind, b"testnet-first", text)),
            cwd: host.work.to_string_lossy().into_owned(),
            kind: decl.kind.wire() as i32,
            config: Some(match decl.kind {
                FakeKind::Codex => {
                    create_agent_request::Config::Codex(CodexCreateConfig::default())
                }
                _ => create_agent_request::Config::Claude(ClaudeCreateConfig::default()),
            }),
            host_name: None,
        };
        let runtime = self.runtime(&decl.host)?;
        runtime.set_launch(self.launch(&host, Some((decl.kind, &script))));
        let spawned = runtime.spawn(request, None).await;
        runtime.set_launch(self.launch(&host, None));
        let agent = spawned?;
        self.agents.insert(
            decl.name.clone(),
            AgentRef {
                name: decl.name,
                host: host.name.clone(),
                host_id: host.host_id,
                id,
                kind: decl.kind,
            },
        );
        Ok(agent)
    }

    /// Starts an exited agent's next incarnation on the same script.
    pub async fn resume(
        &mut self,
        name: &str,
        text: Option<&str>,
    ) -> Result<wire::Agent, NetError> {
        let agent = self.agent(name)?.clone();
        let host = self.host(&agent.host)?.clone();
        let script = self
            .root
            .path()
            .join("scripts")
            .join(format!("{name}.json"));
        let runtime = self.runtime(&agent.host)?;
        runtime.set_launch(self.launch(&host, Some((agent.kind, &script))));
        let prompt = text.map(|text| prompt(agent.kind, Uuid::new_v4().as_bytes(), text));
        let resumed = runtime.resume(agent.id, prompt).await;
        runtime.set_launch(self.launch(&host, None));
        Ok(resumed?)
    }

    /// Sends the agent a prompt through its own host.
    pub async fn send(&self, name: &str, text: &str) -> Result<wire::SendInputResponse, NetError> {
        let agent = self.agent(name)?;
        let runtime = self.runtime(&agent.host)?;
        let input = prompt(agent.kind, Uuid::new_v4().as_bytes(), text);
        runtime
            .send_input(&wire::SendInputRequest {
                agent_id: agent.id.as_bytes().to_vec(),
                input: Some(input),
            })
            .await
            .map_err(|error| NetError::Host {
                host: agent.host.clone(),
                error: error.to_string(),
            })
    }

    /// Sends the agent any input through its own host: an answer, a
    /// withdrawal, an interrupt.
    pub async fn input(
        &self,
        name: &str,
        input: Input,
    ) -> Result<wire::SendInputResponse, NetError> {
        let agent = self.agent(name)?;
        let runtime = self.runtime(&agent.host)?;
        runtime
            .send_input(&wire::SendInputRequest {
                agent_id: agent.id.as_bytes().to_vec(),
                input: Some(input),
            })
            .await
            .map_err(|error| NetError::Host {
                host: agent.host.clone(),
                error: error.to_string(),
            })
    }

    /// Deletes the agent on its own host.
    pub async fn delete(&mut self, name: &str) -> Result<Ack, NetError> {
        let answer = self.delete_family(name).await?;
        Ok(self.ack(format!(
            "{name} deleted with {} children; {} children unreachable",
            answer.removed_children.len(),
            answer.unreachable_children.len()
        )))
    }

    /// Deletes the agent on its own host, as a person does there, and
    /// answers what the cascade reached and what it could not. The net
    /// forgets every agent the cascade removed.
    pub async fn delete_family(&mut self, name: &str) -> Result<DeleteAgentResponse, NetError> {
        let agent = self.agent(name)?.clone();
        let answer = self
            .client(&agent.host)?
            .delete_agent(tonic::Request::new(DeleteAgentRequest {
                agent_id: agent.id.as_bytes().to_vec(),
            }))
            .await
            .map_err(|status| NetError::Refused(Box::new(status)))?
            .into_inner();
        self.agents.remove(name);
        for child in &answer.removed_children {
            self.agents
                .retain(|_, known| known.id.as_bytes() != child.agent_id.as_slice());
        }
        Ok(answer)
    }

    /// The client service a person reaches on `host`: the same calls a
    /// phone or terminal makes there, forwarded to another host where the
    /// agent lives there.
    pub fn client(&self, host: &str) -> Result<ClientApi, NetError> {
        Ok(ClientApi::new(&self.runtime(host)?, None))
    }

    /// The client service as `name`'s tools socket serves it: every call
    /// is made as that agent, which its host checks and forwards.
    pub fn tools(&self, name: &str) -> Result<ClientApi, NetError> {
        let agent = self.agent(name)?;
        Ok(ClientApi::new(&self.runtime(&agent.host)?, Some(agent.id)))
    }

    /// `parent` spawns `decl` through its tools socket, naming
    /// `decl.host` as the host: the parent's host resolves the name among
    /// the hosts it trusts and forwards the create there, where the child
    /// plays its script in that host's work directory.
    pub async fn spawn_child(
        &mut self,
        parent: &str,
        decl: AgentDecl,
    ) -> Result<wire::Agent, NetError> {
        if self.agents.contains_key(&decl.name) {
            return Err(NetError::AgentExists(decl.name));
        }
        let host = self.host(&decl.host)?.clone();
        let script = self.write_script(&decl.name, decl.script.clone().unwrap_or_default())?;
        let id = Uuid::new_v4();
        let request = CreateAgentRequest {
            agent_id: id.as_bytes().to_vec(),
            host_name: Some(decl.host.clone()),
            name: Some(decl.name.clone()),
            initial_prompt: decl
                .prompt
                .as_deref()
                .map(|text| prompt(decl.kind, b"testnet-first", text)),
            cwd: host.work.to_string_lossy().into_owned(),
            kind: decl.kind.wire() as i32,
            config: Some(match decl.kind {
                FakeKind::Codex => {
                    create_agent_request::Config::Codex(CodexCreateConfig::default())
                }
                _ => create_agent_request::Config::Claude(ClaudeCreateConfig::default()),
            }),
            ..CreateAgentRequest::default()
        };
        let tools = self.tools(parent)?;
        let target = self.runtime(&decl.host)?;
        target.set_launch(self.launch(&host, Some((decl.kind, &script))));
        let spawned = tools.create_agent(tonic::Request::new(request)).await;
        target.set_launch(self.launch(&host, None));
        let agent = spawned
            .map_err(|status| NetError::Refused(Box::new(status)))?
            .into_inner();
        self.agents.insert(
            decl.name.clone(),
            AgentRef {
                name: decl.name,
                host: host.name.clone(),
                host_id: host.host_id,
                id,
                kind: decl.kind,
            },
        );
        Ok(agent)
    }

    /// The next process to start on `name`'s host, whoever starts it,
    /// plays `script` as `name`: for a resume the host makes on its own,
    /// such as a parent's message to its exited child.
    pub fn next_start(&self, name: &str, script: Script) -> Result<Ack, NetError> {
        let agent = self.agent(name)?.clone();
        let host = self.host(&agent.host)?.clone();
        let path = self.write_script(name, script)?;
        self.runtime(&agent.host)?
            .set_launch(self.launch(&host, Some((agent.kind, &path))));
        Ok(self.ack(format!("{name}'s next start plays its new script")))
    }

    /// Stops `name`'s process where it stands, as a machine too busy to
    /// schedule it would: it keeps its lock and its connection and answers
    /// nothing until thawed.
    pub fn freeze(&self, name: &str) -> Result<Ack, NetError> {
        self.signal(name, "-STOP")?;
        Ok(self.ack(format!("{name} frozen")))
    }

    /// Lets a frozen process run again.
    pub fn thaw(&self, name: &str) -> Result<Ack, NetError> {
        self.signal(name, "-CONT")?;
        Ok(self.ack(format!("{name} thawed")))
    }

    fn signal(&self, name: &str, signal: &str) -> Result<(), NetError> {
        let dir = self.agent_dir(name)?;
        #[cfg(unix)]
        {
            // `amux agent <dir>`: the directory is on its command line.
            let status = std::process::Command::new("pkill")
                .arg(signal)
                .arg("-f")
                .arg(format!("agent {}", dir.display()))
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(NetError::Host {
                    host: self.agent(name)?.host.clone(),
                    error: format!("no process of {name} to signal"),
                })
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (dir, signal);
            Err(NetError::Unsupported("signalling agent processes"))
        }
    }

    fn write_script(&self, name: &str, mut script: Script) -> Result<PathBuf, NetError> {
        let gates = self.gates();
        let resolve = |path: &mut PathBuf| {
            if path.is_relative() {
                *path = gates.join(&*path);
            }
        };
        for step in &mut script.steps {
            match step {
                Step::WaitFor { path } => resolve(path),
                Step::Tool(tool) => {
                    if let Some(path) = tool.wait_for.as_mut() {
                        resolve(path);
                    }
                }
                _ => {}
            }
        }
        let path = self
            .root
            .path()
            .join("scripts")
            .join(format!("{name}.json"));
        let text = serde_json::to_string_pretty(&script).map_err(std::io::Error::other)?;
        std::fs::write(&path, text)?;
        Ok(path)
    }

    /// What a host's agents start with. With an agent, its fake and its
    /// script; without, the host's defaults, which play nothing.
    fn launch(&self, host: &HostInfo, agent: Option<(FakeKind, &Path)>) -> Launch {
        let mut launch = Launch {
            install_path: self.binaries.amux(),
            claude_command: self
                .binaries
                .fake(FakeKind::ClaudeSdk)
                .to_string_lossy()
                .into_owned(),
            codex_command: self
                .binaries
                .fake(FakeKind::Codex)
                .to_string_lossy()
                .into_owned(),
            ..Launch::default()
        };
        launch.provider_env.insert(
            "CLAUDE_CONFIG_DIR".to_owned(),
            self.hosts[&host.name]
                .dir
                .join("claude")
                .to_string_lossy()
                .into_owned(),
        );
        if let Some((kind, script)) = agent {
            if kind == FakeKind::ClaudePty {
                launch.claude_command = self.binaries.fake(kind).to_string_lossy().into_owned();
            }
            launch
                .provider_env
                .insert(SCRIPT_ENV.to_owned(), script.to_string_lossy().into_owned());
        }
        // Long enough to ride out a daemon restart, short enough that an
        // agent a failed run leaves behind goes away on its own.
        launch.agent.grace_secs = 20;
        launch.agent.drain_secs = 5;
        launch.stop_deadline_ms = 15_000;
        if let Some(hook) = &self.launch_hook {
            hook(&host.name, &mut launch);
        }
        launch
    }

    // --- hosts -------------------------------------------------------------

    fn create_host(&mut self, decl: &HostDecl) -> Result<(), NetError> {
        // Names that differ only in case share a directory on a
        // case-insensitive filesystem.
        let twins = self
            .hosts
            .keys()
            .filter(|name| name.eq_ignore_ascii_case(&decl.name))
            .count();
        let dir = match twins {
            0 => self.root.path().join(&decl.name),
            n => self.root.path().join(format!("{}-{n}", decl.name)),
        };
        let data_dir = dir.join("data");
        let work = dir.join("work");
        std::fs::create_dir_all(&work)?;
        let profile = node::create_profile(&data_dir)?;
        let host_id = node::host_id(&node::profile_dir(&data_dir, profile))?;
        let front_door = dir.join("door.sock");
        let config = dir.join("installation.yaml");
        std::fs::write(
            &config,
            format!(
                "root: {}\nfront_door_socket: {}\nhost_name: {}\n",
                data_dir.display(),
                front_door.display(),
                decl.name,
            ),
        )?;
        self.hosts.insert(
            decl.name.clone(),
            Host {
                decl: decl.clone(),
                info: HostInfo {
                    name: decl.name.clone(),
                    host_id,
                    profile,
                    data_dir,
                    front_door,
                    config,
                    work,
                },
                dir,
                daemon: None,
                runtime: Weak::new(),
                boot: 1,
                checkpoint: None,
            },
        );
        Ok(())
    }

    async fn start_host(&mut self, name: &str) -> Result<(), NetError> {
        if let Some(relay) = &self.relay
            && !self.udp.contains_key(name)
        {
            let gate = relay.gate().await?;
            self.udp.insert(name.to_owned(), gate);
        }
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        if host.daemon.is_some() {
            return Err(NetError::Running(name.to_owned()));
        }
        let decl = &host.decl;
        let mut edge = EdgeOptions {
            host_name: decl.name.clone(),
            kinds: vec![
                wire::Kind::ClaudePty,
                wire::Kind::ClaudeSdk,
                wire::Kind::Codex,
            ],
            lan: decl.lan.then(|| LanOptions {
                bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            }),
            discovery: decl.discovery.then(|| scripted_discovery(&self.bus)),
            discovery_scope: decl
                .scope
                .clone()
                .unwrap_or_else(|| self.topology.scope.clone()),
            dial: decl.lan,
            link_socket: false,
            cloud: node::CloudOptions::default(),
        };
        if let Some(relay) = &self.relay {
            edge.cloud = relay.cloud_options(&self.udp[name]);
        }
        if let Some(hook) = &self.edge_hook {
            hook(name, &mut edge);
        }
        let options = node::StartOptions {
            data_dir: host.info.data_dir.clone(),
            boot_id: Some(format!("boot-{}", host.boot)),
            launch: self.launch(&host.info, None),
            clock: self.clock.clone(),
            push: Arc::new(node::NoopSender),
            daemon_log: None,
            front_door: Some(host.info.front_door.clone()),
            edge,
        };
        let daemon = node::start(options, None)
            .await
            .map_err(|error| NetError::Start {
                host: name.to_owned(),
                error: error.to_string(),
            })?;
        let runtime = daemon
            .profile(self.hosts[name].info.profile)
            .ok_or_else(|| NetError::Start {
                host: name.to_owned(),
                error: "the daemon does not host the profile".to_owned(),
            })?;
        let host = self.hosts.get_mut(name).expect("the host");
        host.runtime = Arc::downgrade(&runtime);
        host.daemon = Some(std::sync::Mutex::new(daemon));
        Ok(())
    }

    /// Makes `a` trust `b` as pairing would, without linking them: they
    /// meet over whatever route they find.
    pub async fn trust(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let far = self.edge(b)?;
        self.edge(a)?
            .trust(&far)
            .await
            .map_err(|error| NetError::Host {
                host: a.to_owned(),
                error: error.message().to_owned(),
            })?;
        Ok(self.ack(format!("{a} trusts {b}")))
    }

    /// Signs the host's profile in to `account` on the net's relay, through
    /// the front door as a person's login does, adopting the agents and
    /// peers it holds, and returns once its relay link is up.
    pub async fn sign_in(&self, host: &str, account: &str) -> Result<Ack, NetError> {
        let relay = self.relay()?;
        let profile = self.host(host)?.profile;
        self.front_door(host)
            .await?
            .bind_profile(wire::BindProfileRequest {
                profile_id: Some(profile.to_string()),
                cloud_url: relay.url().to_owned(),
                staged_refresh_token: relay.login(account),
                // The person confirms adopting whatever the profile holds.
                adopt_non_pristine: true,
                ..wire::BindProfileRequest::default()
            })
            .await
            .map_err(|status| NetError::Refused(Box::new(status)))?;
        let edge = self.edge(host)?;
        observe::eventually(&format!("{host}'s relay link"), PATIENCE, || {
            let connected = matches!(edge.observed(), node::Observed::Connected { .. });
            async move { connected }
        })
        .await?;
        Ok(self.ack(format!("{host} signed in to {account}")))
    }

    /// Takes UDP away from the host's way to the relay, or gives it back:
    /// datagrams through its gate are dropped both ways while blocked.
    pub fn block_udp(&self, host: &str, blocked: bool) -> Result<Ack, NetError> {
        let gate = self
            .udp
            .get(host)
            .ok_or_else(|| NetError::NoHost(host.to_owned()))?;
        gate.block(blocked);
        Ok(self.ack(format!(
            "UDP to the relay {} for {host}",
            if blocked { "blocked" } else { "open" }
        )))
    }

    /// Makes `a` forget `b`, as unpairing does: `b`'s key leaves `a`'s
    /// trust store and the links between them close.
    pub async fn untrust(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let peer = self.host(b)?.host_id;
        self.edge(a)?
            .unpair(
                wire::PeerRef {
                    identifier: Some(wire::peer_ref::Identifier::HostId(peer.as_bytes().to_vec())),
                },
                "testnet".to_owned(),
            )
            .await
            .map_err(|error| NetError::Host {
                host: a.to_owned(),
                error: error.message().to_owned(),
            })?;
        Ok(self.ack(format!("{a} no longer trusts {b}")))
    }

    /// Takes a host's daemon down: `clean` shuts it down, otherwise it
    /// crashes. Its links drop as its sockets would, and this returns once
    /// nothing of that run is left in the process.
    async fn take_down(&mut self, name: &str, clean: bool) -> Result<(), NetError> {
        let host = self
            .hosts
            .get_mut(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        let daemon = host
            .daemon
            .take()
            .ok_or_else(|| NetError::Down(name.to_owned()))?
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner());
        host.runtime = Weak::new();
        for ((a, b), link) in &mut self.links {
            if a == name || b == name {
                link.live = None;
            }
        }
        let runs: Vec<Weak<ProfileRuntime>> =
            daemon.profiles().iter().map(Arc::downgrade).collect();
        if clean {
            daemon.shutdown().await?;
        } else {
            drop(daemon);
        }
        observe::eventually(&format!("{name}'s runtime to be gone"), PATIENCE, || {
            let gone = runs.iter().all(|run| run.upgrade().is_none());
            async move { gone }
        })
        .await?;
        Ok(())
    }

    // --- faults ------------------------------------------------------------

    /// Cuts the link between two hosts the way a dead connection goes: no
    /// LinkClose, the carrier just stops.
    pub fn sever_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let link = self
            .links
            .get_mut(&link_key(a, b))
            .ok_or_else(|| NetError::NoLink(a.to_owned(), b.to_owned()))?;
        link.severed = true;
        if let Some(live) = link.live.take() {
            live.into_inner()
                .unwrap_or_else(|poison| poison.into_inner())
                .sever();
        }
        Ok(self.ack(format!("link {a} - {b} severed")))
    }

    /// Links the two hosts again, if both are up.
    pub fn restore_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let link = self
            .links
            .get_mut(&link_key(a, b))
            .ok_or_else(|| NetError::NoLink(a.to_owned(), b.to_owned()))?;
        link.severed = false;
        self.link_all()?;
        Ok(self.ack(format!("link {a} - {b} restored")))
    }

    /// Links every declared pair that is not severed, not live and has both
    /// ends up.
    fn link_all(&mut self) -> Result<(), NetError> {
        let wanted: Vec<(String, String)> = self
            .links
            .iter()
            .filter(|((a, b), link)| {
                !link.severed && link.live.is_none() && self.is_up(a) && self.is_up(b)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for (a, b) in wanted {
            let live = self
                .edge(&a)?
                .link_in_process(&self.edge(&b)?)
                .map_err(|error| NetError::Host {
                    host: a.clone(),
                    error,
                })?;
            self.links.get_mut(&(a, b)).expect("the link").live = Some(std::sync::Mutex::new(live));
        }
        Ok(())
    }

    /// Crashes a host's daemon. Its agent processes keep running, in their
    /// grace, waiting for the next daemon.
    pub async fn kill_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        self.take_down(host, false).await?;
        Ok(self.ack(format!("{host}'s daemon killed")))
    }

    /// Shuts a host's daemon down cleanly: flushed, marked clean. Its agent
    /// processes keep running.
    pub async fn stop_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        self.take_down(host, true).await?;
        Ok(self.ack(format!("{host}'s daemon stopped")))
    }

    /// Starts a host's daemon again from what its installation holds, under
    /// the same boot, and relinks its declared links.
    pub async fn restart_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        self.start_host(host).await?;
        self.link_all()?;
        Ok(self.ack(format!("{host}'s daemon started")))
    }

    /// Records what has reached the drive now: the store flushed and its
    /// file as it stands. [`Net::rewind_host`] loses power back to here.
    pub async fn checkpoint_host(&mut self, host: &str) -> Result<Ack, NetError> {
        let runtime = self.runtime(host)?;
        let bytes = {
            // Held so nothing commits between the flush and the read.
            let store = runtime.store().await;
            store.flush_to_drive().map_err(|error| NetError::Host {
                host: host.to_owned(),
                error: error.to_string(),
            })?;
            std::fs::read(runtime.dir().join(node::STORE))?
        };
        drop(runtime);
        self.hosts.get_mut(host).expect("the host").checkpoint = Some(bytes);
        Ok(self.ack(format!("{host} checkpointed")))
    }

    /// Power loss without power: the daemon and every agent process on the
    /// host die at once; the store goes back to the last checkpoint with
    /// no WAL, each named journal is cut at its byte, and the machine comes
    /// back under a new boot id with the clean flag never set.
    pub async fn rewind_host(&mut self, host: &str, cuts: &[JournalCut]) -> Result<Ack, NetError> {
        let checkpoint = self
            .hosts
            .get(host)
            .ok_or_else(|| NetError::NoHost(host.to_owned()))?
            .checkpoint
            .clone()
            .ok_or_else(|| NetError::NoCheckpoint(host.to_owned()))?;
        let mut journals = Vec::new();
        for cut in cuts {
            if self.agent(&cut.agent)?.host != host {
                return Err(NetError::Host {
                    host: host.to_owned(),
                    error: format!("agent {} runs elsewhere", cut.agent),
                });
            }
            journals.push((
                self.agent_dir(&cut.agent)?.join(agent_dir::JOURNAL),
                cut.byte,
            ));
        }
        if self.is_up(host) {
            self.take_down(host, false).await?;
        }
        let info = self.host(host)?.clone();
        let profile_dir = node::profile_dir(&info.data_dir, info.profile);
        kill_agents_under(&profile_dir.join(node::AGENTS)).await?;

        let db = profile_dir.join(node::STORE);
        std::fs::write(&db, checkpoint)?;
        for suffix in ["-wal", "-shm"] {
            let mut path = db.clone().into_os_string();
            path.push(suffix);
            match std::fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        for (journal, byte) in journals {
            journal::synthetic::cut(&journal, byte)?;
        }
        self.hosts.get_mut(host).expect("the host").boot += 1;
        self.start_host(host).await?;
        self.link_all()?;
        Ok(self.ack(format!("{host} lost power and came back")))
    }

    // --- observations ------------------------------------------------------

    /// Opens a Subscribe stream on `agent` at `host`, which may be its own
    /// host or one holding its replica, and records it.
    pub async fn observe(&self, host: &str, agent: &str, tail: u32) -> Result<Observer, NetError> {
        let id = self.agent(agent)?.id;
        let mut subscription = self.runtime(host)?.subscribe(id.as_bytes(), tail).await?;
        let (sender, incoming) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Some(event) = subscription.next().await {
                if sender.send((*event).clone()).is_err() {
                    return;
                }
            }
        });
        Ok(ObserverOf::new(
            format!("{agent} observed at {host}"),
            incoming,
            reader,
        ))
    }

    /// Opens a SubscribeInventory stream on `host` and records it.
    pub async fn observe_inventory(&self, host: &str) -> Result<InventoryObserver, NetError> {
        let mut subscription = self.runtime(host)?.subscribe_inventory().await?;
        let (sender, incoming) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Some(event) = subscription.next().await {
                if sender.send((*event).clone()).is_err() {
                    return;
                }
            }
        });
        Ok(ObserverOf::new(
            format!("inventory at {host}"),
            incoming,
            reader,
        ))
    }

    /// Whether both ends of a link route to each other directly now.
    pub async fn link_up(&self, a: &str, b: &str) -> bool {
        let (Ok(edge_a), Ok(edge_b), Ok(info_a), Ok(info_b)) =
            (self.edge(a), self.edge(b), self.host(a), self.host(b))
        else {
            return false;
        };
        edge_a.via(info_b.host_id).await == HostVia::Direct
            && edge_b.via(info_a.host_id).await == HostVia::Direct
    }

    /// Waits until the link between two hosts is up, or down on both ends.
    pub async fn wait_link(&self, a: &str, b: &str, up: bool) -> Result<(), NetError> {
        let (id_a, id_b) = (self.host(a)?.host_id, self.host(b)?.host_id);
        let what = format!("link {a} - {b} {}", if up { "up" } else { "down" });
        observe::eventually(&what, PATIENCE, || async {
            let via_ab = match self.edge(a) {
                Ok(edge) => edge.via(id_b).await,
                Err(_) => HostVia::Offline,
            };
            let via_ba = match self.edge(b) {
                Ok(edge) => edge.via(id_a).await,
                Err(_) => HostVia::Offline,
            };
            if up {
                via_ab == HostVia::Direct && via_ba == HostVia::Direct
            } else {
                via_ab == HostVia::Offline && via_ba == HostVia::Offline
            }
        })
        .await?;
        Ok(())
    }

    /// Checks the replica block invariant for `agent` at `host` against
    /// its origin, at a settled moment: `host`'s rows for it are empty or
    /// one contiguous block ending at the origin's newest row, at the
    /// origin's revisions.
    pub async fn assert_block_invariant(&self, host: &str, agent: &str) -> Result<(), NetError> {
        let agent = self.agent(agent)?.clone();
        let key = agent.key();
        let replica = {
            let runtime = self.runtime(host)?;
            let store = runtime.store().await;
            invariant::block(&store, &key).map_err(|error| NetError::Host {
                host: host.to_owned(),
                error,
            })?
        };
        let origin = {
            let runtime = self.runtime(&agent.host)?;
            let store = runtime.store().await;
            invariant::block(&store, &key).map_err(|error| NetError::Host {
                host: agent.host.clone(),
                error,
            })?
        };
        invariant::check(&replica, &origin)?;
        Ok(())
    }

    // --- teardown ----------------------------------------------------------

    /// Stops every agent and shuts every daemon down cleanly. The explicit
    /// end of a net; dropping one kills what it can and is for panics.
    pub async fn shutdown(mut self) -> Result<(), NetError> {
        let names: Vec<String> = self.hosts.keys().cloned().collect();
        for name in &names {
            if let Ok(runtime) = self.runtime(name) {
                for id in runtime.live() {
                    let _ = tokio::time::timeout(PATIENCE, runtime.stop(id, StopMode::Kill)).await;
                }
            }
        }
        for name in &names {
            if self.is_up(name) {
                self.take_down(name, true).await?;
            }
        }
        // Agents of hosts that were down at the end have no daemon to
        // stop them.
        kill_agents_under(self.root.path()).await?;
        Ok(())
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        for host in self.hosts.values_mut() {
            drop(host.daemon.take());
        }
        #[cfg(unix)]
        let _ = std::process::Command::new("pkill")
            .args(["-KILL", "-f"])
            .arg(self.root.path())
            .status();
    }
}

/// A first prompt or a message in the kind's own input arm.
pub fn prompt(kind: FakeKind, id: &[u8], text: &str) -> Input {
    let prompt = PromptInput {
        text: text.to_owned(),
        attachments: Vec::new(),
    };
    Input {
        input_id: id.to_vec(),
        of: Some(match kind {
            FakeKind::ClaudePty => input::Of::ClaudePty(ClaudePtyInput {
                of: Some(claude_pty_input::Of::Prompt(prompt)),
            }),
            FakeKind::ClaudeSdk => input::Of::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Prompt(prompt)),
            }),
            FakeKind::Codex => input::Of::Codex(CodexInput {
                of: Some(codex_input::Of::Prompt(prompt)),
            }),
        }),
    }
}

/// Withdraws a queued prompt, in the kind's own input arm.
pub fn withdraw(kind: FakeKind, id: &[u8], queued: &[u8]) -> Input {
    let withdraw = WithdrawQueued {
        queued_input_id: queued.to_vec(),
    };
    Input {
        input_id: id.to_vec(),
        of: Some(match kind {
            FakeKind::ClaudePty => input::Of::ClaudePty(ClaudePtyInput {
                of: Some(claude_pty_input::Of::Withdraw(withdraw)),
            }),
            FakeKind::ClaudeSdk => input::Of::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Withdraw(withdraw)),
            }),
            FakeKind::Codex => input::Of::Codex(CodexInput {
                of: Some(codex_input::Of::Withdraw(withdraw)),
            }),
        }),
    }
}

/// SIGKILLs every agent process whose directory lies under `dir`, and
/// waits until each has released its directory's lock.
async fn kill_agents_under(dir: &Path) -> Result<(), NetError> {
    #[cfg(unix)]
    {
        // `amux agent <dir>`: the agent's own directory is on its command
        // line, and every agent of the net lives under its root.
        let _ = tokio::process::Command::new("pkill")
            .args(["-KILL", "-f"])
            .arg(dir)
            .status()
            .await;
        let locked: Vec<PathBuf> = agent_dirs(dir);
        observe::eventually("the killed agents to release their locks", PATIENCE, || {
            let held = locked.iter().any(|dir| agent_dir::locked(dir));
            async move { !held }
        })
        .await?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Err(NetError::Unsupported("killing agent processes"))
    }
}

/// Every directory under `dir` that holds an agent's lock file.
#[cfg(unix)]
fn agent_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join(agent_dir::LOCK).exists() {
                    found.push(path);
                } else {
                    pending.push(path);
                }
            }
        }
    }
    found
}

//! One value declaring a net: hosts, the links between them, the relay and
//! the accounts signed in to it, and the agents each host runs on which
//! fake provider. The Rust builder and the JSON
//! loader both construct it, and [`Topology::validate`] catches unknown and
//! duplicate names before anything starts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use provider_fakes::script::{Script, Step};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topology {
    /// The discovery scope every host advertises and filters by, unless a
    /// host names its own.
    #[serde(default)]
    pub scope: String,
    pub hosts: Vec<HostDecl>,
    #[serde(default)]
    pub links: Vec<LinkDecl>,
    #[serde(default)]
    pub agents: Vec<AgentDecl>,
    /// A relay every host can reach, and the accounts its cloud knows.
    #[serde(default)]
    pub relay: Option<RelayDecl>,
    /// Start each declared agent only once the one before it has settled
    /// (see [`crate::Net::settle`]), so agents that come to rest in the same
    /// standing are listed in declaration order, the last declared as the
    /// most recently active. Every declared agent must come to rest.
    /// Without it the agents start together and their first turns race.
    #[serde(default)]
    pub settle: bool,
}

/// One relay, with the cloud that signs accounts in to it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayDecl {
    #[serde(default)]
    pub accounts: Vec<AccountDecl>,
}

/// An account the cloud knows, and what it has bought.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountDecl {
    pub name: String,
    #[serde(default)]
    pub tier: TierDecl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierDecl {
    /// Relayed tunnels between the account's hosts.
    #[default]
    Pro,
    /// The account's hosts are listed; the relay opens no tunnels.
    Free,
}

/// One installation with one profile: a daemon the net starts in process,
/// serving its front door on a local socket.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostDecl {
    pub name: String,
    /// Listen for direct links on a loopback QUIC listener.
    #[serde(default)]
    pub lan: bool,
    /// Advertise and browse on the net's scripted discovery bus, and dial
    /// trusted hosts it finds.
    #[serde(default)]
    pub discovery: bool,
    /// Advertise on the machine's real local network through the system's
    /// mDNS responder, for a client outside the net to find; needs `lan`.
    #[serde(default)]
    pub bonjour: bool,
    /// This host's discovery scope, where it differs from the net's.
    #[serde(default)]
    pub scope: Option<String>,
    /// The relay account this host's profile signs in to at start.
    #[serde(default)]
    pub account: Option<String>,
    /// What agents a client starts on this host play; none plays nothing.
    #[serde(default)]
    pub script: Option<Script>,
    /// Git repositories the net makes under the host's one repository
    /// root, by path below it; none leaves the host without a root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<String>,
}

/// Two hosts that trust each other, linked in process over a loopback
/// carrier: the link the fault verbs sever and restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkDecl {
    pub a: String,
    pub b: String,
}

/// An agent the net spawns at start: a real `amux agent` process running a
/// fake provider that plays `script`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDecl {
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub kind: FakeKind,
    /// What the fake provider plays, inline.
    #[serde(default)]
    pub script: Option<Script>,
    /// What the fake provider plays, from a file; a relative path is read
    /// from the topology file's directory.
    #[serde(default)]
    pub script_file: Option<PathBuf>,
    /// The first prompt, sent at creation.
    #[serde(default)]
    pub prompt: Option<String>,
    /// An agent declared before this one, on any host.
    #[serde(default)]
    pub parent: Option<String>,
    /// The directory it starts in; none is its host's work directory. An
    /// empty one names none, as an agent's spawn tool sends it, and leaves
    /// the choice to the host that starts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Which fake provider an agent runs, and so which interpreter reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeKind {
    ClaudePty,
    #[default]
    ClaudeSdk,
    Codex,
}

impl FakeKind {
    pub fn wire(self) -> wire::Kind {
        match self {
            Self::ClaudePty => wire::Kind::ClaudePty,
            Self::ClaudeSdk => wire::Kind::ClaudeSdk,
            Self::Codex => wire::Kind::Codex,
        }
    }

    /// The fake binary standing in for the provider.
    pub fn binary(self) -> &'static str {
        match self {
            Self::ClaudePty => "fake-claude-pty",
            Self::ClaudeSdk => "fake-claude-sdk",
            Self::Codex => "fake-codex",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("reading {path}: {error}")]
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("parsing {path}: {error}")]
    Parse {
        path: PathBuf,
        error: serde_json::Error,
    },
    #[error("a topology needs at least one host")]
    NoHosts,
    #[error("host {0:?} is declared twice")]
    DuplicateHost(String),
    #[error("a host name must be a non-empty word of letters, digits, '-' or '_': {0:?}")]
    BadHostName(String),
    #[error("link {a:?} - {b:?} names unknown host {unknown:?}")]
    LinkUnknownHost {
        a: String,
        b: String,
        unknown: String,
    },
    #[error(
        "host {0:?} advertises through Bonjour, which needs lan and not the scripted discovery"
    )]
    BonjourNeedsLan(String),
    #[error("host {0:?} is linked to itself")]
    SelfLink(String),
    #[error("hosts {a:?} and {b:?} are linked twice")]
    DuplicateLink { a: String, b: String },
    #[error("agent {0:?} is declared twice")]
    DuplicateAgent(String),
    #[error("agent {agent:?} runs on unknown host {host:?}")]
    AgentUnknownHost { agent: String, host: String },
    #[error("agent {agent:?} names parent {parent:?}, which is not declared before it")]
    UnknownParent { agent: String, parent: String },
    #[error("host {host:?} signs in to {account:?}, but the topology declares no relay")]
    NoRelay { host: String, account: String },
    #[error("host {host:?} signs in to unknown account {account:?}")]
    UnknownAccount { host: String, account: String },
    #[error("account {0:?} is declared twice")]
    DuplicateAccount(String),
    #[error("agent {0:?} has both an inline script and a script file")]
    TwoScripts(String),
    #[error("agent {agent:?}'s script {path}: {error}")]
    Script {
        agent: String,
        path: PathBuf,
        error: String,
    },
}

impl Topology {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a topology from a JSON file, loading each agent's script file
    /// into it, and validates it.
    pub fn load(path: &Path) -> Result<Self, TopologyError> {
        let text = std::fs::read_to_string(path).map_err(|error| TopologyError::Read {
            path: path.to_owned(),
            error,
        })?;
        let mut topology: Self =
            serde_json::from_str(&text).map_err(|error| TopologyError::Parse {
                path: path.to_owned(),
                error,
            })?;
        let base = path.parent().unwrap_or(Path::new("."));
        for agent in &mut topology.agents {
            if agent.script.is_some() && agent.script_file.is_some() {
                return Err(TopologyError::TwoScripts(agent.name.clone()));
            }
            if let Some(file) = agent.script_file.take() {
                let file = base.join(file);
                let script = Script::load(&file).map_err(|error| TopologyError::Script {
                    agent: agent.name.clone(),
                    path: file.clone(),
                    error: error.to_string(),
                })?;
                agent.script = Some(script);
            }
        }
        topology.validate()?;
        Ok(topology)
    }

    pub fn scope(mut self, scope: &str) -> Self {
        scope.clone_into(&mut self.scope);
        self
    }

    /// A host with nothing on the network but its in-process links.
    pub fn host(self, name: &str) -> Self {
        self.host_decl(HostDecl {
            name: name.to_owned(),
            ..HostDecl::default()
        })
    }

    pub fn host_decl(mut self, host: HostDecl) -> Self {
        self.hosts.push(host);
        self
    }

    /// Declares the relay, with these accounts on the Pro tier.
    pub fn relay(mut self, accounts: &[&str]) -> Self {
        self.relay = Some(RelayDecl {
            accounts: accounts
                .iter()
                .map(|name| AccountDecl {
                    name: (*name).to_owned(),
                    tier: TierDecl::Pro,
                })
                .collect(),
        });
        self
    }

    pub fn link(mut self, a: &str, b: &str) -> Self {
        self.links.push(LinkDecl {
            a: a.to_owned(),
            b: b.to_owned(),
        });
        self
    }

    pub fn agent(mut self, agent: AgentDecl) -> Self {
        self.agents.push(agent);
        self
    }

    pub fn validate(&self) -> Result<(), TopologyError> {
        if self.hosts.is_empty() {
            return Err(TopologyError::NoHosts);
        }
        let mut hosts = BTreeSet::new();
        for host in &self.hosts {
            let word = !host.name.is_empty()
                && host
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !word {
                return Err(TopologyError::BadHostName(host.name.clone()));
            }
            if !hosts.insert(host.name.as_str()) {
                return Err(TopologyError::DuplicateHost(host.name.clone()));
            }
            if host.bonjour && (!host.lan || host.discovery) {
                return Err(TopologyError::BonjourNeedsLan(host.name.clone()));
            }
        }
        let mut accounts = BTreeSet::new();
        for account in self.relay.iter().flat_map(|relay| &relay.accounts) {
            if !accounts.insert(account.name.as_str()) {
                return Err(TopologyError::DuplicateAccount(account.name.clone()));
            }
        }
        for host in &self.hosts {
            if let Some(account) = &host.account {
                if self.relay.is_none() {
                    return Err(TopologyError::NoRelay {
                        host: host.name.clone(),
                        account: account.clone(),
                    });
                }
                if !accounts.contains(account.as_str()) {
                    return Err(TopologyError::UnknownAccount {
                        host: host.name.clone(),
                        account: account.clone(),
                    });
                }
            }
        }
        let mut links = BTreeSet::new();
        for link in &self.links {
            for end in [&link.a, &link.b] {
                if !hosts.contains(end.as_str()) {
                    return Err(TopologyError::LinkUnknownHost {
                        a: link.a.clone(),
                        b: link.b.clone(),
                        unknown: end.clone(),
                    });
                }
            }
            if link.a == link.b {
                return Err(TopologyError::SelfLink(link.a.clone()));
            }
            if !links.insert(link_key(&link.a, &link.b)) {
                return Err(TopologyError::DuplicateLink {
                    a: link.a.clone(),
                    b: link.b.clone(),
                });
            }
        }
        let mut agents = BTreeSet::new();
        for agent in &self.agents {
            if !hosts.contains(agent.host.as_str()) {
                return Err(TopologyError::AgentUnknownHost {
                    agent: agent.name.clone(),
                    host: agent.host.clone(),
                });
            }
            if agent.script.is_some() && agent.script_file.is_some() {
                return Err(TopologyError::TwoScripts(agent.name.clone()));
            }
            if let Some(parent) = &agent.parent
                && !agents.contains(parent.as_str())
            {
                return Err(TopologyError::UnknownParent {
                    agent: agent.name.clone(),
                    parent: parent.clone(),
                });
            }
            if !agents.insert(agent.name.as_str()) {
                return Err(TopologyError::DuplicateAgent(agent.name.clone()));
            }
        }
        Ok(())
    }
}

impl AgentDecl {
    /// A headless Claude agent on `host` whose provider says nothing until
    /// given steps.
    pub fn new(name: &str, host: &str) -> Self {
        Self {
            name: name.to_owned(),
            host: host.to_owned(),
            ..Self::default()
        }
    }

    pub fn kind(mut self, kind: FakeKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn steps(mut self, steps: Vec<Step>) -> Self {
        self.script = Some(Script {
            steps,
            ..Script::default()
        });
        self
    }

    pub fn prompt(mut self, prompt: &str) -> Self {
        self.prompt = Some(prompt.to_owned());
        self
    }

    pub fn parent(mut self, parent: &str) -> Self {
        self.parent = Some(parent.to_owned());
        self
    }

    pub fn cwd(mut self, cwd: &str) -> Self {
        self.cwd = Some(cwd.to_owned());
        self
    }
}

/// A link's name whichever end is written first.
pub(crate) fn link_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

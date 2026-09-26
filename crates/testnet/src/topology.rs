//! One value declaring a net: hosts, the links between them, and the agents
//! each host runs on which fake provider. The Rust builder and the JSON
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
    /// This host's discovery scope, where it differs from the net's.
    #[serde(default)]
    pub scope: Option<String>,
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
}

/// A link's name whichever end is written first.
pub(crate) fn link_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

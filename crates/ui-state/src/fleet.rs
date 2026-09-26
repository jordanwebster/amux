//! The fleet: hosts and agent rows from the inventory stream, and the
//! families their parent edges form.

use std::collections::{BTreeMap, BTreeSet};

use wire::{Agent, HostEntry, InventoryEvent, inventory_event};

use crate::session::Connection;

pub type HostId = Vec<u8>;

/// An agent is named by its host and its id: a parent may live elsewhere.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentRef {
    pub host: HostId,
    pub agent: Vec<u8>,
}

impl AgentRef {
    pub fn of(agent: &Agent) -> AgentRef {
        AgentRef {
            host: agent.host_id.clone(),
            agent: agent.agent_id.clone(),
        }
    }

    fn parent(agent: &Agent) -> Option<AgentRef> {
        agent.parent.as_ref().map(|parent| AgentRef {
            host: parent.host_id.clone(),
            agent: parent.agent_id.clone(),
        })
    }
}

/// How loudly an agent asks for the person, quietest first. A family is as
/// loud as its loudest member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    Exited,
    Idle,
    Starting,
    Working,
    NeedsYou,
}

impl Attention {
    pub fn of(agent: &Agent) -> Attention {
        if agent.lifecycle() == wire::Lifecycle::Exited {
            return Attention::Exited;
        }
        match agent.phase() {
            wire::Phase::Starting => Attention::Starting,
            wire::Phase::Idle => Attention::Idle,
            wire::Phase::Working => Attention::Working,
            wire::Phase::NeedsYou => Attention::NeedsYou,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FleetMsg {
    Event(Box<InventoryEvent>),
    Connection(Connection),
}

/// Parent edges, derived from the agent rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Families {
    children: BTreeMap<AgentRef, BTreeSet<AgentRef>>,
}

impl Families {
    pub fn children(&self, parent: &AgentRef) -> impl DoubleEndedIterator<Item = &AgentRef> {
        self.children.get(parent).into_iter().flatten()
    }
}

/// The fleet. No I/O; rebuilt from the inventory stream on every open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FleetState {
    hosts: BTreeMap<HostId, HostEntry>,
    agents: BTreeMap<AgentRef, Agent>,
    families: Families,
    caught_up: bool,
    connection: Connection,
    /// Rows re-listed since the stream reopened; the rest are dropped at the
    /// next CaughtUp.
    relisted: Option<(BTreeSet<HostId>, BTreeSet<AgentRef>)>,
}

impl FleetState {
    pub fn new() -> FleetState {
        FleetState::default()
    }

    pub fn caught_up(&self) -> bool {
        self.caught_up
    }

    pub fn connection(&self) -> Connection {
        self.connection
    }

    pub fn hosts(&self) -> impl Iterator<Item = &HostEntry> {
        self.hosts.values()
    }

    pub fn host(&self, host: &[u8]) -> Option<&HostEntry> {
        self.hosts.get(host)
    }

    pub fn agents(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values()
    }

    pub fn agent(&self, agent: &AgentRef) -> Option<&Agent> {
        self.agents.get(agent)
    }

    /// An agent by id alone, on whichever host holds it.
    pub fn find(&self, agent_id: &[u8]) -> Option<&Agent> {
        self.agents
            .values()
            .find(|agent| agent.agent_id == agent_id)
    }

    pub fn families(&self) -> &Families {
        &self.families
    }

    /// The agent's parent, when the fleet holds it.
    pub fn parent(&self, agent: &AgentRef) -> Option<&Agent> {
        let parent = AgentRef::parent(self.agents.get(agent)?)?;
        self.agents.get(&parent)
    }

    /// The top of the agent's family: the furthest ancestor the fleet holds.
    pub fn root(&self, agent: &AgentRef) -> AgentRef {
        let mut at = agent.clone();
        let mut seen = BTreeSet::new();
        while let Some(parent) = self.agents.get(&at).and_then(AgentRef::parent) {
            if !self.agents.contains_key(&parent) || !seen.insert(at.clone()) {
                break;
            }
            at = parent;
        }
        at
    }

    /// Agents that head a family: no parent, or a parent the fleet does not
    /// hold.
    pub fn roots(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values().filter(|agent| {
            AgentRef::parent(agent).is_none_or(|parent| !self.agents.contains_key(&parent))
        })
    }

    /// The agent and every descendant, depth first.
    pub fn family(&self, agent: &AgentRef) -> Vec<&Agent> {
        let mut out = Vec::new();
        let mut stack = vec![agent.clone()];
        let mut seen = BTreeSet::new();
        while let Some(at) = stack.pop() {
            if !seen.insert(at.clone()) {
                continue;
            }
            if let Some(agent) = self.agents.get(&at) {
                out.push(agent);
            }
            stack.extend(self.families.children(&at).rev().cloned());
        }
        out
    }

    /// How loudly the agent's family asks for the person: its loudest
    /// member, the agent included.
    pub fn family_attention(&self, agent: &AgentRef) -> Option<Attention> {
        self.family(agent).into_iter().map(Attention::of).max()
    }

    /// Applies one message and returns the agents whose card or family
    /// attention may differ.
    pub fn update(&mut self, msg: FleetMsg) -> Vec<AgentRef> {
        let mut changed = BTreeSet::new();
        match msg {
            FleetMsg::Connection(connection) => {
                self.connection = connection;
                if connection == Connection::Reconnecting {
                    self.caught_up = false;
                    self.relisted = Some(Default::default());
                }
            }
            FleetMsg::Event(event) => {
                let Some(event) = event.of else {
                    return Vec::new();
                };
                match event {
                    inventory_event::Of::Host(host) => {
                        if let Some((hosts, _)) = &mut self.relisted {
                            hosts.insert(host.host_id.clone());
                        }
                        for agent in self
                            .agents
                            .keys()
                            .filter(|agent| agent.host == host.host_id)
                        {
                            changed.insert(agent.clone());
                        }
                        self.hosts.insert(host.host_id.clone(), host);
                    }
                    inventory_event::Of::HostRemoved(removed) => {
                        self.hosts.remove(&removed.host_id);
                        for agent in self
                            .agents
                            .keys()
                            .filter(|agent| agent.host == removed.host_id)
                        {
                            changed.insert(agent.clone());
                        }
                    }
                    inventory_event::Of::Agent(agent) => {
                        let at = AgentRef::of(&agent);
                        if let Some((_, agents)) = &mut self.relisted {
                            agents.insert(at.clone());
                        }
                        self.put(at, Some(agent), &mut changed);
                    }
                    inventory_event::Of::AgentRemoved(removed) => {
                        let at = AgentRef {
                            host: removed.host_id,
                            agent: removed.agent_id,
                        };
                        self.put(at, None, &mut changed);
                    }
                    inventory_event::Of::CaughtUp(_) => {
                        self.caught_up = true;
                        if let Some((hosts, agents)) = self.relisted.take() {
                            self.hosts.retain(|host, _| hosts.contains(host));
                            let gone: Vec<AgentRef> = self
                                .agents
                                .keys()
                                .filter(|agent| !agents.contains(agent))
                                .cloned()
                                .collect();
                            for at in gone {
                                self.put(at, None, &mut changed);
                            }
                        }
                    }
                }
            }
        }
        changed.into_iter().collect()
    }

    /// Replaces or removes one row, keeping the parent edges, and marks the
    /// row and every ancestor whose family attention may move.
    fn put(&mut self, at: AgentRef, agent: Option<Agent>, changed: &mut BTreeSet<AgentRef>) {
        let old_parent = self.agents.get(&at).and_then(AgentRef::parent);
        if let Some(parent) = &old_parent {
            self.mark_ancestors(parent, changed);
            if let Some(children) = self.families.children.get_mut(parent) {
                children.remove(&at);
                if children.is_empty() {
                    self.families.children.remove(parent);
                }
            }
        }
        changed.insert(at.clone());
        match agent {
            Some(agent) => {
                let parent = AgentRef::parent(&agent);
                self.agents.insert(at.clone(), agent);
                if let Some(parent) = parent {
                    self.families
                        .children
                        .entry(parent.clone())
                        .or_default()
                        .insert(at.clone());
                    self.mark_ancestors(&parent, changed);
                }
            }
            None => {
                self.agents.remove(&at);
                // Children keep their edge: they head their own family until
                // the parent returns.
                for child in self.families.children(&at) {
                    changed.insert(child.clone());
                }
            }
        }
    }

    fn mark_ancestors(&self, from: &AgentRef, changed: &mut BTreeSet<AgentRef>) {
        let mut at = Some(from.clone());
        while let Some(agent) = at {
            if !changed.insert(agent.clone()) {
                break;
            }
            at = self.agents.get(&agent).and_then(AgentRef::parent);
        }
    }
}

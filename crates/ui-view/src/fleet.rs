//! The fleet: agent rows ranked by their family's loudest member, cards,
//! and the family header a chat shows.

use std::collections::HashSet;

use ui_state::{AgentRef, Attention, FleetState};
use wire::{Agent, Kind, Presence};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetCard {
    pub agent: AgentRef,
    pub name: String,
    pub kind: Kind,
    pub attention: Attention,
    pub exit_cause: Option<String>,
    pub working_on: Option<String>,
    pub cwd: String,
    pub last_activity_ms: i64,
    pub host: String,
    pub host_presence: Presence,
    /// Children in the fleet, and how loud the family is.
    pub children: u32,
    pub family_attention: Attention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetRow {
    pub card: FleetCard,
    /// Zero for a family's head.
    pub depth: u32,
    pub expanded: bool,
}

fn card(fleet: &FleetState, agent: &Agent) -> FleetCard {
    let at = AgentRef::of(agent);
    let host = fleet.host(&agent.host_id);
    FleetCard {
        name: agent.name.clone().unwrap_or_default(),
        kind: agent.kind(),
        attention: Attention::of(agent),
        exit_cause: agent.exit_cause.clone(),
        working_on: agent
            .working_on
            .as_ref()
            .map(|working| working.text.clone()),
        cwd: agent.cwd.clone(),
        last_activity_ms: agent.last_activity_ms,
        host: host.map(|host| host.name.clone()).unwrap_or_default(),
        host_presence: host.map_or(Presence::Unspecified, |host| host.presence()),
        children: fleet.families().children(&at).count() as u32,
        family_attention: fleet.family_attention(&at).unwrap_or(Attention::Exited),
        agent: at,
    }
}

/// Every family head, loudest family first and then most recently active,
/// with the members of expanded families under their parents.
pub fn fleet_list(fleet: &FleetState, expand: &HashSet<Vec<u8>>) -> Vec<FleetRow> {
    let mut heads: Vec<FleetCard> = fleet.roots().map(|agent| card(fleet, agent)).collect();
    heads.sort_by(|a, b| {
        b.family_attention
            .cmp(&a.family_attention)
            .then(b.last_activity_ms.cmp(&a.last_activity_ms))
            .then(a.agent.cmp(&b.agent))
    });
    let mut rows = Vec::new();
    for head in heads {
        push(fleet, head, 0, expand, &mut rows);
    }
    rows
}

fn push(
    fleet: &FleetState,
    card_: FleetCard,
    depth: u32,
    expand: &HashSet<Vec<u8>>,
    rows: &mut Vec<FleetRow>,
) {
    let expanded = expand.contains(&card_.agent.agent);
    let children: Vec<FleetCard> = if expanded {
        let mut children: Vec<FleetCard> = fleet
            .families()
            .children(&card_.agent)
            .filter_map(|child| fleet.agent(child))
            .map(|agent| card(fleet, agent))
            .collect();
        children.sort_by(|a, b| {
            b.family_attention
                .cmp(&a.family_attention)
                .then(b.last_activity_ms.cmp(&a.last_activity_ms))
        });
        children
    } else {
        Vec::new()
    };
    rows.push(FleetRow {
        card: card_,
        depth,
        expanded,
    });
    for child in children {
        push(fleet, child, depth + 1, expand, rows);
    }
}

pub fn fleet_card(fleet: &FleetState, agent_id: &[u8]) -> Option<FleetCard> {
    fleet.find(agent_id).map(|agent| card(fleet, agent))
}

/// A chat's family: its parent, its children, and how loud the family is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyHeader {
    pub parent: Option<FleetCard>,
    pub children: Vec<FleetCard>,
    pub attention: Attention,
}

/// None for an agent with no parent and no children.
pub fn family_header(fleet: &FleetState, agent_id: &[u8]) -> Option<FamilyHeader> {
    let agent = fleet.find(agent_id)?;
    let at = AgentRef::of(agent);
    let parent = fleet.parent(&at).map(|parent| card(fleet, parent));
    let children: Vec<FleetCard> = fleet
        .families()
        .children(&at)
        .filter_map(|child| fleet.agent(child))
        .map(|child| card(fleet, child))
        .collect();
    if parent.is_none() && children.is_empty() {
        return None;
    }
    let root = fleet.root(&at);
    Some(FamilyHeader {
        parent,
        children,
        attention: fleet.family_attention(&root).unwrap_or(Attention::Exited),
    })
}

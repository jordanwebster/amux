//! The fleet: home's sections, each family placed by its loudest member and
//! ordered by since-when, every row with its second line; cards; and the
//! family header a chat shows.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Activity, ActivityKind, AgentKey, Attention, FleetState, ItemBody, SessionState};
use wire::{Agent, Kind, Presence, SignInState, UsageState};

use crate::ask::{AskBody, ask_card};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FleetCard {
    pub agent: AgentKey,
    pub name: String,
    pub kind: Kind,
    pub attention: Attention,
    pub exit_cause: Option<String>,
    /// The branch of the folder the agent started in, as of its last turn
    /// end; None on a detached head or outside a repository.
    pub branch: Option<String>,
    pub cwd: String,
    pub phase_since_ms: i64,
    pub host: String,
    pub host_presence: Presence,
    /// Children in the fleet, and how loud the family is.
    pub children: u32,
    pub family_attention: Attention,
    /// The whole family below and including this agent, and how many of
    /// them need the person: what a folded family stands for when a client
    /// counts its fleet.
    pub members: u32,
    pub members_need_you: u32,
}

/// Home's sections, computed once for every client. Empty sections are
/// left out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
pub struct FleetView {
    pub sections: Vec<FleetSection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FleetSection {
    pub kind: SectionKind,
    /// How many families the section holds, folded or not.
    pub families: u32,
    /// Each family's head, with the members of expanded families under it.
    pub rows: Vec<FleetRow>,
}

/// Where a family sits: by its loudest member. Live holds the starting,
/// working and idle: alive, whether or not busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub enum SectionKind {
    NeedsYou,
    Live,
    Exited,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FleetRow {
    pub card: FleetCard,
    /// Zero for a family's head.
    pub depth: u32,
    pub expanded: bool,
    pub second_line: SecondLine,
    /// On a folded family's head that does not itself need the person: the
    /// member that does, which the row speaks for.
    pub loud: Option<LoudMember>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct LoudMember {
    pub agent: AgentKey,
    pub name: String,
    pub second_line: SecondLine,
}

/// What a row says under its name, by the agent's state. What the agent is
/// working on is a fact for agents finding each other, not for this line.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub enum SecondLine {
    /// Needs you: what it asks.
    Ask(AskSummary),
    /// Working: the step it is running, as the chat's activity line has it.
    Step(ActivityLine),
    /// Idle: the first line of what it last said.
    LastSaid(String),
    /// Idle, and cannot go on until the person acts.
    Stuck(StuckReason),
    Exited(ExitCause),
    /// The agent's host is out of reach; the agent is only as it last said.
    HostAway,
    /// Starting, or nothing known yet.
    Blank,
}

/// The head ask, small enough for a row: what is asked, without the diff,
/// the plan or the form that the ask card carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AskSummary {
    pub subject: AskSubject,
    /// Asks open, the head included.
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum AskSubject {
    Command {
        command: String,
    },
    Edit {
        path: String,
        files: u32,
        /// As the ask's card has it: None for a write that may do either.
        created: Option<bool>,
    },
    Tool {
        server: String,
        tool: String,
    },
    /// The first question, and how many there are.
    Question {
        question: String,
        count: u32,
    },
    Plan,
    Form {
        server: String,
        message: String,
    },
    Link {
        server: String,
        message: String,
    },
    Access {
        reason: String,
    },
    Unanswerable {
        reason: String,
    },
}

/// The activity line, with the subject of the step it names when the step
/// is a held row: the command, the file, the query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ActivityLine {
    pub activity: Activity,
    pub step: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum StuckReason {
    /// The provider is signed out, its sign-in expired, or signing in failed.
    SignedOut { state: SignInState, account: String },
    /// A usage window is spent; it resets at the latest reset of the
    /// windows that are, when the provider says.
    UsageLimit { resets_at_ms: Option<i64> },
}

/// Why an agent ended, as far as a row distinguishes: it said it was done,
/// it was stopped or exited cleanly, or it failed with a cause.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ExitCause {
    Finished,
    Ended,
    Failed(String),
}

/// Causes that are an ordinary end, not a failure.
const CLEAN_EXITS: [&str; 4] = ["stopped", "exited", "aborted", "killed"];

/// What one agent's session knows for its row. Read from the session alone,
/// so a client never holds the fleet and a session at once; [`fleet_view`]
/// picks from it by the state the inventory row says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionLine {
    pub ask: Option<AskSummary>,
    pub step: Option<ActivityLine>,
    pub last_said: Option<String>,
    pub stuck: Option<StuckReason>,
}

pub fn session_line(state: &SessionState, now_ms: i64) -> SessionLine {
    let agent = state.agent_state();
    let stuck = match agent.sign_in.state() {
        SignInState::SignedOut | SignInState::Expired | SignInState::Failed => {
            Some(StuckReason::SignedOut {
                state: agent.sign_in.state(),
                account: agent.sign_in.account.clone(),
            })
        }
        SignInState::Unknown | SignInState::SignedIn => None,
    }
    .or_else(|| {
        (agent.usage.state() == UsageState::Blocked).then(|| StuckReason::UsageLimit {
            resets_at_ms: crate::usage_windows(&agent.usage)
                .iter()
                .filter(|window| window.state == UsageState::Blocked)
                .filter_map(|window| window.resets_at_ms)
                .max(),
        })
    });
    SessionLine {
        ask: ask_card(state).map(|card| AskSummary {
            subject: subject(card.body),
            count: card.count as u32,
        }),
        step: state.activity(now_ms).map(|activity| ActivityLine {
            step: step_subject(state, &activity),
            activity,
        }),
        last_said: last_said(state),
        stuck,
    }
}

fn subject(body: AskBody) -> AskSubject {
    match body {
        AskBody::Command { command, .. } => AskSubject::Command { command },
        AskBody::Edit {
            path,
            files,
            created,
            ..
        } => AskSubject::Edit {
            path,
            files,
            created,
        },
        AskBody::Tool { server, tool, .. } => AskSubject::Tool { server, tool },
        AskBody::Question(questions) => AskSubject::Question {
            count: questions.len() as u32,
            question: questions
                .into_iter()
                .next()
                .map(|first| first.question)
                .unwrap_or_default(),
        },
        AskBody::Plan { .. } => AskSubject::Plan,
        AskBody::Form {
            server, message, ..
        } => AskSubject::Form { server, message },
        AskBody::Link {
            server, message, ..
        } => AskSubject::Link { server, message },
        AskBody::Access { reason, .. } => AskSubject::Access { reason },
        AskBody::Unanswerable { reason } => AskSubject::Unanswerable { reason },
    }
}

/// The subject of the step a running activity names: its row's, or for a
/// call announced before its row landed, the tool's name.
fn step_subject(state: &SessionState, activity: &Activity) -> Option<String> {
    let ActivityKind::Running { key } = &activity.kind else {
        return None;
    };
    let subject = match state.transcript().get(key) {
        Some(held) => crate::rows::subject_of(held),
        None => state
            .agent_state()
            .running_calls
            .iter()
            .find(|call| call.tool_use_id == *key)
            .map(|call| call.tool_name.clone())
            .unwrap_or_default(),
    };
    (!subject.is_empty()).then_some(subject)
}

/// The first line of the newest message the agent wrote to the person.
fn last_said(state: &SessionState) -> Option<String> {
    use wire::claude_pty_item::Kind as Pty;
    use wire::claude_sdk_item::Kind as Sdk;
    use wire::codex_item::Kind as Codex;
    state
        .transcript()
        .iter()
        .rev()
        .filter(|held| {
            matches!(
                held.body,
                ItemBody::ClaudePty(Pty::Message(_))
                    | ItemBody::ClaudeSdk(Sdk::Message(_))
                    | ItemBody::Codex(Codex::Message(_))
            )
        })
        .find_map(|held| {
            held.item
                .text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
}

fn exit_cause(cause: Option<&str>) -> ExitCause {
    match cause.filter(|cause| !cause.is_empty()) {
        Some("finished") => ExitCause::Finished,
        None => ExitCause::Ended,
        Some(cause) if CLEAN_EXITS.contains(&cause) => ExitCause::Ended,
        Some(cause) => ExitCause::Failed(cause.to_owned()),
    }
}

/// A live agent's host is out of reach from here: offline, connecting, or no
/// longer trusting this machine.
fn host_away(fleet: &FleetState, agent: &Agent) -> bool {
    fleet
        .host(&agent.host_id)
        .is_some_and(|host| host.presence() != Presence::Online || host.revoked == Some(true))
}

/// The row's second line: the state comes from the inventory row, so it
/// agrees with the section; the words come from the agent's session.
fn second_line(fleet: &FleetState, agent: &Agent, line: Option<&SessionLine>) -> SecondLine {
    let attention = ui_state::attention(agent);
    if attention == Attention::Exited {
        return SecondLine::Exited(exit_cause(agent.exit_cause.as_deref()));
    }
    if host_away(fleet, agent) {
        return SecondLine::HostAway;
    }
    let Some(line) = line else {
        return SecondLine::Blank;
    };
    let said = match attention {
        Attention::NeedsYou => line.ask.clone().map(SecondLine::Ask),
        Attention::Working => line.step.clone().map(SecondLine::Step),
        Attention::Idle => line
            .stuck
            .clone()
            .map(SecondLine::Stuck)
            .or_else(|| line.last_said.clone().map(SecondLine::LastSaid)),
        Attention::Starting | Attention::Exited => None,
    };
    said.unwrap_or(SecondLine::Blank)
}

fn section_of(attention: Attention) -> SectionKind {
    match attention {
        Attention::NeedsYou => SectionKind::NeedsYou,
        Attention::Exited => SectionKind::Exited,
        Attention::Idle | Attention::Starting | Attention::Working => SectionKind::Live,
    }
}

fn card(fleet: &FleetState, agent: &Agent) -> FleetCard {
    let at = ui_state::agent_key(agent);
    let host = fleet.host(&agent.host_id);
    let family = fleet.family(&at);
    FleetCard {
        name: agent.name.clone(),
        kind: agent.kind(),
        attention: ui_state::attention(agent),
        exit_cause: agent.exit_cause.clone(),
        branch: agent.git.as_ref().and_then(|git| git.branch.clone()),
        cwd: agent.cwd.clone(),
        phase_since_ms: agent.phase_since_ms,
        host: host.map(|host| host.name.clone()).unwrap_or_default(),
        host_presence: host.map_or(Presence::Unspecified, |host| host.presence()),
        children: fleet.families().children(&at).count() as u32,
        family_attention: fleet.family_attention(&at).unwrap_or(Attention::Exited),
        members: family.len() as u32,
        members_need_you: family
            .iter()
            .filter(|member| ui_state::attention(member) == Attention::NeedsYou)
            .count() as u32,
        agent: at,
    }
}

/// Home: every family in the section of its loudest member, newest
/// since-when first, with the members of expanded families (by root agent
/// id in `expand`) under their parents, newest first. A family stays when
/// `keep` holds for any member, so a filter finds folded members too.
/// `lines` holds what each session knows; an agent without one says only
/// what its inventory row does.
///
/// Since-when moves only when an agent's state does, so streaming never
/// reorders rows.
pub fn fleet_view(
    fleet: &FleetState,
    lines: &HashMap<AgentKey, SessionLine>,
    expand: &HashSet<Vec<u8>>,
    keep: &dyn Fn(&Agent) -> bool,
) -> FleetView {
    let mut families: Vec<(SectionKind, i64, &Agent)> = fleet
        .roots()
        .filter_map(|root| {
            let members = fleet.family(&ui_state::agent_key(root));
            if !members.iter().any(|member| keep(member)) {
                return None;
            }
            let section = section_of(members.iter().map(|m| ui_state::attention(m)).max()?);
            // Ordered by when the members that place it there last changed.
            let since = members
                .iter()
                .filter(|member| section_of(ui_state::attention(member)) == section)
                .map(|member| member.phase_since_ms)
                .max()?;
            Some((section, since, root))
        })
        .collect();
    families.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| ui_state::agent_key(a.2).cmp(&ui_state::agent_key(b.2)))
    });
    let sections = [
        SectionKind::NeedsYou,
        SectionKind::Live,
        SectionKind::Exited,
    ]
    .into_iter()
    .filter_map(|kind| {
        let heads: Vec<&Agent> = families
            .iter()
            .filter(|family| family.0 == kind)
            .map(|family| family.2)
            .collect();
        if heads.is_empty() {
            return None;
        }
        let mut rows = Vec::new();
        for head in &heads {
            push(fleet, lines, head, 0, expand, &mut rows);
        }
        Some(FleetSection {
            kind,
            families: heads.len() as u32,
            rows,
        })
    })
    .collect();
    FleetView { sections }
}

fn push(
    fleet: &FleetState,
    lines: &HashMap<AgentKey, SessionLine>,
    agent: &Agent,
    depth: u32,
    expand: &HashSet<Vec<u8>>,
    rows: &mut Vec<FleetRow>,
) {
    let card_ = card(fleet, agent);
    let mut children: Vec<&Agent> = fleet
        .families()
        .children(&card_.agent)
        .filter_map(|child| fleet.agent(child))
        .collect();
    children.sort_by(|a, b| {
        b.phase_since_ms
            .cmp(&a.phase_since_ms)
            .then_with(|| ui_state::agent_key(a).cmp(&ui_state::agent_key(b)))
    });
    let expanded = !children.is_empty() && expand.contains(&card_.agent.agent);
    let loud = (!children.is_empty() && !expanded && card_.attention != Attention::NeedsYou)
        .then(|| {
            fleet
                .family(&card_.agent)
                .into_iter()
                .filter(|member| ui_state::attention(member) == Attention::NeedsYou)
                .max_by(|a, b| {
                    a.phase_since_ms
                        .cmp(&b.phase_since_ms)
                        .then_with(|| ui_state::agent_key(b).cmp(&ui_state::agent_key(a)))
                })
                .map(|member| {
                    let key = ui_state::agent_key(member);
                    LoudMember {
                        name: member.name.clone(),
                        second_line: second_line(fleet, member, lines.get(&key)),
                        agent: key,
                    }
                })
        })
        .flatten();
    rows.push(FleetRow {
        second_line: second_line(fleet, agent, lines.get(&card_.agent)),
        card: card_,
        depth,
        expanded,
        loud,
    });
    if expanded {
        for child in children {
            push(fleet, lines, child, depth + 1, expand, rows);
        }
    }
}

/// Why a host is out of reach from here, as far as this machine can say.
/// A powered-off host and a signed-out machine look the same from here, so
/// the cause is only ever a fact about this machine, never a claim about
/// the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Away {
    /// Nothing more is known than that the host is away.
    #[default]
    Plain,
    /// This machine is signed out of its account, so the relay carries
    /// nothing for it; a host only the relay reaches is away until it signs
    /// in again.
    SignedOut,
    /// The host said it no longer trusts this machine, as it closed its
    /// link or refused a stream; only pairing again brings it back.
    Revoked,
}

/// Whether this machine is signed out of the account its profile is bound
/// to. A profile that was never bound is not signed out.
pub fn signed_out(fleet: &FleetState, local_host: &[u8]) -> bool {
    fleet
        .host(local_host)
        .is_some_and(|entry| entry.signed_in == Some(false))
}

/// Why `host` is away, when it is. What the host itself said comes first.
pub fn away(fleet: &FleetState, local_host: &[u8], host: &[u8]) -> Away {
    if host == local_host {
        return Away::Plain;
    }
    if fleet
        .host(host)
        .is_some_and(|entry| entry.revoked == Some(true))
    {
        Away::Revoked
    } else if signed_out(fleet, local_host) {
        Away::SignedOut
    } else {
        Away::Plain
    }
}

pub fn fleet_card(fleet: &FleetState, agent_id: &[u8]) -> Option<FleetCard> {
    fleet.find(agent_id).map(|agent| card(fleet, agent))
}

/// A chat's family: its parent, its children, and how loud the family is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FamilyHeader {
    pub parent: Option<FleetCard>,
    pub children: Vec<FleetCard>,
    pub attention: Attention,
}

/// None for an agent with no parent and no children.
pub fn family_header(fleet: &FleetState, agent_id: &[u8]) -> Option<FamilyHeader> {
    let agent = fleet.find(agent_id)?;
    let at = ui_state::agent_key(agent);
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

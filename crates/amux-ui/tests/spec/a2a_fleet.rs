//! Chapter 19 — Families: parent edges folded into fleet rows.
//!
//! An agent that spawned others is not several rows. It is one row that
//! knows how many agents it stands for and how loudly any of them is
//! asking for you, and it carries its descendants so a renderer that
//! expands it needs no second derivation of its own.

use amux_ui::{Agent, Attention, FleetItem, Model, Msg, StreamMsg, Why};

use crate::harness::*;

/// The agent, re-parented onto another fixture agent.
fn child_of(agent: Agent, parent: &str, host: &str) -> Agent {
    Agent {
        parent: Some(amux_ui::AgentParent {
            agent_id: agent_id(parent),
            host_id: host_id(host),
        }),
        ..agent
    }
}

fn base() -> Vec<Msg> {
    seq([vec![connected("nova"), host_up(&a_host("nova"))], synced()])
}

/// One parent with two children on the same host.
fn family_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(a_codex_agent("scribe", "nova"), "lead", "nova")),
            agent_up(&child_of(an_agent("tester", "nova"), "lead", "nova")),
        ],
    ])
}

/// A three-generation family: the grandchild belongs to the same row.
fn grandchild_sequence() -> Vec<Msg> {
    seq([
        family_sequence(),
        vec![agent_up(&child_of(
            an_agent("intern", "nova"),
            "scribe",
            "nova",
        ))],
    ])
}

/// A child whose parent lives on a host this inventory cannot see.
fn absent_parent_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![agent_up(&child_of(
            an_agent("stranded", "nova"),
            "never-listed",
            "hetzner",
        ))],
    ])
}

/// A family beside a lone agent, with the child the one that needs you.
fn ranked_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![
            agent_up(&an_agent("solo", "nova")),
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(an_agent("tester", "nova"), "lead", "nova")),
        ],
        // Only the child opens a stream and asks a question.
        vec![
            stream("tester", StreamMsg::Opened { truncated: false }),
            stream("tester", StreamMsg::ReplayComplete),
        ],
        vec![batch(
            "tester",
            10,
            chat_rows("question_single")[..8].to_vec(),
        )],
    ])
}

/// Two agents naming each other as parent: inventory that cannot be a
/// forest.
fn cycle_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![
            agent_up(&child_of(an_agent("ouro", "nova"), "boros", "nova")),
            agent_up(&child_of(an_agent("boros", "nova"), "ouro", "nova")),
        ],
    ])
}

fn family<'m>(
    model: &'m Model,
    parent: &str,
) -> (
    &'m amux_ui::AgentCard,
    Vec<&'m amux_ui::AgentCard>,
    usize,
    Attention,
) {
    model
        .fleet()
        .into_iter()
        .find_map(|item| match item {
            FleetItem::Family {
                parent: card,
                children,
                child_count,
                highest_attention,
            } if card.agent.id == agent_id(parent) => Some((
                card,
                children.iter().map(|member| member.card).collect(),
                child_count,
                highest_attention,
            )),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{parent} heads a family row"))
}

/// A parent and its children occupy ONE fleet row. The children are not
/// separate top-level rows — collapsed, they are a count.
#[test]
fn a2a_fleet_folds_a_parent_and_its_children_into_one_row() {
    let model = fold(family_sequence());
    let fleet = model.fleet();
    assert_eq!(fleet.len(), 1, "three agents, one family row: {fleet:?}");
    let (parent, children, child_count, _) = family(&model, "lead");
    assert_eq!(parent.display_name(), "lead");
    assert_eq!(child_count, 2);
    let names: Vec<String> = children.iter().map(|card| card.display_name()).collect();
    assert_eq!(
        names,
        vec!["tester".to_string(), "scribe".to_string()],
        "siblings tie on attention and recency, so the id breaks the tie"
    );
    assert_eq!(
        model.fleet_agent_count(),
        3,
        "the fleet still counts every agent it holds"
    );
}

/// The card states the edge as the wire gave it, and the Model answers the
/// same question without walking the fleet.
#[test]
fn a2a_fleet_card_states_its_parent_edge() {
    let model = fold(family_sequence());
    let child = model.agent(agent_id("scribe")).expect("card exists");
    let parent = child.parent().expect("the child knows its parent");
    assert_eq!(parent.agent_id, agent_id("lead"));
    assert_eq!(parent.host_id, host_id("nova"));
    assert_eq!(model.agent(agent_id("lead")).unwrap().parent(), None);

    let descendants: Vec<String> = model
        .family_of(agent_id("lead"))
        .iter()
        .map(|member| member.card.display_name())
        .collect();
    assert_eq!(
        descendants,
        vec!["tester".to_string(), "scribe".to_string()]
    );
    assert!(model.family_of(agent_id("scribe")).is_empty());
}

/// A grandchild is family too: the row stands for the whole subtree, and
/// each member carries the generations between it and the top row.
#[test]
fn a2a_fleet_counts_the_whole_subtree() {
    let model = fold(grandchild_sequence());
    assert_eq!(model.fleet().len(), 1);
    let (_, _, child_count, _) = family(&model, "lead");
    assert_eq!(child_count, 3, "two children and one grandchild");
    let shape: Vec<(String, usize)> = model
        .family_of(agent_id("lead"))
        .iter()
        .map(|member| (member.card.display_name(), member.depth))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("tester".to_string(), 1),
            ("scribe".to_string(), 1),
            ("intern".to_string(), 2),
        ],
        "depth-first, so a child's own children follow it"
    );
}

/// An edge naming an agent this inventory does not hold leaves the child a
/// row of its own. A family we only half know still shows the half we have.
#[test]
fn a2a_fleet_lists_a_child_whose_parent_is_absent() {
    let model = fold(absent_parent_sequence());
    let fleet = model.fleet();
    assert!(
        matches!(fleet.as_slice(), [FleetItem::Agent(card)] if card.display_name() == "stranded"),
        "an unreachable parent must not hide the child: {fleet:?}"
    );
}

/// The family is ranked as a unit on its loudest member: an idle parent
/// rises above a lone agent because its child is asking a question.
#[test]
fn a2a_fleet_ranks_a_family_by_its_loudest_member() {
    let model = fold(ranked_sequence());
    let (_, _, _, highest) = family(&model, "lead");
    assert_eq!(
        model.agent(agent_id("tester")).unwrap().attention,
        Attention::NeedsYou { why: Why::Question }
    );
    assert_eq!(highest, Attention::NeedsYou { why: Why::Question });
    let heads: Vec<String> = model
        .fleet()
        .iter()
        .map(|item| match item {
            FleetItem::Agent(card) => card.display_name(),
            FleetItem::Family { parent, .. } => parent.display_name(),
            FleetItem::PendingCreate { name, .. } => (*name).to_string(),
        })
        .collect();
    assert_eq!(
        heads,
        vec!["lead".to_string(), "solo".to_string()],
        "the family outranks the lone agent on its child's ask"
    );
}

/// Nothing known needs you, but a member sits on an offline host: the
/// family reports what it does not know rather than claiming idle.
#[test]
fn a2a_fleet_family_attention_degrades_to_unknown() {
    let model = fold(seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            host_up(&an_offline_host("hetzner")),
        ],
        synced(),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(an_agent("remote", "hetzner"), "lead", "nova")),
        ],
    ]));
    let (parent, _, _, highest) = family(&model, "lead");
    assert_eq!(
        model.effective_attention(parent),
        Attention::Unknown,
        "no stream folded yet, so the parent itself is Unknown"
    );
    assert_eq!(highest, Attention::Unknown);
}

/// Parent edges that loop belong to no family. The agents are real, so
/// each still gets a row — and the Model says its structural index broke.
#[test]
fn a2a_fleet_survives_a_looping_parent_edge() {
    let model = fold(cycle_sequence());
    let fleet = model.fleet();
    assert_eq!(fleet.len(), 2, "both agents stay reachable: {fleet:?}");
    assert!(
        fleet.iter().all(|item| matches!(item, FleetItem::Agent(_))),
        "neither may claim to head a family: {fleet:?}"
    );
    let violations = model.check_invariants();
    assert_eq!(
        violations.len(),
        2,
        "each stranded agent is named: {violations:?}"
    );
    assert!(
        violations
            .iter()
            .all(|violation| violation.kind() == "parent-cycle"),
        "{violations:?}"
    );
}

/// A parentless fleet is unchanged: every agent is its own row, ranked as
/// before.
#[test]
fn a2a_fleet_without_parents_is_a_flat_list() {
    let model = fold(seq([
        base(),
        vec![
            agent_up(&an_agent("one", "nova")),
            agent_up(&a_codex_agent("two", "nova")),
        ],
    ]));
    let fleet = model.fleet();
    assert_eq!(fleet.len(), 2);
    assert!(fleet.iter().all(|item| matches!(item, FleetItem::Agent(_))));
    assert!(model.check_invariants().is_empty());
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("a2a_fleet::family", family_sequence()),
        ("a2a_fleet::grandchild", grandchild_sequence()),
        ("a2a_fleet::absent_parent", absent_parent_sequence()),
        ("a2a_fleet::ranked", ranked_sequence()),
    ]
}

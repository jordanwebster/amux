//! One fleet, two kinds of Claude session.
//!
//! A Claude session reached over a terminal and one reached over
//! stream-JSON are the same agent to whoever is looking at the fleet. The
//! ranking key is attention, then recency, then identity — and nothing
//! else. These chapters prove it by observation rather than by reading
//! the sort: build a fleet, swap which agent is driven which way, and
//! watch the order stay exactly where it was.

use amux_ui::{Agent, Attention, FleetItem, Model, Msg, StreamMsg, Why};
use serde_json::{Value, json};

use crate::harness::*;

/// The same agent, reached over stream-JSON. Only the kind changes: the
/// command it was started with, its host and its name are the agent's own
/// facts, not the machinery's.
fn a_session_agent(name: &str, on: &str) -> Agent {
    Agent {
        kind: amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Sdk,
        },
        ..an_agent(name, on)
    }
}

fn re_parented(agent: Agent, parent: &str, host: &str) -> Agent {
    Agent {
        parent: Some(amux_ui::AgentParent {
            agent_id: agent_id(parent),
            host_id: host_id(host),
        }),
        ..agent
    }
}

// --- the two vocabularies for the same three moments ----------------------

fn terminal_rows(moment: Moment) -> Vec<Value> {
    let ready = json!({"type": "amux.transcript_ready"});
    let prompt = json!({
        "type": "user",
        "uuid": "dddddddd-0000-4000-8000-000000000001",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "timestamp": "2026-08-11T22:00:00.000Z",
        "message": {"role": "user", "content": "do the thing"},
        "origin": {"kind": "human"},
        "promptSource": "typed",
    });
    match moment {
        Moment::Working => vec![ready, prompt],
        Moment::Permission => vec![
            ready,
            prompt,
            json!({
                "type": "hook.permission_request",
                "tool_name": "Bash",
                "tool_input": {"command": "echo probe"},
            }),
        ],
        Moment::Question => vec![
            ready,
            prompt,
            json!({
                "type": "hook.permission_request",
                "tool_name": "AskUserQuestion",
                "tool_input": {"questions": []},
            }),
        ],
    }
}

fn session_rows(moment: Moment) -> Vec<Value> {
    let ready = json!({
        "type": "amux.claude_sdk.ready",
        "session_id": "33333333-3333-4333-8333-333333333333",
        "resumed": false,
    });
    let prompt = json!({
        "type": "user",
        "sessionId": "33333333-3333-4333-8333-333333333333",
        "parent_tool_use_id": null,
        "message": {"role": "user", "content": "do the thing"},
    });
    match moment {
        Moment::Working => vec![ready, prompt],
        Moment::Permission => vec![
            ready,
            prompt,
            json!({
                "type": "amux.claude_sdk.permission_required",
                "request_id": "permission-1",
                "tool_name": "Bash",
                "input": {"command": "echo probe"},
                "suggestions": [],
            }),
        ],
        Moment::Question => vec![
            ready,
            prompt,
            json!({
                "type": "amux.claude_sdk.dialog_required",
                "request_id": "dialog-1",
                "dialog_kind": "choose",
                "payload": {"message": "which one?"},
            }),
        ],
    }
}

/// What an agent is stopped on, said once for both vocabularies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moment {
    Working,
    Permission,
    Question,
}

/// How an agent is driven. The fleet is never told; these chapters are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Driven {
    OverATerminal,
    OverStreamJson,
}

impl Driven {
    fn agent(self, name: &str) -> Agent {
        match self {
            Self::OverATerminal => an_agent(name, "nova"),
            Self::OverStreamJson => a_session_agent(name, "nova"),
        }
    }

    fn rows(self, moment: Moment) -> Vec<Value> {
        match self {
            Self::OverATerminal => terminal_rows(moment),
            Self::OverStreamJson => session_rows(moment),
        }
    }

    fn flipped(self) -> Self {
        match self {
            Self::OverATerminal => Self::OverStreamJson,
            Self::OverStreamJson => Self::OverATerminal,
        }
    }
}

/// One agent, driven the named way, stopped on the named moment, whose
/// last activity is `at_seconds`.
fn member(name: &str, driven: Driven, moment: Moment, at_seconds: i64) -> Vec<Msg> {
    seq([
        vec![agent_up(&driven.agent(name))],
        vec![
            stream(name, StreamMsg::Opened { truncated: false }),
            stream(name, StreamMsg::ReplayComplete),
            batch(name, at_seconds, driven.rows(moment)),
        ],
    ])
}

fn base() -> Vec<Msg> {
    seq([vec![connected("nova"), host_up(&a_host("nova"))], synced()])
}

/// The fleet these chapters rank: four Claude agents in two pairs, each
/// pair stopped on the same thing, one member of each driven each way.
/// `flip` swaps which member is which, changing nothing else.
fn mixed_sequence(flip: bool) -> Vec<Msg> {
    let first = match flip {
        false => Driven::OverATerminal,
        true => Driven::OverStreamJson,
    };
    seq([
        base(),
        member("fix-auth", first, Moment::Permission, 40),
        member("fix-sync", first.flipped(), Moment::Permission, 30),
        member("docs-auth", first, Moment::Working, 20),
        member("docs-sync", first.flipped(), Moment::Working, 10),
    ])
}

/// The same four agents as one family: an agent driven one way parents an
/// agent driven the other, in both directions at once.
fn mixed_family_sequence(flip: bool) -> Vec<Msg> {
    let first = match flip {
        false => Driven::OverATerminal,
        true => Driven::OverStreamJson,
    };
    seq([
        base(),
        member("lead", first, Moment::Working, 40),
        member("scribe", first.flipped(), Moment::Working, 30),
        // The edges arrive after the agents, as they do on the wire.
        vec![
            agent_up(&re_parented(
                first.flipped().agent("scribe"),
                "lead",
                "nova",
            )),
            agent_up(&re_parented(first.agent("runner"), "lead", "nova")),
        ],
        vec![
            stream("runner", StreamMsg::Opened { truncated: false }),
            stream("runner", StreamMsg::ReplayComplete),
            batch("runner", 50, first.rows(Moment::Question)),
        ],
    ])
}

fn ranked_names(model: &Model) -> Vec<String> {
    model
        .fleet()
        .into_iter()
        .map(|item| match item {
            FleetItem::Agent(card) => card.display_name(),
            FleetItem::Family { parent, .. } => parent.display_name(),
            FleetItem::PendingCreate { name, .. } => name.to_string(),
        })
        .collect()
}

fn attention_of(model: &Model, name: &str) -> Attention {
    let card = model.agent(agent_id(name)).expect("a card for the agent");
    model.effective_attention(card)
}

// --- the chapters ---------------------------------------------------------

/// Both vocabularies summarize to the same attention. Everything below
/// rests on this: if a permission request over stream-JSON did not read
/// as `NeedsYou { Permission }`, the rows would differ for an honest
/// reason and there would be nothing to prove.
#[test]
fn fleet_mixed_reaches_the_same_attention_through_either_vocabulary() {
    let model = fold(mixed_sequence(false));
    for name in ["fix-auth", "fix-sync"] {
        assert_eq!(
            attention_of(&model, name),
            Attention::NeedsYou {
                why: Why::Permission
            },
            "{name} is stopped on a permission request"
        );
    }
    for name in ["docs-auth", "docs-sync"] {
        assert_eq!(
            attention_of(&model, name),
            Attention::Working,
            "{name} is mid-turn"
        );
    }
}

/// The ranking key holds attention, then recency, then identity. Swap
/// which agent is driven which way and the order does not move: the
/// driver is not in the key, and no tie is broken by it.
#[test]
fn fleet_mixed_ranks_and_sorts_by_the_same_rules_for_both() {
    let expected = vec![
        "fix-auth".to_string(),
        "fix-sync".to_string(),
        "docs-auth".to_string(),
        "docs-sync".to_string(),
    ];
    for flip in [false, true] {
        let model = fold(mixed_sequence(flip));
        assert_eq!(
            ranked_names(&model),
            expected,
            "asking for you first, then the newer of each pair (flipped: {flip})"
        );
    }
}

/// Recency still decides within an attention band, whichever way each
/// agent is driven: make the second of a pair the newer one and it
/// overtakes the first, in both arrangements.
#[test]
fn fleet_mixed_lets_recency_decide_inside_a_band_for_both() {
    for flip in [false, true] {
        let first = match flip {
            false => Driven::OverATerminal,
            true => Driven::OverStreamJson,
        };
        let model = fold(seq([
            base(),
            member("fix-auth", first, Moment::Permission, 30),
            member("fix-sync", first.flipped(), Moment::Permission, 40),
        ]));
        assert_eq!(
            ranked_names(&model),
            vec!["fix-sync".to_string(), "fix-auth".to_string()],
            "the newer ask is on top (flipped: {flip})"
        );
    }
}

/// A family is one row whichever way its members are driven: the parent
/// carries its whole subtree across the two vocabularies, and the row is
/// ranked on its loudest member no matter which member that is.
#[test]
fn fleet_mixed_groups_a_family_across_both() {
    for flip in [false, true] {
        let model = fold(mixed_family_sequence(flip));
        let fleet = model.fleet();
        assert_eq!(
            fleet.len(),
            1,
            "three agents, one family row (flipped: {flip}): {fleet:?}"
        );
        let FleetItem::Family {
            parent,
            children,
            child_count,
            highest_attention,
        } = &fleet[0]
        else {
            panic!("the parent heads a family row");
        };
        assert_eq!(parent.display_name(), "lead");
        assert_eq!(*child_count, 2);
        let names: Vec<String> = children
            .iter()
            .map(|member| member.card.display_name())
            .collect();
        assert_eq!(
            names,
            vec!["runner".to_string(), "scribe".to_string()],
            "the child that needs you is first among its siblings (flipped: {flip})"
        );
        assert_eq!(
            *highest_attention,
            Attention::NeedsYou { why: Why::Question },
            "the family is as loud as its loudest member (flipped: {flip})"
        );
        assert_eq!(model.fleet_agent_count(), 3);
    }
}

/// A family ranked against a lone agent behaves the same both ways: the
/// question inside the family lifts the whole row above an agent that is
/// merely working, no matter which member raised it.
#[test]
fn fleet_mixed_ranks_a_family_against_a_lone_agent_the_same_way() {
    for flip in [false, true] {
        let first = match flip {
            false => Driven::OverATerminal,
            true => Driven::OverStreamJson,
        };
        let model = fold(seq([
            mixed_family_sequence(flip),
            member("solo", first.flipped(), Moment::Working, 60),
        ]));
        assert_eq!(
            ranked_names(&model),
            vec!["lead".to_string(), "solo".to_string()],
            "the newer lone agent still ranks under a family that needs you (flipped: {flip})"
        );
    }
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("fleet_mixed::ranked", mixed_sequence(false)),
        ("fleet_mixed::ranked_flipped", mixed_sequence(true)),
        ("fleet_mixed::family", mixed_family_sequence(false)),
        ("fleet_mixed::family_flipped", mixed_family_sequence(true)),
    ]
}

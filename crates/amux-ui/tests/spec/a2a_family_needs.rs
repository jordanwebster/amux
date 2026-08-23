//! Chapter 23 — A child's ask, composed into its parent.
//!
//! A parent whose child is blocked has to be able to say so, and the child
//! is the only one who knows what it is blocked on. So nothing is written
//! into the parent: the parent's chat asks the Model which of its family
//! needs the human, gets back the addresses, and draws each ask out of the
//! child's own layer. Composition, not synthesis — which means the ask
//! leaves the parent's chat the moment the child stops asking, however it
//! stopped, with nothing to clear.

use amux_ui::{Agent, Attention, Model, Msg, StreamMsg, Why};
use serde_json::{Value, json};

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

/// The stream rows a Codex agent produces while waiting on approval to run
/// a command — the shape chapter 15 pins.
fn codex_approval_rows() -> Vec<Value> {
    vec![
        json!({"type":"amux.codex_ready"}),
        json!({"type":"turn/started","turn":{"id":"turn-1","status":"inProgress"}}),
        json!({"type":"item/started","item":{"id":"exec-1","type":"commandExecution",
            "command":"cargo test","cwd":"/work","status":"inProgress"}}),
        json!({"type":"item/commandExecution/requestApproval","itemId":"exec-1",
            "command":"cargo test","cwd":"/work","reason":"run tests?"}),
        json!({"type":"amux.codex_approval_required","request_id":7,
            "availableDecisions":["accept","cancel"]}),
    ]
}

fn opened(agent: &str) -> Vec<Msg> {
    vec![
        stream(agent, StreamMsg::Opened { truncated: false }),
        stream(agent, StreamMsg::ReplayComplete),
    ]
}

/// A lead with two children, one of each kind. The Codex child stops for
/// permission; the Claude child asks a question.
fn asking_family_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(a_codex_agent("scribe", "nova"), "lead", "nova")),
            agent_up(&child_of(an_agent("tester", "nova"), "lead", "nova")),
        ],
        opened("scribe"),
        opened("tester"),
        vec![
            batch("scribe", 10, codex_approval_rows()),
            batch("tester", 20, chat_rows("question_single")[..8].to_vec()),
        ],
    ])
}

/// The same family, with the Codex child's approval answered afterwards.
fn answered_sequence() -> Vec<Msg> {
    seq([
        asking_family_sequence(),
        vec![batch(
            "scribe",
            30,
            vec![json!({"type":"amux.codex_approval_resolved",
                "request_id":7, "resolution":"answered"})],
        )],
    ])
}

/// A three-generation family: the grandchild is the one that stops.
fn grandchild_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(an_agent("scribe", "nova"), "lead", "nova")),
            agent_up(&child_of(a_codex_agent("intern", "nova"), "scribe", "nova")),
        ],
        opened("intern"),
        vec![batch("intern", 10, codex_approval_rows())],
    ])
}

/// A family whose loudest ask is furthest from the top and hidden behind
/// the quietest branch: `reviewer` finished, so it leads its siblings,
/// while `builder` is idle and only its child `intern` is blocked on
/// permission. Walking the tree meets the finished reviewer first; the
/// person needs the permission first.
fn buried_permission_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(an_agent("reviewer", "nova"), "lead", "nova")),
            agent_up(&child_of(an_agent("builder", "nova"), "lead", "nova")),
            agent_up(&child_of(
                a_codex_agent("intern", "nova"),
                "builder",
                "nova",
            )),
        ],
        opened("reviewer"),
        opened("builder"),
        opened("intern"),
        vec![
            batch("reviewer", 10, chat_rows("permission")),
            batch("builder", 20, chat_rows("interrupt")),
            batch("intern", 30, codex_approval_rows()),
        ],
    ])
}

/// A parent with one child, both blocked: the parent on a permission of
/// its own, the child on a question.
fn both_asking_sequence() -> Vec<Msg> {
    seq([
        base(),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(an_agent("tester", "nova"), "lead", "nova")),
        ],
        opened("lead"),
        opened("tester"),
        vec![
            batch("lead", 10, chat_rows("permission")[..8].to_vec()),
            batch("tester", 20, chat_rows("question_single")[..8].to_vec()),
        ],
    ])
}

/// A blocked child on a host that has since gone dark.
fn offline_child_sequence() -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            host_up(&a_host("hetzner")),
        ],
        synced(),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child_of(
                a_codex_agent("remote", "hetzner"),
                "lead",
                "nova",
            )),
        ],
        opened("remote"),
        vec![batch("remote", 10, codex_approval_rows())],
        vec![host_up(&an_offline_host("hetzner"))],
    ])
}

fn needs(model: &Model, parent: &str) -> Vec<(String, usize, Why)> {
    model
        .family_needs(agent_id(parent))
        .into_iter()
        .map(|need| (need.card.display_name(), need.depth, need.why))
        .collect()
}

/// The parent is told which of its family is asking, and for what. The
/// list is ranked by how loudly each one is asking, so the loudest leads.
#[test]
fn a2a_family_needs_names_every_asking_child() {
    let model = fold(asking_family_sequence());
    assert_eq!(
        needs(&model, "lead"),
        vec![
            ("scribe".to_string(), 1, Why::Permission),
            ("tester".to_string(), 1, Why::Question),
        ],
        "permission outranks question, as it does in the fleet"
    );
}

/// Loudness is the order, and it is measured over the whole family rather
/// than branch by branch. A permission two generations down outranks a
/// sibling that merely finished, however the tree happens to be walked —
/// otherwise the one consumer that shows a single need and counts the
/// rest would name the finished agent and leave the blocked one waiting.
#[test]
fn a2a_family_needs_puts_the_loudest_first_wherever_it_sits() {
    let model = fold(buried_permission_sequence());
    assert_eq!(
        model.effective_attention(model.agent(agent_id("builder")).expect("card")),
        Attention::Idle,
        "the branch hiding the permission is the quietest one on the fleet"
    );
    assert_eq!(
        model
            .family_of(agent_id("lead"))
            .into_iter()
            .map(|member| member.card.display_name())
            .collect::<Vec<_>>(),
        vec![
            "reviewer".to_string(),
            "builder".to_string(),
            "intern".to_string()
        ],
        "the family itself still reads as a tree: parents before their children"
    );
    assert_eq!(
        needs(&model, "lead"),
        vec![
            ("intern".to_string(), 2, Why::Permission),
            ("reviewer".to_string(), 1, Why::Finished),
        ],
        "but what needs a person is ranked by how much it needs one"
    );
}

/// What travels is an address, not a copy: the child's id and the layer
/// that knows how to draw its ask. The parent's chat renders the child's
/// own panel from them, and an answer is a command to the child.
#[test]
fn a2a_family_needs_addresses_the_child_not_its_ask() {
    let model = fold(asking_family_sequence());
    let asks = model.family_needs(agent_id("lead"));
    let codex_child = asks.first().expect("the codex child is asking");
    assert_eq!(codex_child.agent(), agent_id("scribe"));
    assert_eq!(
        codex_child.layer(),
        Some(amux_ui::StructuredProtocol::Codex)
    );
    let ask = model
        .codex(codex_child.agent())
        .and_then(|layer| layer.ask_head())
        .expect("the ask stayed in the child's own layer");
    assert_eq!(ask.request_id, json!(7));

    let claude_child = asks.get(1).expect("the claude child is asking");
    assert_eq!(claude_child.agent(), agent_id("tester"));
    assert_eq!(
        claude_child.layer(),
        Some(amux_ui::StructuredProtocol::Claude)
    );
    assert!(
        model
            .claude(claude_child.agent())
            .and_then(|layer| layer.ask_head())
            .is_some()
    );
}

/// Answering in the child's own view empties the parent's list. Nothing
/// was stored, so nothing has to be cleared — the derivation simply stops
/// finding an ask.
#[test]
fn a2a_family_needs_clears_when_the_child_clears() {
    let model = fold(answered_sequence());
    assert_eq!(
        model.agent(agent_id("scribe")).unwrap().attention,
        Attention::Working,
        "the child went back to work"
    );
    assert_eq!(
        needs(&model, "lead"),
        vec![("tester".to_string(), 1, Why::Question)],
        "only the child that is still asking remains"
    );
}

/// A grandchild is family: its ask reaches the top of the family it
/// belongs to, wearing the distance it travelled.
#[test]
fn a2a_family_needs_reaches_a_grandchild() {
    let model = fold(grandchild_sequence());
    assert_eq!(
        needs(&model, "lead"),
        vec![("intern".to_string(), 2, Why::Permission)]
    );
    assert_eq!(
        needs(&model, "scribe"),
        vec![("intern".to_string(), 1, Why::Permission)],
        "the same ask is one generation away from its own parent"
    );
}

/// A parent's own ask is not its family's: its chat is already showing it,
/// and a chat that told you twice about one obligation would be lying
/// about how many there are.
#[test]
fn a2a_family_needs_excludes_the_parent_itself() {
    let model = fold(both_asking_sequence());
    assert_eq!(
        model.agent(agent_id("lead")).unwrap().attention,
        Attention::NeedsYou {
            why: Why::Permission
        }
    );
    assert_eq!(
        needs(&model, "lead"),
        vec![("tester".to_string(), 1, Why::Question)]
    );
    assert!(
        model.family_needs(agent_id("tester")).is_empty(),
        "an agent with no children has no family asking for it"
    );
}

/// The composition raises no new reason. Every ask that surfaces was
/// already the child's own attention, unchanged, and the parent's own
/// attention is untouched by carrying it.
#[test]
fn a2a_family_needs_invents_no_attention() {
    let model = fold(asking_family_sequence());
    let parent = model.agent(agent_id("lead")).expect("card");
    assert_eq!(
        model.effective_attention(parent),
        Attention::Unknown,
        "the parent folded no stream of its own and says so"
    );
    for need in model.family_needs(agent_id("lead")) {
        assert_eq!(
            model.effective_attention(need.card),
            Attention::NeedsYou { why: need.why },
            "{} reports exactly its own attention",
            need.card.display_name()
        );
    }
}

/// A child on a host that went dark holds an ask we can no longer see
/// resolved. The fleet badge degrades to Unknown there, and so does this:
/// a banner offering to answer an ask nobody can confirm is still pending
/// is worse than no banner.
#[test]
fn a2a_family_needs_drops_a_child_we_cannot_see() {
    let model = fold(offline_child_sequence());
    assert!(
        model
            .codex(agent_id("remote"))
            .and_then(|layer| layer.ask_head())
            .is_some(),
        "the fold still holds the last thing the child said"
    );
    assert!(
        needs(&model, "lead").is_empty(),
        "but an unreachable child asks for nobody"
    );
}

/// An agent this inventory does not hold has no family, and neither does
/// one that spawned nobody.
#[test]
fn a2a_family_needs_of_a_childless_agent_is_empty() {
    let model = fold(seq([base(), vec![agent_up(&an_agent("solo", "nova"))]]));
    assert!(model.family_needs(agent_id("solo")).is_empty());
    assert!(model.family_needs(agent_id("never-listed")).is_empty());
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("a2a_family_needs::asking", asking_family_sequence()),
        ("a2a_family_needs::answered", answered_sequence()),
        ("a2a_family_needs::grandchild", grandchild_sequence()),
        ("a2a_family_needs::buried", buried_permission_sequence()),
        ("a2a_family_needs::both", both_asking_sequence()),
        ("a2a_family_needs::offline", offline_child_sequence()),
    ]
}

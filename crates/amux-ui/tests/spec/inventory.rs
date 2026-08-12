//! Chapter 2 — Inventory: idempotent upserts, unknown agent types, display
//! fallback, and the authority rule.
//!
//! Subscriptions are the sole writer of entity state. Deltas are upserts —
//! "the state IS this" — so applying one twice is applying it once.

use amux_ui::{Attention, Msg, OpOutcome, display_name_fallback};

use crate::harness::*;

fn base() -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            host_up(&an_offline_host("hetzner")),
        ],
        synced(),
    ])
}

fn unknown_type_sequence() -> Vec<Msg> {
    let mut exotic = an_agent("mystery", "nova");
    exotic.agent_type = "frobnicator-2000".to_string();
    exotic.io_protocols = Vec::new();
    seq([base(), vec![agent_up(&exotic)]])
}

fn unnamed_agent_sequence() -> Vec<Msg> {
    let mut unnamed = an_agent("unnamed", "nova");
    unnamed.name = None;
    seq([base(), vec![agent_up(&unnamed)]])
}

fn stale_rename_sequence() -> Vec<Msg> {
    let mut renamed = an_agent("fix-auth-bug", "nova");
    renamed.name = Some("renamed-by-server".to_string());
    let mut stale = an_agent("fix-auth-bug", "nova");
    stale.name = Some("stale-rpc-name".to_string());
    seq([
        base(),
        vec![
            agent_up(&an_agent("fix-auth-bug", "nova")),
            command(op(1), rename_cmd("fix-auth-bug", "stale-rpc-name")),
            // The subscription echoes the newer truth first…
            agent_up(&renamed),
            // …then the (slower, stale) RPC response arrives.
            op_result(op(1), OpOutcome::AgentRenamed { agent: stale }),
        ],
    ])
}

/// Applying the same upsert twice yields the same Model as applying it
/// once — deltas are idempotent, last-write-wins, zero domain logic.
#[test]
fn agent_upserts_are_idempotent() {
    let agent = an_agent("fix-auth-bug", "nova");
    let once = fold(seq([base(), vec![agent_up(&agent)]]));
    let twice = fold(seq([base(), vec![agent_up(&agent), agent_up(&agent)]]));
    assert_eq!(once, twice);
    assert_eq!(
        serde_json::to_value(&once).unwrap(),
        serde_json::to_value(&twice).unwrap()
    );
}

/// A client that does not know an agent type degrades to the card: the row
/// renders, attention honestly reports Unknown, nothing panics.
#[test]
fn unknown_agent_type_still_renders_a_card() {
    let model = fold(unknown_type_sequence());
    let card = model.agent(agent_id("mystery")).expect("card exists");
    assert_eq!(card.agent.agent_type, "frobnicator-2000");
    assert_eq!(card.attention, Attention::Unknown);
    assert_eq!(card.display_name(), "mystery");
    assert_eq!(model.status_label_for(card), "–");
}

/// Display naming is a Model derivation, computed once for every renderer:
/// user-assigned name, then adapter-translated provider label, then a short
/// id. (Nothing populates `provider_label` until the naming-translation
/// chunk lands; the fallback chain is exercised directly.)
#[test]
fn display_name_falls_back_name_then_provider_then_id() {
    let named = fold(seq([
        base(),
        vec![agent_up(&an_agent("fix-auth-bug", "nova"))],
    ]));
    assert_eq!(
        named
            .agent(agent_id("fix-auth-bug"))
            .expect("card exists")
            .display_name(),
        "fix-auth-bug"
    );

    let id = agent_id("unnamed");
    assert_eq!(
        display_name_fallback(None, Some("claude-sonnet"), &id),
        "claude-sonnet"
    );

    let unnamed = fold(unnamed_agent_sequence());
    let card = unnamed.agent(id).expect("card exists");
    assert_eq!(
        card.display_name(),
        id.simple().to_string()[..8].to_string()
    );
}

/// Arrival order is not freshness: an RPC result resolves its pending op and
/// nothing else. The subscription's newer name survives the stale response.
#[test]
fn stale_rpc_result_does_not_overwrite_subscription_state() {
    let model = fold(stale_rename_sequence());
    let card = model.agent(agent_id("fix-auth-bug")).expect("card exists");
    assert_eq!(card.agent.name.as_deref(), Some("renamed-by-server"));
    assert!(model.pending_ops().next().is_none(), "op resolved");
    assert!(model.finished_op(op(1)).is_some());
}

fn readonly_sequence() -> Vec<Msg> {
    let mut readonly = an_agent("captured-session", "nova");
    readonly.readonly = true;
    vec![
        connected("nova"),
        host_up(&a_host("nova")),
        hosts_synced(),
        agent_up(&an_agent("fix-auth-bug", "nova")),
        agent_up(&readonly),
        agents_synced(),
    ]
}

/// Readonly agents (captured sessions the chrome cannot drive) are hidden
/// from the fleet — and get no stream subscription, because a badge nobody
/// can see is not worth a stream — until the chat view can render them.
#[test]
fn readonly_agents_are_hidden_from_the_fleet() {
    let (model, effects) = fold_with_effects(readonly_sequence());
    assert_eq!(model.agent_count(), 2, "both entities exist in the Model");
    assert_eq!(model.fleet_agent_count(), 1, "only one is fleet-visible");
    assert_eq!(model.fleet().len(), 1);
    let visible: Vec<_> = model
        .fleet()
        .iter()
        .filter_map(|item| match item {
            amux_ui::FleetItem::Agent(card) => Some(card.display_name()),
            _ => None,
        })
        .collect();
    assert_eq!(visible, ["fix-auth-bug"]);
    let opened = effects
        .iter()
        .filter(|effect| matches!(effect, amux_ui::Effect::OpenStream { .. }))
        .count();
    assert_eq!(opened, 1, "no stream for the hidden readonly agent");
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("inventory::base", base()),
        ("inventory::readonly_hidden", readonly_sequence()),
        ("inventory::unknown_type", unknown_type_sequence()),
        ("inventory::unnamed_agent", unnamed_agent_sequence()),
        ("inventory::stale_rename", stale_rename_sequence()),
    ]
}

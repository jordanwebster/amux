//! Chapter 2 — Inventory: idempotent upserts, kinds without a layer, display
//! fallback, and the authority rule.
//!
//! Subscriptions are the sole writer of entity state. Deltas are upserts —
//! "the state IS this" — so applying one twice is applying it once.

use ui_state::{Attention, Effect, Msg, OpOutcome, StructuredProtocol, display_name_fallback};

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

fn no_structured_layer_sequence() -> Vec<Msg> {
    let mut exotic = an_agent("mystery", "nova");
    exotic.kind = model::AgentKind::TestAgent;
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
fn kind_without_a_structured_layer_still_renders_a_card() {
    let (model, effects) = fold_with_effects(no_structured_layer_sequence());
    let card = model.agent(agent_id("mystery")).expect("card exists");
    assert_eq!(card.agent.kind, model::AgentKind::TestAgent);
    assert_eq!(card.structured_protocol(), None);
    assert_eq!(card.attention, Attention::Unknown);
    assert_eq!(card.display_name(), "mystery");
    assert_eq!(model.status_label_for(card), "–");
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::OpenStream { .. })),
        "an unknown structured protocol must not open a native stream"
    );
}

#[test]
fn known_agents_do_not_open_eager_streams() {
    for (agent, _expected) in [
        (
            an_agent("claude-agent", "nova"),
            StructuredProtocol::ClaudePtyTranscript,
        ),
        (
            a_codex_agent("codex-agent", "nova"),
            StructuredProtocol::Codex,
        ),
    ] {
        let (_, effects) = fold_with_effects(seq([base(), vec![agent_up(&agent)]]));
        assert!(effects.is_empty(), "inventory must not open {agent:?}");
    }
}

/// Opening an SDK conversation requests exactly one typed stream; inventory
/// alone requested none.
#[test]
fn claude_sdk_inventory_opens_one_stream_shared_with_user_attach() {
    let mut sdk = an_agent("sdk-agent", "nova");
    sdk.kind = model::AgentKind::Claude {
        driver: model::ClaudeDriver::Sdk,
    };
    let (model, effects) = fold_with_effects(seq([
        base(),
        vec![agent_up(&sdk)],
        vec![Msg::UserAttached {
            agent: agent_id("sdk-agent"),
        }],
    ]));

    assert_eq!(
        model
            .agent(agent_id("sdk-agent"))
            .expect("SDK card")
            .structured_protocol(),
        Some(StructuredProtocol::ClaudeSdk)
    );
    assert_eq!(
        effects,
        vec![Effect::OpenStream {
            agent: agent_id("sdk-agent"),
            protocol: StructuredProtocol::ClaudeSdk,
            tail: ui_state::REPLAY_TAIL,
        }]
    );
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

/// Readonly agents (captured sessions the chrome cannot drive) surface in
/// the fleet — A3: they exist and open in chat only, with their resting
/// status word stating `read-only` — but still get no eager stream
/// subscription: a resting row is not worth a stream. Opening one subscribes
/// through the store-backed chat lifecycle.
#[test]
fn readonly_agents_surface_in_the_fleet_without_an_eager_stream() {
    let (model, effects) = fold_with_effects(readonly_sequence());
    assert_eq!(model.agent_count(), 2, "both entities exist in the Model");
    assert_eq!(model.fleet_agent_count(), 2, "both are fleet-visible");
    assert_eq!(model.fleet().len(), 2);
    let readonly = model
        .agent(agent_id("captured-session"))
        .expect("card exists");
    assert_eq!(
        model.status_label_for(readonly),
        "read-only",
        "the resting status word states the inventory fact"
    );
    let opened = effects
        .iter()
        .filter(|effect| matches!(effect, ui_state::Effect::OpenStream { .. }))
        .count();
    assert_eq!(opened, 0, "neither fleet row opens a chat stream");
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("inventory::base", base()),
        ("inventory::readonly", readonly_sequence()),
        (
            "inventory::no_structured_layer",
            no_structured_layer_sequence(),
        ),
        ("inventory::unnamed_agent", unnamed_agent_sequence()),
        ("inventory::stale_rename", stale_rename_sequence()),
    ]
}

/// A conversation left open stays subscribed through both a transport close
/// and the temporary disappearance of its host's inventory.
#[test]
fn attached_remote_conversations_rejoin_after_an_outage() {
    use ui_state::{ServerMsg, StreamCloseReason, StreamMsg, update};

    for kind in [
        ui_state::AgentKind::Claude {
            driver: ui_state::ClaudeDriver::Pty,
        },
        ui_state::AgentKind::Claude {
            driver: ui_state::ClaudeDriver::Sdk,
        },
        ui_state::AgentKind::Codex,
    ] {
        for readonly in [false, true] {
            let mut remote = an_agent("open-chat", "hetzner");
            remote.kind = kind;
            remote.readonly = readonly;
            let mut model = fold(seq([
                base(),
                vec![host_up(&a_host("hetzner")), agent_up(&remote)],
            ]));
            let requested = update(&mut model, Msg::UserAttached { agent: remote.id });
            assert_eq!(requested.len(), 1);
            assert!(update(&mut model, agent_up(&remote)).is_empty());

            update(
                &mut model,
                Msg::Stream {
                    agent: remote.id,
                    event: StreamMsg::Closed {
                        reason: StreamCloseReason::TransportError {
                            message: "relay lost".into(),
                        },
                    },
                },
            );
            assert_eq!(update(&mut model, agent_up(&remote)), requested);

            for offline_first in [false, true] {
                let mut outage = vec![
                    Msg::Server(ServerMsg::AgentRemoved { id: remote.id }),
                    host_up(&an_offline_host("hetzner")),
                ];
                if offline_first {
                    outage.reverse();
                }
                for msg in outage {
                    update(&mut model, msg);
                }
                // Recorder checkpoints must remember intent even without a card.
                model = serde_json::from_value(serde_json::to_value(model).unwrap()).unwrap();
                update(&mut model, host_up(&a_host("hetzner")));
                assert_eq!(update(&mut model, agent_up(&remote)), requested);
                assert!(update(&mut model, agent_up(&remote)).is_empty());
            }

            // A confirmed deletion releases the attachment, so an inventory
            // upsert alone cannot subscribe this remote agent again.
            update(
                &mut model,
                Msg::Server(ServerMsg::AgentRemoved { id: remote.id }),
            );
            update(
                &mut model,
                Msg::Server(ServerMsg::HostInventory {
                    host_id: remote.host_id,
                    agent_ids: vec![],
                }),
            );
            assert!(update(&mut model, agent_up(&remote)).is_empty());
        }
    }
}

/// A machine that comes back puts the conversation held open on it back on
/// its transcript, on the strength of the inventory alone.
///
/// The records a returning machine re-states are byte-identical to the ones
/// already cached, so no per-agent event follows its return. Only the
/// inventory says the machine is speaking again — without reading it, a
/// conversation closed by the outage stays closed while its machine reads
/// online, and its agent stays a memory.
#[test]
fn a_returning_host_rejoins_the_conversation_left_open_on_it() {
    use ui_state::{ServerMsg, StreamCloseReason, StreamMsg, update};

    let remote = an_agent("open-chat", "hetzner");
    let mut model = fold(seq([
        base(),
        vec![host_up(&a_host("hetzner")), agent_up(&remote)],
    ]));
    let requested = update(&mut model, Msg::UserAttached { agent: remote.id });
    assert_eq!(requested.len(), 1);
    update(
        &mut model,
        Msg::Stream {
            agent: remote.id,
            event: StreamMsg::Opened { truncated: false },
        },
    );

    // The machine leaves: the link drops the stream and the host row goes
    // offline, but the agent stays as cached inventory.
    update(
        &mut model,
        Msg::Stream {
            agent: remote.id,
            event: StreamMsg::Closed {
                reason: StreamCloseReason::TransportError {
                    message: "relay lost".into(),
                },
            },
        },
    );
    update(&mut model, host_up(&an_offline_host("hetzner")));
    assert!(
        !model.agent(remote.id).expect("cached card").live,
        "an agent on a machine that left is a memory"
    );

    // It comes back. The host row alone is not enough…
    assert!(update(&mut model, host_up(&a_host("hetzner"))).is_empty());
    // …its inventory is.
    assert_eq!(
        update(
            &mut model,
            Msg::Server(ServerMsg::HostInventory {
                host_id: remote.host_id,
                agent_ids: vec![remote.id],
            }),
        ),
        requested,
        "the held-open conversation asks for its stream again"
    );
    assert!(
        model.agent(remote.id).expect("card exists").live,
        "and its agent is answering again"
    );

    // Saying it twice asks once: the stream is already opening.
    assert!(
        update(
            &mut model,
            Msg::Server(ServerMsg::HostInventory {
                host_id: remote.host_id,
                agent_ids: vec![remote.id],
            }),
        )
        .is_empty()
    );
}

/// Closing a conversation gives back the only stream it asked for.
#[test]
fn a_closed_conversation_lets_go_of_the_stream_it_asked_for() {
    use ui_state::{Effect, update};

    for (on, readonly) in [
        ("hetzner", false),
        ("hetzner", true),
        ("nova", false),
        ("nova", true),
    ] {
        let mut agent = an_agent(&format!("chat-{on}-{readonly}"), on);
        agent.readonly = readonly;
        let mut model = fold(seq([base(), vec![host_up(&a_host("hetzner"))]]));

        let inventory = update(&mut model, agent_up(&agent));
        assert_eq!(inventory.len(), 0, "{on} readonly={readonly}");
        update(&mut model, Msg::UserAttached { agent: agent.id });
        assert!(model.is_attached(agent.id));
        assert!(model.stream(agent.id).is_some(), "{on} readonly={readonly}");

        let released = update(&mut model, Msg::UserDetached { agent: agent.id });
        assert!(
            !model.is_attached(agent.id),
            "a closed conversation is not open: {on} readonly={readonly}"
        );
        assert!(
            matches!(released.as_slice(), [Effect::CloseLegacyStream { agent: closed }] if *closed == agent.id),
            "the stream nobody asked for any more stays open: {released:?}"
        );
        assert!(model.stream(agent.id).is_none());
        // The inventory keeps arriving; it must not re-open what was closed.
        assert!(update(&mut model, agent_up(&agent)).is_empty());
        assert!(model.stream(agent.id).is_none());
        // Opening it again is ordinary.
        let reopened = update(&mut model, Msg::UserAttached { agent: agent.id });
        assert_eq!(reopened.len(), 1, "{on} readonly={readonly}");
        assert!(model.is_attached(agent.id));
    }
}

//! Cloud state and host reachability come from their authoritative streams.
//! Account prompts are derived only when an account action can restore a
//! trusted host, and unreachable hosts retain cached non-live agents.

use ui_state::{AccountPrompt, CloudState, HostVia, Msg, RelayCarrier, ServerMsg, Tier, update};

use crate::harness::*;

fn cloud(state: CloudState) -> Msg {
    Msg::Server(ServerMsg::CloudState(state))
}

fn relay_host(name: &str) -> ui_state::HostEntry {
    let mut host = a_host(name);
    host.via = HostVia::Relay;
    host
}

#[test]
fn cloud_state_transitions_follow_the_status_stream_exactly() {
    let states = [
        CloudState::SignedOut,
        CloudState::Connecting,
        CloudState::Retrying,
        CloudState::AuthRequired,
        CloudState::Connected {
            tier: Tier::Free,
            carrier: RelayCarrier::Tcp,
        },
        CloudState::Connected {
            tier: Tier::Pro,
            carrier: RelayCarrier::Quic,
        },
    ];
    let mut model = ui_state::Model::default();
    for state in states {
        update(&mut model, cloud(state.clone()));
        assert_eq!(model.cloud_state(), &state);
    }
}

#[test]
fn cloud_state_away_is_a_free_relay_route_never_pro_or_offline() {
    let host = relay_host("desktop");
    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::Connected {
            tier: Tier::Free,
            carrier: RelayCarrier::Quic,
        }),
        host_up(&host),
    ]);
    assert!(model.host_is_away(host.id));

    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::Connected {
            tier: Tier::Pro,
            carrier: RelayCarrier::Tcp,
        }),
        host_up(&host),
    ]);
    assert!(!model.host_is_away(host.id));

    let offline = an_offline_host("desktop");
    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::Connected {
            tier: Tier::Free,
            carrier: RelayCarrier::Quic,
        }),
        host_up(&offline),
    ]);
    assert!(!model.host_is_away(offline.id));
}

#[test]
fn cloud_state_account_prompt_is_absent_when_every_host_is_reachable() {
    let direct = a_host("desktop");
    let mut ssh = a_host("server");
    ssh.via = HostVia::Ssh;
    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::SignedOut),
        host_up(&direct),
        host_up(&ssh),
    ]);
    assert_eq!(model.needs_account_prompt(), None);

    let relay = relay_host("away");
    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::SignedOut),
        host_up(&relay),
    ]);
    assert_eq!(model.needs_account_prompt(), Some(AccountPrompt::SignIn));

    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::Connected {
            tier: Tier::Pro,
            carrier: RelayCarrier::Quic,
        }),
        host_up(&direct),
        host_up(&ssh),
        host_up(&relay),
    ]);
    assert_eq!(model.needs_account_prompt(), None);

    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::Connected {
            tier: Tier::Free,
            carrier: RelayCarrier::Tcp,
        }),
        host_up(&relay),
    ]);
    assert_eq!(model.needs_account_prompt(), Some(AccountPrompt::Subscribe));

    let mut signed_out = an_offline_host("signed-out");
    signed_out.signed_in = Some(false);
    let model = fold(vec![
        connected("phone"),
        cloud(CloudState::Connected {
            tier: Tier::Pro,
            carrier: RelayCarrier::Quic,
        }),
        host_up(&signed_out),
    ]);
    assert_eq!(model.needs_account_prompt(), Some(AccountPrompt::SignIn));
}

fn retained_agent_sequence() -> Vec<Msg> {
    let online = a_host("desktop");
    let offline = an_offline_host("desktop");
    let returning = relay_host("desktop");
    seq([
        vec![
            connected("phone"),
            cloud(CloudState::Connected {
                tier: Tier::Pro,
                carrier: RelayCarrier::Quic,
            }),
            host_up(&online),
            agent_up(&an_agent("work", "desktop")),
        ],
        synced(),
        vec![
            disconnected(ui_state::DisconnectReason::TransportError {
                message: "profile reconnect".into(),
            }),
            connected("phone"),
            host_up(&offline),
            agent_gone("work"),
        ],
        synced(),
        vec![
            disconnected(ui_state::DisconnectReason::TransportError {
                message: "profile reconnect".into(),
            }),
            connected("phone"),
            host_up(&returning),
            agent_up(&an_agent("work", "desktop")),
        ],
        synced(),
    ])
}

#[test]
fn cloud_state_agent_inventory_survives_offline_and_returns_live_after_reconciliation() {
    let offline_prefix = retained_agent_sequence()
        .into_iter()
        .take(12)
        .collect::<Vec<_>>();
    let model = fold(offline_prefix);
    let cached = model.agent(agent_id("work")).expect("cached offline agent");
    assert!(!cached.live);

    let mut model = fold(retained_agent_sequence());
    assert!(model.agent(agent_id("work")).expect("returned agent").live);

    update(
        &mut model,
        Msg::Server(ServerMsg::HostRemoved {
            id: host_id("desktop"),
        }),
    );
    assert!(model.agent(agent_id("work")).is_none());
}

/// The tier changing is a reachability change in both directions: what the
/// relay refuses to carry for a free account it carries the moment the
/// account pays for it, and no machine sends its inventory again to say so.
fn paid_for_sequence() -> Vec<Msg> {
    let direct = a_host("workstation");
    let away = relay_host("workstation");
    seq([
        vec![
            connected("phone"),
            cloud(CloudState::Connected {
                tier: Tier::Free,
                carrier: RelayCarrier::Quic,
            }),
            host_up(&direct),
            agent_up(&an_agent("helper", "workstation")),
        ],
        synced(),
        vec![
            host_up(&away),
            cloud(CloudState::Connected {
                tier: Tier::Pro,
                carrier: RelayCarrier::Quic,
            }),
        ],
    ])
}

#[test]
fn cloud_state_a_paid_tier_puts_an_away_machines_agents_back_in_reach() {
    let away = paid_for_sequence().into_iter().take(7).collect::<Vec<_>>();
    let model = fold(away);
    assert!(
        !model.agent(agent_id("helper")).expect("away agent").live,
        "a free account's relay-only machine carries nothing, so its agent is not live"
    );

    let model = fold(paid_for_sequence());
    assert!(
        model.agent(agent_id("helper")).expect("paid agent").live,
        "the subscription put the machine back in reach and its agent is still a memory"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("cloud_state::retained_agent", retained_agent_sequence()),
        ("cloud_state::paid_for", paid_for_sequence()),
    ]
}

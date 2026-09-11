//! Cloud state and host reachability come from their authoritative streams.
//! Account prompts are derived only when an account action can restore a
//! trusted host, and unreachable hosts retain cached non-live agents.

use amux_ui::{AccountPrompt, CloudState, HostVia, Msg, RelayCarrier, ServerMsg, Tier, update};

use crate::harness::*;

fn cloud(state: CloudState) -> Msg {
    Msg::Server(ServerMsg::CloudState(state))
}

fn relay_host(name: &str) -> amux_ui::HostEntry {
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
    let mut model = amux_ui::Model::default();
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
            disconnected(amux_ui::DisconnectReason::TransportError {
                message: "profile reconnect".into(),
            }),
            connected("phone"),
            host_up(&offline),
            agent_gone("work"),
        ],
        synced(),
        vec![
            disconnected(amux_ui::DisconnectReason::TransportError {
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

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![("cloud_state::retained_agent", retained_agent_sequence())]
}

//! Chapter 4 — Session streams: lifecycle facts observed per agent.
//!
//! Stream entries arrive coalesced (the recorded Msg is the batch); close
//! reasons are facts the kernel records (an exit code becomes the phase
//! word). Content interpretation is the attention milestone's job.

use amux_ui::{AgentPhase, Msg, StreamCloseReason, StreamMsg, StreamPhase};

use crate::harness::*;

fn stream_base() -> Vec<Msg> {
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&an_agent("refactor-tunnels", "nova")),
        ],
        synced(),
    ])
}

fn activity_sequence() -> Vec<Msg> {
    seq([
        stream_base(),
        vec![
            stream("refactor-tunnels", StreamMsg::Opened { truncated: false }),
            stream("refactor-tunnels", StreamMsg::ReplayComplete),
            batch(
                "refactor-tunnels",
                30,
                vec![serde_json::json!({"type": "assistant", "message": "…"})],
            ),
        ],
    ])
}

fn exited_sequence() -> Vec<Msg> {
    seq([
        activity_sequence(),
        vec![stream(
            "refactor-tunnels",
            StreamMsg::Closed {
                reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
            },
        )],
    ])
}

/// Batched entries advance the recency used for fleet ranking.
#[test]
fn stream_batches_advance_last_activity() {
    let model = fold(activity_sequence());
    let card = model.agent(agent_id("refactor-tunnels")).expect("card");
    assert_eq!(card.last_activity, t0_plus(30));
}

/// Replay and live are distinct stream phases, separated by an explicit
/// marker — a late-joining fold can tell catch-up from now.
#[test]
fn stream_lifecycle_tracks_replay_then_live() {
    let opened = seq([
        stream_base(),
        vec![stream(
            "refactor-tunnels",
            StreamMsg::Opened { truncated: false },
        )],
    ]);
    let model = fold(opened.clone());
    let state = model.stream(agent_id("refactor-tunnels")).expect("stream");
    assert_eq!(state.phase, StreamPhase::Replaying);

    let model = fold(seq([
        opened,
        vec![stream("refactor-tunnels", StreamMsg::ReplayComplete)],
    ]));
    let state = model.stream(agent_id("refactor-tunnels")).expect("stream");
    assert_eq!(state.phase, StreamPhase::Live);
}

/// A stream closed by agent exit records the exit code on the card; the
/// status word shows the phase once the agent is no longer running.
#[test]
fn exit_close_reports_exited_phase() {
    let model = fold(exited_sequence());
    let card = model.agent(agent_id("refactor-tunnels")).expect("card");
    assert_eq!(card.phase, AgentPhase::Exited { exit_code: Some(0) });
    assert_eq!(card.status_label(), "exited(0)");
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("sessions::activity", activity_sequence()),
        ("sessions::exited", exited_sequence()),
    ]
}

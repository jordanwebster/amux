//! Chapter 4 — Session streams: lifecycle facts observed per agent.
//!
//! Stream entries arrive coalesced (the recorded Msg is the batch); close
//! reasons are facts the kernel records (an exit code becomes the phase
//! word). Content interpretation is the attention milestone's job.

use ui_state::{AgentPhase, Msg, StreamCloseReason, StreamMsg, StreamPhase};

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

fn removed_sequence() -> Vec<Msg> {
    seq([activity_sequence(), vec![agent_gone("refactor-tunnels")]])
}

fn late_stream_after_removal_sequence() -> Vec<Msg> {
    // The shell aborts the stream task when the removal folds, but events it
    // queued beforehand still arrive — every StreamMsg arm, after the agent
    // is gone.
    seq([
        removed_sequence(),
        vec![
            stream("refactor-tunnels", StreamMsg::Opened { truncated: false }),
            batch(
                "refactor-tunnels",
                60,
                vec![serde_json::json!({"type": "assistant", "message": "…"})],
            ),
            stream("refactor-tunnels", StreamMsg::ReplayComplete),
            stream(
                "refactor-tunnels",
                StreamMsg::Closed {
                    reason: StreamCloseReason::TransportError {
                        message: "connection reset".to_string(),
                    },
                },
            ),
        ],
    ])
}

/// Batched entries advance the recency used for fleet ranking.
#[test]
fn stream_batches_advance_last_activity() {
    let model = fold(activity_sequence());
    let card = model.agent(agent_id("refactor-tunnels")).expect("card");
    assert_eq!(card.last_activity, t0_plus(30));
}

/// Reading an agent's history back is not the agent doing anything. Opening
/// a conversation replays its tail, and dating those rows by when they
/// arrived would move the agent to the top of every list the moment someone
/// looked at it.
#[test]
fn replayed_history_does_not_advance_last_activity() {
    let model = fold(seq([
        stream_base(),
        vec![
            stream("refactor-tunnels", StreamMsg::Opened { truncated: false }),
            batch(
                "refactor-tunnels",
                600,
                vec![serde_json::json!({"type": "assistant", "message": "old"})],
            ),
            stream("refactor-tunnels", StreamMsg::ReplayComplete),
        ],
    ]));
    let card = model.agent(agent_id("refactor-tunnels")).expect("card");
    assert_eq!(card.last_activity, t0());
}

/// An agent this client never opens is dated by its host: the inventory
/// carries when it last did anything, and a later announcement moves it.
#[test]
fn unopened_agents_carry_the_hosts_last_activity() {
    let mut agent = an_agent("quiet", "nova");
    agent.last_activity = t0_plus(120);
    let announced = seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            agent_up(&agent),
        ],
        synced(),
    ]);
    let model = fold(announced.clone());
    let card = model.agent(agent_id("quiet")).expect("card");
    assert_eq!(card.last_activity, t0_plus(120));

    agent.last_activity = t0_plus(300);
    let model = fold(seq([announced, vec![agent_up(&agent)]]));
    let card = model.agent(agent_id("quiet")).expect("card");
    assert_eq!(card.last_activity, t0_plus(300));
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
    assert_eq!(model.status_label_for(card), "exited(0)");
}

/// Stream events for an agent the Model no longer knows are discarded: a
/// late `Opened` (queued before the removal aborted its task) must not
/// re-materialize entries in `Model::streams` — churn would accumulate
/// ghosts. The whole late tail is a no-op.
#[test]
fn late_stream_events_after_removal_leave_no_ghost_state() {
    let model = fold(late_stream_after_removal_sequence());
    assert!(
        model.agent(agent_id("refactor-tunnels")).is_none(),
        "the agent stays gone"
    );
    assert!(
        model.stream(agent_id("refactor-tunnels")).is_none(),
        "a late Opened must not re-materialize stream state"
    );
    assert_eq!(
        model,
        fold(removed_sequence()),
        "late stream events for a removed agent change nothing"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("sessions::activity", activity_sequence()),
        ("sessions::exited", exited_sequence()),
        (
            "sessions::late_stream_after_removal",
            late_stream_after_removal_sequence(),
        ),
    ]
}

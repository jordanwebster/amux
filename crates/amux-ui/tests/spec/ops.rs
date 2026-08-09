//! Chapter 3 — Operations: the Command write surface.
//!
//! Dispatch returns an OpId; outcomes return as state (finished ops),
//! because a lost outcome must not leave a spinner lying. While
//! disconnected, commands fail fast — there is no offline queue.

use amux_ui::{Command, Effect, FleetItem, Msg, NOT_CONNECTED_ERROR, OpOutcome};

use crate::harness::*;

fn connected_base() -> Vec<Msg> {
    seq([vec![connected("nova"), host_up(&a_host("nova"))], synced()])
}

fn create_flow_sequence() -> Vec<Msg> {
    seq([
        connected_base(),
        vec![
            command(op(1), create_cmd("claude-4", Some("nova"))),
            op_result(
                op(1),
                OpOutcome::AgentCreated {
                    agent: an_agent("claude-4", "nova"),
                },
            ),
        ],
    ])
}

fn failed_create_sequence() -> Vec<Msg> {
    seq([
        connected_base(),
        vec![
            command(op(1), create_cmd("claude-4", Some("nova"))),
            op_failed(op(1), "not connected — daemon unreachable"),
        ],
    ])
}

fn disconnected_command_sequence() -> Vec<Msg> {
    // Never connected at all: the very first Msg is a user command.
    vec![command(op(1), create_cmd("claude-4", Some("nova")))]
}

/// A dispatched create is immediately visible as a pending op (the fleet
/// grows an optimistic `◌ creating…` row), and resolving it clears the
/// spinner. The created entity itself arrives only via subscription.
#[test]
fn create_agent_tracks_pending_then_finished() {
    let dispatch_only = seq([
        connected_base(),
        vec![command(op(1), create_cmd("claude-4", Some("nova")))],
    ]);
    let (model, effects) = fold_with_effects(dispatch_only);
    assert_eq!(model.pending_ops().count(), 1);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::Rpc {
                op: effect_op,
                command: Command::CreateAgent { .. }
            }] if *effect_op == op(1)
        ),
        "dispatch produces exactly the RPC effect: {effects:?}"
    );
    let fleet = model.fleet();
    assert!(
        matches!(
            fleet.last(),
            Some(FleetItem::PendingCreate { name, .. }) if *name == "claude-4"
        ),
        "pending create renders an optimistic row"
    );

    let model = fold(create_flow_sequence());
    assert_eq!(model.pending_ops().count(), 0);
    let finished = model.finished_op(op(1)).expect("outcome retained as state");
    assert!(!finished.outcome.is_error());
    assert_eq!(
        model.agent_count(),
        0,
        "RPC results never write entities; the agent arrives by subscription"
    );
}

/// Failure clears the pending op and carries the error for the status line.
#[test]
fn failed_op_clears_pending_and_carries_error() {
    let model = fold(failed_create_sequence());
    assert_eq!(model.pending_ops().count(), 0);
    let failure = model.latest_op_failure().expect("failure surfaced");
    assert_eq!(failure.op, op(1));
    let OpOutcome::Error { error } = &failure.outcome else {
        panic!("expected error outcome");
    };
    assert_eq!(error.message, "not connected — daemon unreachable");
}

/// Commands dispatched while disconnected fail fast with a finished-op
/// error: no pending spinner, no effect, no offline queue.
#[test]
fn commands_fail_fast_while_disconnected() {
    let (model, effects) = fold_with_effects(disconnected_command_sequence());
    assert!(effects.is_empty(), "no RPC leaves a disconnected client");
    assert_eq!(model.pending_ops().count(), 0);
    let failure = model.latest_op_failure().expect("failure surfaced");
    let OpOutcome::Error { error } = &failure.outcome else {
        panic!("expected error outcome");
    };
    assert_eq!(error.message, NOT_CONNECTED_ERROR);
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("ops::create_flow", create_flow_sequence()),
        ("ops::failed_create", failed_create_sequence()),
        ("ops::disconnected_command", disconnected_command_sequence()),
    ]
}

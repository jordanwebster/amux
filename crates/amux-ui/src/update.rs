//! The reducer: `update(&mut Model, Msg) -> Vec<Effect>`.
//!
//! Pure by contract: no IO, no clock, no randomness — `std::time`, `std::fs`
//! and id-minting are banned in this module (enforced by review plus the
//! wire_free replay spec tests). The same reducer build folding the same
//! checkpoint and ordered Msgs produces identical Models and Effects.

use crate::effect::{DumpReason, Effect};
use crate::model::{
    AgentCard, AgentPhase, Attention, Connection, FINISHED_OPS_RETAINED, FinishedOp, HostState,
    Model, PendingOp, StreamPhase, StreamState,
};
use crate::msg::{Command, Msg, OpError, OpId, OpOutcome, ServerMsg, StreamCloseReason, StreamMsg};
use crate::summarizers::SummarizerState;
use crate::summarizers::claude::ClaudeSummarizer;

/// Error message for commands dispatched while the daemon link is down.
/// Commands fail fast — there is no offline queue.
pub const NOT_CONNECTED_ERROR: &str = "not connected — daemon unreachable";

/// Structured-stream catch-up window (`Tail{count}`): the one place this
/// policy number lives. Generous on purpose; the honest-degrade rule covers
/// whatever it misses.
pub const REPLAY_TAIL: u64 = 1000;

/// The structured stream the subscription policy watches. Advertised by the
/// agent's `io_protocols` — the fact, not an assumption about its type.
const STRUCTURED_PROTOCOL: &str = "claude_pty_transcript_v1";

pub fn update(model: &mut Model, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Command { op, command } => update_command(model, op, command),
        Msg::Server(server) => update_server(model, server),
        Msg::OpResult { op, outcome } => update_op_result(model, op, outcome),
        Msg::Stream { agent, event } => update_stream(model, agent, event),
        Msg::UserAttached { agent } => {
            // Widen the subscription policy to agents the user interacts
            // with, wherever they run.
            ensure_stream(model, agent).into_iter().collect()
        }
        Msg::Tick { now } => {
            model.now = Some(now);
            Vec::new()
        }
    }
}

/// Subscription policy: open the structured stream for an agent when it
/// advertises one and none is already live. Emits at most one effect;
/// re-upserts are idempotent. Retryable closes (transport loss) reopen on
/// the next inventory event; terminal closes (deleted, exited) do not.
fn ensure_stream(model: &mut Model, agent_id: amux::AgentId) -> Option<Effect> {
    let card = model.agents.get(&agent_id)?;
    if !card
        .agent
        .io_protocols
        .iter()
        .any(|protocol| protocol == STRUCTURED_PROTOCOL)
    {
        return None;
    }
    let reopen = match model.streams.get(&agent_id) {
        None => true,
        Some(state) => match &state.phase {
            StreamPhase::Opening | StreamPhase::Replaying | StreamPhase::Live => false,
            StreamPhase::Closed { reason } => !matches!(
                reason,
                StreamCloseReason::AgentDeleted | StreamCloseReason::AgentExited { .. }
            ),
        },
    };
    if !reopen {
        return None;
    }
    model.streams.insert(
        agent_id,
        StreamState {
            phase: StreamPhase::Opening,
            truncated: false,
        },
    );
    Some(Effect::OpenStream {
        agent: agent_id,
        tail: REPLAY_TAIL,
    })
}

fn update_command(model: &mut Model, op: OpId, command: Command) -> Vec<Effect> {
    let seq = model.op_seq;
    model.op_seq += 1;

    if !model.is_connected() {
        push_finished(
            model,
            FinishedOp {
                op,
                seq,
                command,
                outcome: OpOutcome::Error {
                    error: OpError {
                        message: NOT_CONNECTED_ERROR.to_string(),
                        auth_required: false,
                    },
                },
            },
        );
        return Vec::new();
    }

    model.pending_ops.insert(
        op,
        PendingOp {
            op,
            seq,
            command: command.clone(),
        },
    );
    vec![Effect::Rpc { op, command }]
}

fn update_op_result(model: &mut Model, op: OpId, outcome: OpOutcome) -> Vec<Effect> {
    // Results for superseded or unknown requests are discarded: arrival
    // order is not freshness.
    let Some(pending) = model.pending_ops.remove(&op) else {
        return Vec::new();
    };
    if let OpOutcome::Error { error } = &outcome
        && error.auth_required
    {
        model.cloud_auth_required = true;
    }
    // Entity payloads riding on the outcome resolve the op only —
    // subscriptions are the sole writer of entity state.
    push_finished(
        model,
        FinishedOp {
            op,
            seq: pending.seq,
            command: pending.command,
            outcome,
        },
    );
    Vec::new()
}

fn update_server(model: &mut Model, server: ServerMsg) -> Vec<Effect> {
    match server {
        ServerMsg::Connected { local_host_id } => {
            model.epoch += 1;
            model.connection = Connection::Connected {
                hosts_synchronized: false,
                agents_synchronized: false,
            };
            model.local_host_id = local_host_id.or(model.local_host_id);
            model.cloud_auth_required = false;
            Vec::new()
        }
        ServerMsg::Disconnected { reason } => {
            model.connection = Connection::Disconnected { reason };
            Vec::new()
        }
        ServerMsg::HostUpserted { host } => {
            if !model.is_connected() {
                return tripwire("host upsert while not connected");
            }
            model.hosts.insert(
                host.id,
                HostState {
                    entry: host,
                    epoch: model.epoch,
                },
            );
            Vec::new()
        }
        ServerMsg::HostRemoved { id } => {
            if !model.is_connected() {
                return tripwire("host removal while not connected");
            }
            model.hosts.remove(&id);
            Vec::new()
        }
        ServerMsg::HostsSynchronized => {
            let Connection::Connected {
                hosts_synchronized, ..
            } = &mut model.connection
            else {
                return tripwire("hosts synchronized while not connected");
            };
            *hosts_synchronized = true;
            prune_if_synchronized(model);
            Vec::new()
        }
        ServerMsg::AgentUpserted { agent } => {
            if !model.is_connected() {
                return tripwire("agent upsert while not connected");
            }
            let epoch = model.epoch;
            let agent_id = agent.id;
            let is_local = model.local_host_id == Some(agent.host_id);
            match model.agents.get_mut(&agent_id) {
                Some(card) => {
                    // Facts update; UI-layer derived state persists across
                    // upserts of the same entity.
                    card.agent = agent;
                    card.epoch = epoch;
                }
                None => {
                    let card = AgentCard {
                        last_activity: agent.created_at,
                        provider_label: None,
                        attention: Attention::Unknown,
                        phase: AgentPhase::Running,
                        summarizer: None,
                        epoch,
                        agent,
                    };
                    model.agents.insert(agent_id, card);
                }
            }
            // Kernel policy: every local agent's structured stream is
            // subscribed (in-process, cheap); remote agents join on attach.
            if is_local {
                ensure_stream(model, agent_id).into_iter().collect()
            } else {
                Vec::new()
            }
        }
        ServerMsg::AgentRemoved { id } => {
            if !model.is_connected() {
                return tripwire("agent removal while not connected");
            }
            model.agents.remove(&id);
            if let Some(stream) = model.streams.remove(&id)
                && !matches!(stream.phase, StreamPhase::Closed { .. })
            {
                return vec![Effect::CloseStream { agent: id }];
            }
            Vec::new()
        }
        ServerMsg::AgentsSynchronized => {
            let Connection::Connected {
                agents_synchronized,
                ..
            } = &mut model.connection
            else {
                return tripwire("agents synchronized while not connected");
            };
            *agents_synchronized = true;
            prune_if_synchronized(model);
            Vec::new()
        }
    }
}

fn update_stream(model: &mut Model, agent: amux::AgentId, event: StreamMsg) -> Vec<Effect> {
    // Stream tasks race the inventory stream: events for agents we no longer
    // know (or after a disconnect) are legitimate latecomers, not tripwires.
    match event {
        StreamMsg::Opened { truncated } => {
            model.streams.insert(
                agent,
                StreamState {
                    phase: StreamPhase::Replaying,
                    truncated,
                },
            );
            with_claude_summarizer(model, agent, |fold| fold.begin_window(truncated));
        }
        StreamMsg::Batch { at, entries } => {
            if let Some(card) = model.agents.get_mut(&agent) {
                card.last_activity = card.last_activity.max(at);
            }
            with_claude_summarizer(model, agent, |fold| {
                for entry in &entries {
                    fold.observe(&entry.payload);
                }
            });
        }
        StreamMsg::ReplayComplete => {
            if let Some(stream) = model.streams.get_mut(&agent) {
                stream.phase = StreamPhase::Live;
            }
        }
        StreamMsg::Closed { reason } => {
            if reason == StreamCloseReason::AuthenticationRequired {
                model.cloud_auth_required = true;
            }
            match &reason {
                StreamCloseReason::AgentExited { exit_code } => {
                    let exit_code = *exit_code;
                    if let Some(card) = model.agents.get_mut(&agent) {
                        card.phase = AgentPhase::Exited { exit_code };
                    }
                    with_claude_summarizer(model, agent, |fold| fold.observe_exit());
                }
                StreamCloseReason::AgentDeleted => {}
                // The stream died underneath us: whatever the fold knew is
                // stale. Degrade to Unknown, never to a wrong badge.
                _ => with_claude_summarizer(model, agent, |fold| fold.invalidate()),
            }
            if let Some(stream) = model.streams.get_mut(&agent) {
                stream.phase = StreamPhase::Closed { reason };
            }
        }
    }
    Vec::new()
}

/// Run a fold step on the agent's claude summarizer (creating it on first
/// evidence) and refresh the derived attention. Non-claude agents have no
/// summarizer and honestly stay `Unknown`.
fn with_claude_summarizer(
    model: &mut Model,
    agent: amux::AgentId,
    step: impl FnOnce(&mut ClaudeSummarizer),
) {
    let Some(card) = model.agents.get_mut(&agent) else {
        return;
    };
    if card.agent.agent_type != "claude" {
        return;
    }
    let state = card
        .summarizer
        .get_or_insert_with(|| SummarizerState::Claude(ClaudeSummarizer::default()));
    let SummarizerState::Claude(fold) = state;
    step(fold);
    card.attention = state.attention();
}

fn tripwire(detail: &str) -> Vec<Effect> {
    vec![Effect::RequestDump {
        reason: DumpReason::Tripwire {
            detail: detail.to_string(),
        },
    }]
}

/// Reconnect replaces state by snapshot: once both snapshots for the new
/// epoch are complete, entities not re-upserted under it are gone.
fn prune_if_synchronized(model: &mut Model) {
    if !model.is_synchronized() {
        return;
    }
    let epoch = model.epoch;
    model.hosts.retain(|_, host| host.epoch == epoch);
    model.agents.retain(|_, card| card.epoch == epoch);
    let live: Vec<amux::AgentId> = model.streams.keys().copied().collect();
    for id in live {
        if !model.agents.contains_key(&id) {
            model.streams.remove(&id);
        }
    }
}

fn push_finished(model: &mut Model, finished: FinishedOp) {
    model.finished_ops.push(finished);
    if model.finished_ops.len() > FINISHED_OPS_RETAINED {
        let excess = model.finished_ops.len() - FINISHED_OPS_RETAINED;
        model.finished_ops.drain(..excess);
    }
}

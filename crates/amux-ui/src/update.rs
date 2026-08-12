//! The reducer: `update(&mut Model, Msg) -> Vec<Effect>`.
//!
//! Pure by contract: no IO, no clock, no randomness — `std::time`, `std::fs`
//! and id-minting are banned in this module (enforced by review plus the
//! wire_free replay spec tests). The same reducer build folding the same
//! checkpoint and ordered Msgs produces identical Models and Effects.

use crate::claude::AskState;
use crate::claude::encoding::{self, KeyStep};
use crate::effect::{DumpReason, Effect};
use crate::model::{
    AgentCard, AgentPhase, Attention, Connection, FINISHED_OPS_RETAINED, FinishedOp, HostState,
    Model, PendingOp, StreamPhase, StreamState,
};
use crate::msg::{Command, Msg, OpError, OpId, OpOutcome, ServerMsg, StreamCloseReason, StreamMsg};

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
    // Readonly agents are hidden from the fleet; a badge nobody can see is
    // not worth a stream.
    if card.agent.readonly {
        return None;
    }
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
    // 1-based so "no failures dismissed" is naturally seq 0 for viewers.
    model.op_seq += 1;
    let seq = model.op_seq;

    if !model.is_connected() {
        // Commands fail fast while disconnected — no offline queue.
        return refuse(model, op, seq, command, NOT_CONNECTED_ERROR);
    }

    match command {
        Command::CreateAgent { .. } | Command::RenameAgent { .. } | Command::DeleteAgent { .. } => {
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
        Command::SendPrompt { .. } => update_send_prompt(model, op, seq, command),
        Command::AnswerAsk { .. } => update_answer_ask(model, op, seq, command),
        Command::Interrupt { .. } => update_interrupt(model, op, seq, command),
        Command::CyclePermissionMode { .. } => update_cycle_mode(model, op, seq, command),
    }
}

/// A synchronous command refusal: the outcome is finished state
/// immediately — no pending op, no effect, no spinner (the model states
/// the failure; drafts and panels resurface from it).
fn refuse(model: &mut Model, op: OpId, seq: u64, command: Command, message: &str) -> Vec<Effect> {
    push_finished(
        model,
        FinishedOp {
            op,
            seq,
            command,
            outcome: OpOutcome::Error {
                error: OpError {
                    message: message.to_string(),
                    auth_required: false,
                },
            },
        },
    );
    Vec::new()
}

/// One keystroke-injection dispatch: the program plus the reducer's
/// stale-seq policy for it.
struct InputDispatch {
    agent: amux::AgentId,
    program: Vec<KeyStep>,
    retry_stale: bool,
}

/// Track the op and hand the shell one keystroke-injection effect, seq-
/// guarded by the layer's cursor. The shell always answers with an
/// `OpResult`; readonly and transport rejections come back that way — the
/// server rejects, the model states it.
fn dispatch_input(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    dispatch: InputDispatch,
) -> Vec<Effect> {
    let expected_seq = model
        .claude(dispatch.agent)
        .map_or(0, |layer| layer.cursor());
    model.pending_ops.insert(op, PendingOp { op, seq, command });
    vec![Effect::SendInput {
        op,
        agent: dispatch.agent,
        expected_seq,
        program: dispatch.program,
        retry_stale: dispatch.retry_stale,
    }]
}

/// B1/D2: send is gated on phase (one derivation — the same gate the
/// composer footer states); a dispatched send records the optimistic echo
/// before the bytes leave.
fn update_send_prompt(model: &mut Model, op: OpId, seq: u64, command: Command) -> Vec<Effect> {
    let Command::SendPrompt { agent, ref text } = command else {
        unreachable!("routed by update_command");
    };
    if let Some(refusal) = model.claude_send_gate(agent).refusal() {
        return refuse(model, op, seq, command.clone(), refusal);
    }
    let text = encoding::normalize_prompt(text);
    let program = match encoding::prompt_program(&text) {
        Ok(program) => program,
        Err(error) => return refuse(model, op, seq, command.clone(), &error.to_string()),
    };
    let now = model.now;
    with_existing_claude_layer(model, agent, |layer| {
        layer.note_prompt_sent(op, text, now);
    });
    dispatch_input(
        model,
        op,
        seq,
        command,
        InputDispatch {
            agent,
            program,
            retry_stale: false,
        },
    )
}

/// C5: the answer intent flips the ask to answered-optimistic (the
/// retained answer is the pending-marker data) and becomes a keystroke
/// program addressed by the ask's typed kind.
fn update_answer_ask(model: &mut Model, op: OpId, seq: u64, command: Command) -> Vec<Effect> {
    let Command::AnswerAsk {
        agent,
        ask,
        ref answer,
    } = command
    else {
        unreachable!("routed by update_command");
    };
    let Some(layer) = model.claude(agent) else {
        return refuse(
            model,
            op,
            seq,
            command.clone(),
            "chat input unavailable for this agent",
        );
    };
    let Some(entry) = layer.asks().find(|entry| entry.id == ask) else {
        // Remote resolution wins over local intent: the ask is gone, the
        // panel has already collapsed to the fact.
        return refuse(model, op, seq, command.clone(), "ask already resolved");
    };
    // Claude's remote menu only ever displays the HEAD of the queue: a
    // program addressed to a later ask would encode that ask's digits and
    // apply them to the head's menu — answering the wrong request. The
    // queue is the model's honesty about depth, not an addressing space.
    if layer.ask_head().is_some_and(|head| head.id != ask) {
        return refuse(
            model,
            op,
            seq,
            command.clone(),
            "ask is queued behind the current menu — answer the head ask first",
        );
    }
    if matches!(entry.state, AskState::AnsweredOptimistic { .. }) {
        return refuse(
            model,
            op,
            seq,
            command.clone(),
            "answer already in flight — awaiting confirmation",
        );
    }
    let program = match encoding::answer_program(&entry.kind, answer) {
        Ok(program) => program,
        Err(error) => return refuse(model, op, seq, command.clone(), &error.to_string()),
    };
    let answer = answer.clone();
    with_existing_claude_layer(model, agent, |layer| {
        layer.note_ask_answered(ask, op, answer);
    });
    dispatch_input(
        model,
        op,
        seq,
        command,
        InputDispatch {
            agent,
            program,
            retry_stale: false,
        },
    )
}

/// D3: interrupt is allowed in every state — no phase gate, no ask gate.
/// Its meaning does not depend on the session's position, so a stale-seq
/// refusal is retried mechanically by the shell.
fn update_interrupt(model: &mut Model, op: OpId, seq: u64, command: Command) -> Vec<Effect> {
    let Command::Interrupt { agent } = command else {
        unreachable!("routed by update_command");
    };
    if !model.agents.contains_key(&agent) {
        return refuse(model, op, seq, command, "unknown agent");
    }
    dispatch_input(
        model,
        op,
        seq,
        command,
        InputDispatch {
            agent,
            program: encoding::interrupt_program(),
            retry_stale: true,
        },
    )
}

/// D4: the cycle injects Shift+Tab; the mode FACT returns via hook
/// payloads (the cycle emits no transcript row — fixture-verified), so
/// nothing is flipped optimistically.
fn update_cycle_mode(model: &mut Model, op: OpId, seq: u64, command: Command) -> Vec<Effect> {
    let Command::CyclePermissionMode { agent } = command else {
        unreachable!("routed by update_command");
    };
    if let Some(refusal) = model.claude_mode_cycle_gate(agent) {
        return refuse(model, op, seq, command, refusal);
    }
    dispatch_input(
        model,
        op,
        seq,
        command,
        InputDispatch {
            agent,
            program: encoding::mode_cycle_program(),
            retry_stale: false,
        },
    )
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
    // A failed input send resurfaces its optimistic state with the failure
    // stated (C5): the echo leaves (the draft resurfaces from ViewState;
    // this finished op carries the fact), the ask flips to SendFailed. An
    // ask that resolved remotely in the meantime stays gone — the layer
    // mutators no-op on missing targets, so a late failure cannot
    // resurrect anything.
    if let OpOutcome::Error { error } = &outcome {
        match &pending.command {
            Command::SendPrompt { agent, .. } => {
                with_existing_claude_layer(model, *agent, |layer| {
                    layer.note_prompt_send_failed(op);
                });
            }
            Command::AnswerAsk { agent, ask, .. } => {
                let message = error.message.clone();
                let ask = *ask;
                with_existing_claude_layer(model, *agent, |layer| {
                    layer.note_ask_send_failed(ask, op, message);
                });
            }
            _ => {}
        }
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
            prune_if_synchronized(model)
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
                        claude: None,
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
            prune_if_synchronized(model)
        }
    }
}

fn update_stream(model: &mut Model, agent: amux::AgentId, event: StreamMsg) -> Vec<Effect> {
    // Stream tasks race the inventory stream: events for agents we no longer
    // know (or after a disconnect) are legitimate latecomers, not tripwires —
    // but folding them would re-materialize state for entities that no
    // longer exist (a late `Opened`, queued before `AgentRemoved` aborted
    // its task, must not leave a ghost in `streams`). Discard them all.
    if !model.agents.contains_key(&agent) {
        return Vec::new();
    }
    match event {
        StreamMsg::Opened { truncated } => {
            model.streams.insert(
                agent,
                StreamState {
                    phase: StreamPhase::Replaying,
                    truncated,
                },
            );
            // A fresh subscription replays the source tail from scratch, so
            // the chat layer folds from scratch too — its window carries
            // the same truncation fact (B9's honest boundary).
            with_claude_layer(model, agent, |layer| layer.begin_window(truncated));
        }
        StreamMsg::Batch { at, entries } => {
            if let Some(card) = model.agents.get_mut(&agent) {
                card.last_activity = card.last_activity.max(at);
            }
            with_claude_layer(model, agent, |layer| {
                for entry in &entries {
                    layer.observe(entry.seq, at, &entry.payload);
                }
            });
        }
        StreamMsg::ReplayComplete => {
            if let Some(stream) = model.streams.get_mut(&agent) {
                stream.phase = StreamPhase::Live;
            }
            // The out-of-band liveness unlock: a truncated tail may no
            // longer contain the in-band `amux.transcript_ready` marker (a
            // long-running session wrote past the bounded buffer), and a
            // window that never unlocks would suppress live prompts and
            // permission hooks forever.
            with_claude_layer(model, agent, |layer| layer.observe_replay_complete());
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
                    // Nothing is left to need: obligations do not outlive
                    // the process that owned them.
                    with_claude_layer(model, agent, |layer| layer.observe_exit());
                }
                StreamCloseReason::AgentDeleted => {}
                // The stream died underneath us: whatever the fold knew is
                // stale. Degrade to Unknown, never to a wrong badge.
                _ => with_claude_layer(model, agent, |layer| layer.invalidate()),
            }
            if let Some(stream) = model.streams.get_mut(&agent) {
                stream.phase = StreamPhase::Closed { reason };
            }
        }
    }
    Vec::new()
}

/// Run a fold step on the agent's Claude chat layer (creating it on first
/// evidence), gated on the advertised-protocol fact — the fold consumes
/// `claude_pty_transcript_v1` rows, whatever the agent calls itself.
/// The layer is always folded, chat open or not, and the kernel attention
/// summary is derived from the SAME fold state afterwards (E2: one fold,
/// not two) — so fleet attention behaves identically whether or not a chat
/// is open (E3). Agents without the protocol have no layer and honestly
/// stay `Unknown`.
fn with_claude_layer(
    model: &mut Model,
    agent: amux::AgentId,
    step: impl FnOnce(&mut crate::claude::ClaudeLayer),
) {
    let Some(card) = model.agents.get_mut(&agent) else {
        return;
    };
    if !card
        .agent
        .io_protocols
        .iter()
        .any(|protocol| protocol == STRUCTURED_PROTOCOL)
    {
        return;
    }
    let layer = card.claude.get_or_insert_with(Default::default);
    step(layer);
    card.attention = layer.attention();
}

/// Mutate an agent's EXISTING Claude layer (never creating one — op
/// results for gone agents fold to nothing) and refresh the derived
/// attention so card and layer stay one story.
fn with_existing_claude_layer(
    model: &mut Model,
    agent: amux::AgentId,
    step: impl FnOnce(&mut crate::claude::ClaudeLayer),
) {
    let Some(card) = model.agents.get_mut(&agent) else {
        return;
    };
    let Some(layer) = card.claude.as_mut() else {
        return;
    };
    step(layer);
    card.attention = layer.attention();
}

fn tripwire(detail: &str) -> Vec<Effect> {
    vec![Effect::RequestDump {
        reason: DumpReason::Tripwire {
            detail: detail.to_string(),
        },
    }]
}

/// Reconnect replaces state by snapshot: once both snapshots for the new
/// epoch are complete, entities not re-upserted under it are gone. Streams
/// dropped here still have a shell task behind them — each one leaves as a
/// `CloseStream` effect so no task is orphaned across reconnects (both
/// synchronized arms call this and must propagate the effects).
fn prune_if_synchronized(model: &mut Model) -> Vec<Effect> {
    if !model.is_synchronized() {
        return Vec::new();
    }
    let epoch = model.epoch;
    model.hosts.retain(|_, host| host.epoch == epoch);
    model.agents.retain(|_, card| card.epoch == epoch);
    let stale: Vec<amux::AgentId> = model
        .streams
        .keys()
        .filter(|id| !model.agents.contains_key(id))
        .copied()
        .collect();
    let mut effects = Vec::new();
    for id in stale {
        if let Some(stream) = model.streams.remove(&id)
            && !matches!(stream.phase, StreamPhase::Closed { .. })
        {
            effects.push(Effect::CloseStream { agent: id });
        }
    }
    effects
}

fn push_finished(model: &mut Model, finished: FinishedOp) {
    model.finished_ops.push(finished);
    if model.finished_ops.len() > FINISHED_OPS_RETAINED {
        let excess = model.finished_ops.len() - FINISHED_OPS_RETAINED;
        model.finished_ops.drain(..excess);
    }
}

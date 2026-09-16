//! The reducer: `update(&mut Model, Msg) -> Vec<Effect>`.
//!
//! Pure by contract: no IO, no clock, no randomness — `std::time`, `std::fs`
//! and id-minting are banned in this module (enforced by review plus the
//! wire_free replay spec tests). The same reducer build folding the same
//! checkpoint and ordered Msgs produces identical Models and Effects.

use crate::effect::{DumpReason, Effect, InputPayload};
use crate::model::{
    AgentCard, AgentLayer, AgentPhase, Attention, Connection, FINISHED_OPS_RETAINED, FinishedOp,
    HostState, LocalSummary, Model, PendingOp, StreamPhase, StreamState,
};
use crate::msg::{Command, Msg, OpError, OpId, OpOutcome, ServerMsg, StreamCloseReason, StreamMsg};
use crate::store::{ChatCommand, StoreUpdate};

/// Error message for commands dispatched while the daemon link is down.
/// Immediate writes fail fast; explicitly held drafts remain local.
pub const NOT_CONNECTED_ERROR: &str = "not connected — daemon unreachable";

/// Structured-stream catch-up window (`Tail{count}`): the one place this
/// policy number lives. Generous on purpose; the honest-degrade rule covers
/// whatever it misses.
pub const REPLAY_TAIL: u64 = 1000;

pub fn update(model: &mut Model, msg: Msg) -> Vec<Effect> {
    let mut effects = match msg {
        Msg::Command { op, command } => update_command(model, op, command),
        Msg::Server(server) => update_server(model, server),
        Msg::OpResult { op, outcome } => update_op_result(model, op, outcome),
        Msg::Stream { agent, event } => update_stream(model, agent, event),
        Msg::StoreStartup {
            profile,
            generations,
        } => crate::store::startup(&mut model.store, profile, generations),
        Msg::Store(message) => update_store_message(model, message),
        Msg::FleetDelta(delta) => crate::store::fleet_apply(&mut model.store, delta),
        Msg::Chat(command) => update_chat_command(model, command),
        Msg::ChatStream {
            agent,
            attempt,
            event,
        } => {
            if !model.store.accepts_stream(agent, attempt) {
                Vec::new()
            } else {
                let mirror = event.clone();
                let mut effects =
                    crate::store::update_chat_stream(&mut model.store, agent, attempt, event);
                effects.extend(mirror_chat_stream(model, agent, mirror));
                sync_chat_summary(model, agent);
                effects
            }
        }
        Msg::UserAttached { agent } => {
            if let Some(card) = model.agents.get(&agent) {
                model.attached.insert(agent, card.agent.host_id);
            }
            ensure_stream(model, agent).into_iter().collect()
        }
        Msg::UserDetached { agent } => {
            model.attached.remove(&agent);
            release_stream(model, agent).into_iter().collect()
        }
        Msg::Tick { now } => {
            model.now = Some(now);
            for card in model.agents.values_mut() {
                if let Some(local) = card.local_summary.as_mut() {
                    let changes = local.fold.apply_summary(fold::Input::Tick { now });
                    local.through = changes.through;
                }
            }
            Vec::new()
        }
    };
    effects.extend(crate::queue::deliver_ready(model));
    effects
}

/// Open a legacy structured stream only for an explicitly attached
/// conversation. Fleet inventory never calls this; standing comes from the
/// daemon's fleet summaries.
fn ensure_stream(model: &mut Model, agent_id: model::AgentId) -> Option<Effect> {
    let card = model.agents.get(&agent_id)?;
    let protocol = AgentLayer::from_kind(&card.agent.kind)?.protocol();
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
    refresh_attention(model, agent_id);
    Some(Effect::OpenStream {
        agent: agent_id,
        protocol,
        tail: REPLAY_TAIL,
    })
}

fn release_stream(model: &mut Model, agent_id: crate::AgentId) -> Option<Effect> {
    let stream = model.streams.remove(&agent_id)?;
    refresh_attention(model, agent_id);
    (!matches!(stream.phase, StreamPhase::Closed { .. }))
        .then_some(Effect::CloseStream { agent: agent_id })
}

fn update_command(model: &mut Model, op: OpId, command: Command) -> Vec<Effect> {
    // 1-based so "no failures dismissed" is naturally seq 0 for viewers.
    model.op_seq += 1;
    let seq = model.op_seq;

    if let Command::Queue(command) = command {
        return crate::queue::update_command(model, op, seq, command);
    }

    if !model.is_synchronized() {
        // Commands fail fast until this connection's inventory snapshot has
        // confirmed the card — no write may target remembered-only state.
        return refuse(model, op, seq, redact_command(command), NOT_CONNECTED_ERROR);
    }

    match command {
        Command::Queue(_) => unreachable!("queue commands handled above"),
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
        Command::Send { agent, draft } => crate::queue::update_draft(model, op, seq, agent, draft),
        Command::SendPromptWithAttachments {
            agent,
            text,
            attachments,
        } => update_attachment_prompt(model, op, seq, agent, text, attachments),
        Command::PutAttachment { agent, attachment } => dispatch_operation(
            model,
            op,
            seq,
            // The model keeps what the artifact is, never the bytes: pending
            // operations are recorded, and a recording is not a place for
            // somebody's photograph.
            redact_command(Command::PutAttachment {
                agent,
                attachment: attachment.clone(),
            }),
            Effect::PutAttachment {
                op,
                agent,
                attachment,
            },
        ),
        Command::FetchDiff { agent, id } => dispatch_operation(
            model,
            op,
            seq,
            Command::FetchDiff {
                agent,
                id: id.clone(),
            },
            Effect::FetchDiff { op, agent, id },
        ),
        Command::OpenAttachment { agent, id } => dispatch_operation(
            model,
            op,
            seq,
            Command::OpenAttachment {
                agent,
                id: id.clone(),
            },
            Effect::OpenExternally { op, agent, id },
        ),
        Command::RequestDiff { agent, base } => dispatch_operation(
            model,
            op,
            seq,
            Command::RequestDiff {
                agent,
                base: base.clone(),
            },
            Effect::Diff { op, agent, base },
        ),
        Command::Claude(command) => crate::claude::update::update_command(model, op, seq, command),
        Command::ClaudeSdk(command) => {
            crate::claude_sdk::update::update_command(model, op, seq, command)
        }
        Command::Codex(command) => crate::codex::update::update_command(model, op, seq, command),
        command @ (Command::SetModel { .. }
        | Command::SetEffort { .. }
        | Command::SetPreset { .. }) => crate::provider::update_settings(model, op, seq, command),
    }
}

fn redact_command(mut command: Command) -> Command {
    if let Command::PutAttachment { attachment, .. } = &mut command {
        attachment.bytes = None;
    }
    if let Command::SendPromptWithAttachments { attachments, .. }
    | Command::Send {
        draft: crate::Draft { attachments, .. },
        ..
    } = &mut command
    {
        for attachment in attachments {
            attachment.bytes = None;
        }
    }
    command
}

fn dispatch_operation(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    effect: Effect,
) -> Vec<Effect> {
    model.pending_ops.insert(op, PendingOp { op, seq, command });
    vec![effect]
}

pub(crate) fn update_attachment_prompt(
    model: &mut Model,
    op: OpId,
    seq: u64,
    agent: model::AgentId,
    text: String,
    attachments: Vec<crate::attachments::DraftAttachment>,
) -> Vec<Effect> {
    let state_attachments = attachments
        .iter()
        .cloned()
        .map(|mut attachment| {
            attachment.bytes = None;
            attachment
        })
        .collect();
    let command = Command::SendPromptWithAttachments {
        agent,
        text: text.clone(),
        attachments: state_attachments,
    };
    let kind = model.agents.get(&agent).map(|card| card.agent.kind);
    let native = match kind {
        Some(model::AgentKind::Claude {
            driver: model::ClaudeDriver::Pty,
        }) => crate::claude::update::update_command(
            model,
            op,
            seq,
            crate::claude::ClaudeCommand::SendPrompt { agent, text },
        ),
        Some(model::AgentKind::Codex) => crate::codex::update::update_command(
            model,
            op,
            seq,
            crate::codex::CodexCommand::Prompt { agent, text },
        ),
        Some(model::AgentKind::Claude {
            driver: model::ClaudeDriver::Sdk,
        }) => crate::claude_sdk::update::update_command(
            model,
            op,
            seq,
            crate::claude_sdk::ClaudeSdkCommand::SendPrompt { agent, text },
        ),
        Some(model::AgentKind::TestAgent) | None => {
            return refuse(
                model,
                op,
                seq,
                command,
                "chat input unavailable for this agent",
            );
        }
    };

    if let Some(pending) = model.pending_ops.get_mut(&op) {
        pending.command = command.clone();
    }
    if let Some(finished) = model
        .finished_ops
        .iter_mut()
        .rev()
        .find(|finished| finished.op == op)
    {
        finished.command = command;
    }

    let pin: Vec<model::ArtifactId> = attachments
        .iter()
        .map(|attachment| attachment.id.clone())
        .collect();
    native
        .into_iter()
        .map(|effect| match effect {
            Effect::SendInput {
                op, agent, payload, ..
            } => Effect::PutThenSend {
                op,
                agent,
                puts: attachments.clone(),
                input: payload,
                pin: pin.clone(),
            },
            other => other,
        })
        .collect()
}

/// A synchronous command refusal: the outcome is finished state
/// immediately — no pending op, no effect, no spinner (the model states
/// the failure; drafts and panels resurface from it).
pub(crate) fn refuse(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    message: &str,
) -> Vec<Effect> {
    push_finished(
        model,
        FinishedOp {
            op,
            seq,
            command,
            outcome: OpOutcome::Error {
                error: OpError::general(message),
            },
        },
    );
    Vec::new()
}

/// Track the op and hand the shell one native input effect. The operation
/// UUID is also the protocol-visible correlation id, keeping replay pure.
pub(crate) fn dispatch_input(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: model::AgentId,
    payload: InputPayload,
) -> Vec<Effect> {
    model.pending_ops.insert(op, PendingOp { op, seq, command });
    vec![Effect::SendInput {
        op,
        agent,
        input_id: op.0.as_bytes().to_vec(),
        payload,
    }]
}

fn update_op_result(model: &mut Model, op: OpId, outcome: OpOutcome) -> Vec<Effect> {
    // Results for superseded or unknown requests are discarded: arrival
    // order is not freshness.
    let Some(pending) = model.pending_ops.remove(&op) else {
        return Vec::new();
    };
    if let OpOutcome::Error { error } = &outcome
        && error.auth_required()
    {
        model.cloud_auth_required = true;
    }
    if let OpOutcome::Error { error } = &outcome
        && error.subscription_required()
    {
        model.cloud_subscription_required = true;
    }
    // A failed input send resurfaces its optimistic state with the failure
    // stated (C5): the echo leaves (the draft resurfaces from ViewState;
    // this finished op carries the fact), the ask flips to SendFailed. An
    // ask that resolved remotely in the meantime stays gone — the layer
    // mutators no-op on missing targets, so a late failure cannot
    // resurrect anything.
    if let OpOutcome::Error { error } = &outcome
        && let Command::Claude(command) = &pending.command
    {
        crate::claude::update::update_failed_command(model, op, command, error)
    }
    if matches!(&outcome, OpOutcome::Error { .. }) {
        match &pending.command {
            Command::Send { agent, .. }
            | Command::SetModel { agent, .. }
            | Command::SetEffort { agent, .. }
            | Command::SetPreset { agent, .. } => {
                crate::codex::update::note_send_failed(model, op, *agent)
            }
            _ => {}
        }
    }
    if let OpOutcome::Error { error } = &outcome
        && let Command::ClaudeSdk(command) = &pending.command
    {
        crate::claude_sdk::update::update_failed_command(model, op, command, error)
    }
    if let OpOutcome::Error { error } = &outcome
        && let Command::Codex(command) = &pending.command
    {
        crate::codex::update::update_failed_command(model, op, command, error)
    }
    if let OpOutcome::Error { error } = &outcome
        && let Command::SendPromptWithAttachments { agent, text, .. } = &pending.command
    {
        match model.agents.get(agent).map(|card| &card.agent.kind) {
            Some(model::AgentKind::Claude {
                driver: model::ClaudeDriver::Pty,
            }) => crate::claude::update::update_failed_command(
                model,
                op,
                &crate::claude::ClaudeCommand::SendPrompt {
                    agent: *agent,
                    text: text.clone(),
                },
                error,
            ),
            Some(model::AgentKind::Claude {
                driver: model::ClaudeDriver::Sdk,
            }) => crate::claude_sdk::update::update_failed_command(
                model,
                op,
                &crate::claude_sdk::ClaudeSdkCommand::SendPrompt {
                    agent: *agent,
                    text: text.clone(),
                },
                error,
            ),
            Some(model::AgentKind::Codex) => crate::codex::update::update_failed_command(
                model,
                op,
                &crate::codex::CodexCommand::Prompt {
                    agent: *agent,
                    text: text.clone(),
                },
                error,
            ),
            _ => {}
        }
    }
    if let OpOutcome::DiffFetched { id, patch } = &outcome
        && let Command::FetchDiff { agent, .. } = &pending.command
    {
        with_layer(model, *agent, |layer| match layer {
            AgentLayer::Claude(layer) => {
                layer
                    .attachments_mut()
                    .insert_diff(id.clone(), patch.clone());
            }
            AgentLayer::Codex(layer) => {
                layer
                    .attachments_mut()
                    .insert_diff(id.clone(), patch.clone());
            }
            AgentLayer::ClaudeSdk(layer) => {
                layer
                    .attachments_mut()
                    .insert_diff(id.clone(), patch.clone());
            }
        });
    }
    if crate::queue::observe_result(model, &pending, &outcome) {
        return Vec::new();
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
            model.remote_inventories.clear();
            model.connection = Connection::Connected {
                hosts_synchronized: false,
                agents_synchronized: false,
            };
            model.local_host_id = local_host_id.or(model.local_host_id);
            model.cloud_auth_required = false;
            model.cloud_subscription_required = false;
            crate::store::reconnect(&mut model.store)
        }
        ServerMsg::CloudSubscriptionStatus { required } => {
            model.cloud_subscription_required = required;
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
            model.attached.retain(|_, host| *host != id);
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
            if let Some(ids) = model.remote_inventories.get_mut(&agent.host_id) {
                ids.insert(agent_id);
            }
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
                        remembered: false,
                        local_summary: None,
                        layer: None,
                        epoch,
                        agent,
                    };
                    model.agents.insert(agent_id, card);
                }
            }
            if model.attached.contains_key(&agent_id) {
                ensure_stream(model, agent_id).into_iter().collect()
            } else {
                Vec::new()
            }
        }
        ServerMsg::AgentSummary { agent, envelope } => {
            if !model.is_connected() {
                return tripwire("agent summary while not connected");
            }
            if let Some(card) = model.agents.get_mut(&agent) {
                card.agent.summary = Some(envelope);
            }
            Vec::new()
        }
        ServerMsg::AgentProgress { agent, progress } => {
            if !model.is_connected() {
                return tripwire("agent progress while not connected");
            }
            if let Some(card) = model.agents.get_mut(&agent) {
                card.agent.progress = Some(progress);
            }
            Vec::new()
        }
        ServerMsg::AgentRemoved { id } => {
            if !model.is_connected() {
                return tripwire("agent removal while not connected");
            }
            crate::queue::remove(model, id);
            // Remote removal can race the separate host-offline event. Its
            // authoritative HostInventory, not reachability at this instant,
            // decides whether a requested conversation is really gone.
            if model.attached.get(&id).copied() == model.local_host_id {
                model.attached.remove(&id);
            }
            model.agents.remove(&id);
            if let Some(stream) = model.streams.remove(&id)
                && !matches!(stream.phase, StreamPhase::Closed { .. })
            {
                return vec![Effect::CloseStream { agent: id }];
            }
            Vec::new()
        }
        ServerMsg::HostInventory { host_id, agent_ids } => {
            if !model.is_connected() {
                return tripwire("host inventory while not connected");
            }
            let agent_ids: std::collections::BTreeSet<_> = agent_ids.into_iter().collect();
            model
                .attached
                .retain(|agent, host| *host != host_id || agent_ids.contains(agent));
            model.remote_inventories.insert(host_id, agent_ids);
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
            let epoch = model.epoch;
            for card in model.agents.values_mut().filter(|card| card.epoch == epoch) {
                card.remembered = false;
            }
            prune_if_synchronized(model)
        }
    }
}

fn update_chat_command(model: &mut Model, command: ChatCommand) -> Vec<Effect> {
    match command {
        ChatCommand::Open { agent } => {
            let Some(protocol) = model
                .agents
                .get(&agent)
                .and_then(AgentCard::structured_protocol)
            else {
                return Vec::new();
            };
            if model.store.chats.get(&agent).is_some_and(|chat| {
                !matches!(
                    chat.state,
                    crate::store::ChatState::Absent | crate::store::ChatState::Flushing
                )
            }) {
                return Vec::new();
            }
            crate::store::open_chat(&mut model.store, agent, protocol)
        }
        ChatCommand::PageOlder { agent, n } => crate::store::page_older(&mut model.store, agent, n),
        ChatCommand::Close { agent, now } => crate::store::close_chat(&mut model.store, agent, now),
        ChatCommand::FlushDeadline { agent, now } => {
            crate::store::flush_deadline(&mut model.store, agent, now)
        }
    }
}

fn mirror_chat_stream(
    model: &mut Model,
    agent: model::AgentId,
    event: crate::store::ChatStreamMsg,
) -> Vec<Effect> {
    let event = match event {
        crate::store::ChatStreamMsg::Opened { facts, .. } => StreamMsg::Opened {
            truncated: !matches!(facts.outcome, crate::store::ReplayOutcomeDto::Continuous),
        },
        crate::store::ChatStreamMsg::Batch { at, entries } => StreamMsg::Batch { at, entries },
        crate::store::ChatStreamMsg::ReplayComplete { .. } => StreamMsg::ReplayComplete,
        crate::store::ChatStreamMsg::Closed { reason, .. } => StreamMsg::Closed { reason },
    };
    update_stream(model, agent, event)
}

fn sync_chat_summary(model: &mut Model, agent: model::AgentId) {
    let local = model
        .store
        .chats
        .get(&agent)
        .and_then(|chat| chat.head.as_ref())
        .map(|head| LocalSummary {
            fold: head.agent_fold(),
            through: head.through(),
        });
    if let Some(card) = model.agents.get_mut(&agent) {
        card.local_summary = local;
    }
}

fn update_store_message(model: &mut Model, message: crate::store::StoreMsg) -> Vec<Effect> {
    let affected_agent = match &message {
        crate::store::StoreMsg::Loaded { agent, .. }
        | crate::store::StoreMsg::Committed { agent, .. }
        | crate::store::StoreMsg::Conflict { agent, .. }
        | crate::store::StoreMsg::Paged { agent, .. } => Some(*agent),
        crate::store::StoreMsg::Failed { agent, .. } => *agent,
        crate::store::StoreMsg::FleetLoaded { .. }
        | crate::store::StoreMsg::FleetApplied { .. }
        | crate::store::StoreMsg::FleetChanged { .. }
        | crate::store::StoreMsg::ViewLoaded { .. }
        | crate::store::StoreMsg::ViewSet { .. }
        | crate::store::StoreMsg::Unavailable { .. } => None,
    };
    let StoreUpdate { effects, fleet } = crate::store::update_store(&mut model.store, message);
    if let Some(fleet) = fleet {
        install_remembered_fleet(model, fleet);
    }
    if let Some(agent) = affected_agent {
        sync_chat_summary(model, agent);
    }
    effects
}

fn install_remembered_fleet(model: &mut Model, fleet: fold::Fleet) {
    for remembered in fleet.hosts {
        model.hosts.entry(remembered.host.id).or_insert(HostState {
            entry: remembered.host,
            epoch: model.epoch,
        });
    }
    for remembered in fleet.agents {
        if remembered.membership != fold::Membership::Cached {
            model.agents.remove(&remembered.agent.id);
            continue;
        }
        let agent = remembered.agent;
        let last_activity = agent
            .summary
            .as_ref()
            .and_then(|summary| summary.summary.last_activity)
            .unwrap_or(agent.created_at);
        if let Some(card) = model.agents.get_mut(&agent.id) {
            // Another client can advance the shared fleet fold without this
            // connection receiving a matching inventory upsert. Refresh the
            // durable facts while retaining this client's live stream layer.
            card.agent = agent;
            card.last_activity = card.last_activity.max(last_activity);
            card.epoch = model.epoch;
            continue;
        }
        model.agents.insert(
            agent.id,
            AgentCard {
                last_activity,
                provider_label: None,
                attention: Attention::Unknown,
                phase: AgentPhase::Running,
                remembered: true,
                local_summary: None,
                layer: None,
                epoch: model.epoch,
                agent,
            },
        );
    }
}

fn update_stream(model: &mut Model, agent: model::AgentId, event: StreamMsg) -> Vec<Effect> {
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
            crate::queue::reopened(model, agent);
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
            with_layer(model, agent, |layer| layer.begin_window(truncated));
            if let Some(card) = model.agents.get_mut(&agent)
                && let Some(protocol) =
                    AgentLayer::from_kind(&card.agent.kind).map(|layer| layer.protocol())
            {
                let mut fold = fold::AgentFold::for_protocol(protocol);
                let baseline = if truncated {
                    fold::Baseline::Truncated { from: 1 }
                } else {
                    fold::Baseline::Start
                };
                fold.begin(1, baseline);
                card.local_summary = Some(LocalSummary { fold, through: 0 });
            }
        }
        StreamMsg::Batch { at, entries } => {
            if let Some(card) = model.agents.get_mut(&agent) {
                card.last_activity = card.last_activity.max(at);
            }
            with_layer(model, agent, |layer| {
                for entry in &entries {
                    layer.observe(entry.seq, at, &entry.payload);
                }
            });
            if let Some(local) = model
                .agents
                .get_mut(&agent)
                .and_then(|card| card.local_summary.as_mut())
            {
                for entry in &entries {
                    let payload = serde_json::to_vec(&entry.payload)
                        .expect("a serde_json::Value always serializes as JSON");
                    let changes = local.fold.apply_summary(fold::Input::Row {
                        seq: entry.seq,
                        published_at: entry.published_at,
                        activity_at: entry.activity_at,
                        historical: entry.historical,
                        payload: &payload,
                    });
                    local.through = changes.through;
                }
            }
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
            with_layer(model, agent, AgentLayer::observe_replay_complete);
        }
        StreamMsg::Closed { reason } => {
            if reason == StreamCloseReason::AuthenticationRequired {
                model.cloud_auth_required = true;
            }
            if reason == StreamCloseReason::SubscriptionRequired {
                model.cloud_subscription_required = true;
            }
            match &reason {
                StreamCloseReason::AgentExited { exit_code } => {
                    let exit_code = *exit_code;
                    if let Some(card) = model.agents.get_mut(&agent) {
                        card.phase = AgentPhase::Exited { exit_code };
                        if let Some(local) = card.local_summary.as_mut() {
                            let at = model.now.unwrap_or(card.last_activity);
                            let changes = local
                                .fold
                                .apply_summary(fold::Input::ProcessExited { exit_code, at });
                            local.through = changes.through;
                        }
                    }
                    // Nothing is left to need: obligations do not outlive
                    // the process that owned them.
                    with_layer(model, agent, AgentLayer::observe_exit);
                }
                StreamCloseReason::AgentDeleted => {}
                // The stream died underneath us: whatever the fold knew is
                // stale. Degrade to Unknown, never to a wrong badge.
                _ => {
                    with_layer(model, agent, AgentLayer::invalidate);
                    if let Some(card) = model.agents.get_mut(&agent)
                        && let Some(local) = card.local_summary.as_mut()
                    {
                        let at = model.now.unwrap_or(card.last_activity);
                        let changes = local.fold.apply_summary(fold::Input::ObserverLost { at });
                        local.through = changes.through;
                    }
                }
            }
            if let Some(stream) = model.streams.get_mut(&agent) {
                stream.phase = StreamPhase::Closed { reason };
            }
            refresh_attention(model, agent);
        }
    }
    Vec::new()
}

/// Run a fold step on the typed native layer this agent's kind determines,
/// creating it on first evidence. Attention is
/// summarized from that same fold state and the provider's kernel-level
/// projection rule afterwards.
fn with_layer(model: &mut Model, agent: model::AgentId, step: impl FnOnce(&mut AgentLayer)) {
    {
        let Some(card) = model.agents.get_mut(&agent) else {
            return;
        };
        let Some(selected) = AgentLayer::from_kind(&card.agent.kind) else {
            return;
        };
        let layer = card.layer.get_or_insert(selected);
        step(layer);
    }
    refresh_attention(model, agent);
}

/// Refresh the card cache after either its typed fold or stream phase moves.
/// The `AgentLayer` dispatch applies the same stream-aware replay rule to
/// both layers.
fn refresh_attention(model: &mut Model, agent: model::AgentId) {
    let stream_phase = model.streams.get(&agent).map(|stream| &stream.phase);
    let Some(card) = model.agents.get_mut(&agent) else {
        return;
    };
    let Some(layer) = card.layer.as_ref() else {
        return;
    };
    card.attention = layer.attention(stream_phase);
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
    let removed: Vec<_> = model
        .agents
        .iter()
        .filter(|(_, card)| card.epoch != epoch)
        .map(|(id, _)| *id)
        .collect();
    for id in removed {
        crate::queue::remove(model, id);
    }
    model.agents.retain(|_, card| card.epoch == epoch);
    let stale: Vec<model::AgentId> = model
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

pub(crate) fn push_finished(model: &mut Model, finished: FinishedOp) {
    model.finished_ops.push(finished);
    if model.finished_ops.len() > FINISHED_OPS_RETAINED {
        let excess = model.finished_ops.len() - FINISHED_OPS_RETAINED;
        model.finished_ops.drain(..excess);
    }
}

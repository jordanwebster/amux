//! One classification of session facts; projections never inspect feed content.

use amux::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AskWhy, ClaudeSdkLayer};
use crate::{AgentPhase, Attention, Model, StreamPhase, Violation, Why};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum TurnState {
    #[default]
    Unknown,
    Idle,
    Working,
    Finished,
    Errored,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum SdkPhase {
    Unavailable,
    Exited,
    Replaying,
    Unknown,
    Idle,
    Working,
    Finished,
    Errored,
    Interrupted,
    NeedsYou { id: u64, why: AskWhy },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendGate {
    Ready,
    Unavailable,
    Exited,
    ReadOnly,
    Replaying,
    Working,
    NeedsYou,
    Unknown,
    InputInFlight,
}

impl SendGate {
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Unavailable => Some("chat input unavailable for this agent"),
            Self::Exited => Some("agent exited"),
            Self::ReadOnly => Some("agent is read-only — you are observing this session"),
            Self::Replaying => Some("send gated while replaying"),
            Self::Working => Some("send gated while working"),
            Self::NeedsYou => Some("send gated — answer the pending ask"),
            Self::Unknown => Some("send gated — session state unknown"),
            Self::InputInFlight => Some("send gated — an input is in flight"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SdkConditionState {
    Unavailable,
    Exited,
    Replaying,
    Unknown,
    Idle,
    Working,
    Finished,
    Errored,
    Interrupted,
    AskPending { id: u64, why: AskWhy },
}

struct SdkCondition {
    state: SdkConditionState,
    input_in_flight: bool,
    observer_readonly: bool,
}

impl SdkCondition {
    fn phase(&self) -> SdkPhase {
        match self.state {
            SdkConditionState::Unavailable => SdkPhase::Unavailable,
            SdkConditionState::Exited => SdkPhase::Exited,
            SdkConditionState::Replaying => SdkPhase::Replaying,
            SdkConditionState::Unknown => SdkPhase::Unknown,
            SdkConditionState::Idle => SdkPhase::Idle,
            SdkConditionState::Working => SdkPhase::Working,
            SdkConditionState::Finished => SdkPhase::Finished,
            SdkConditionState::Errored => SdkPhase::Errored,
            SdkConditionState::Interrupted => SdkPhase::Interrupted,
            SdkConditionState::AskPending { id, why } => SdkPhase::NeedsYou { id, why },
        }
    }

    fn attention(&self) -> Attention {
        match self.state {
            SdkConditionState::Unavailable
            | SdkConditionState::Replaying
            | SdkConditionState::Unknown => Attention::Unknown,
            SdkConditionState::Idle | SdkConditionState::Interrupted => Attention::Idle,
            SdkConditionState::Working => Attention::Working,
            SdkConditionState::Finished
            | SdkConditionState::Errored
            | SdkConditionState::Exited => Attention::NeedsYou { why: Why::Finished },
            SdkConditionState::AskPending { why, .. } => Attention::NeedsYou {
                why: match why {
                    AskWhy::Permission | AskWhy::Plan => Why::Permission,
                    AskWhy::Question | AskWhy::Elicitation | AskWhy::Dialog => Why::Question,
                },
            },
        }
    }

    fn send_gate(&self) -> SendGate {
        match self.state {
            SdkConditionState::Unavailable => return SendGate::Unavailable,
            SdkConditionState::Exited => return SendGate::Exited,
            _ => {}
        }
        if self.observer_readonly {
            return SendGate::ReadOnly;
        }
        match self.state {
            SdkConditionState::Replaying => SendGate::Replaying,
            SdkConditionState::Unknown => SendGate::Unknown,
            _ if self.input_in_flight => SendGate::InputInFlight,
            SdkConditionState::AskPending { .. } => SendGate::NeedsYou,
            SdkConditionState::Working => SendGate::Working,
            _ => SendGate::Ready,
        }
    }
}

fn classify(
    layer: Option<&ClaudeSdkLayer>,
    stream: Option<&StreamPhase>,
    agent: Option<&AgentPhase>,
    readonly: bool,
) -> SdkCondition {
    let mut condition = SdkCondition {
        state: SdkConditionState::Unavailable,
        input_in_flight: false,
        observer_readonly: readonly,
    };
    let Some(layer) = layer else {
        return condition;
    };
    condition.input_in_flight = layer.in_flight.is_some() || layer.echo.is_some();
    condition.state = if layer.exited || matches!(agent, Some(AgentPhase::Exited { .. })) {
        SdkConditionState::Exited
    } else if matches!(stream, Some(StreamPhase::Opening | StreamPhase::Replaying)) {
        SdkConditionState::Replaying
    } else if !matches!(stream, Some(StreamPhase::Live)) || layer.stale || layer.gap {
        SdkConditionState::Unknown
    } else if let Some(ask) = layer.ask_head() {
        SdkConditionState::AskPending {
            id: ask.id,
            why: ask.why(),
        }
    } else {
        match layer.turn {
            TurnState::Unknown => SdkConditionState::Unknown,
            TurnState::Idle => SdkConditionState::Idle,
            TurnState::Working => SdkConditionState::Working,
            TurnState::Finished => SdkConditionState::Finished,
            TurnState::Errored => SdkConditionState::Errored,
            TurnState::Interrupted => SdkConditionState::Interrupted,
        }
    };
    condition
}

fn classify_model(model: &Model, agent: AgentId) -> SdkCondition {
    let Some(card) = model.agent(agent) else {
        return classify(None, None, None, false);
    };
    // Losing the owning host degrades every public projection together.
    let stream = model
        .host_online(card.agent.host_id)
        .then(|| model.stream(agent))
        .flatten();
    let mut condition = classify(
        card.claude_sdk(),
        stream.map(|s| &s.phase),
        Some(&card.phase),
        card.agent.readonly,
    );
    if card.claude_sdk().is_some() && !model.host_online(card.agent.host_id) {
        condition.state = SdkConditionState::Unknown;
    }
    condition
}

pub fn phase(model: &Model, agent: AgentId) -> SdkPhase {
    classify_model(model, agent).phase()
}
pub fn send_gate(model: &Model, agent: AgentId) -> SendGate {
    classify_model(model, agent).send_gate()
}

impl ClaudeSdkLayer {
    pub fn attention(&self, stream: Option<&StreamPhase>) -> Attention {
        classify(Some(self), stream, None, false).attention()
    }
}

pub(super) fn observe(layer: &mut ClaudeSdkLayer, row: &Value) {
    let parent = !row["parent_tool_use_id"].is_null();
    match row["type"].as_str().unwrap_or("") {
        "amux.claude_sdk.ready" | "conversation_reset" => {
            layer.turn = TurnState::Idle;
            layer.gap = false;
            layer.stale = false;
            layer.interrupted = false;
            layer.asks.clear();
            layer.clear_inputs("session reset");
        }
        "amux.claude_sdk.gap" => {
            layer.gap = true;
            layer.turn = TurnState::Unknown;
        }
        "result" if !parent => {
            layer.turn = if layer.interrupted {
                TurnState::Interrupted
            } else if row["is_error"] == true {
                TurnState::Errored
            } else {
                TurnState::Finished
            };
            layer.interrupted = false;
        }
        "assistant" if !parent && row["message"]["id"].is_string() => working(layer),
        "stream_event"
            if !parent
                && row["event"]["type"] == "message_start"
                && row["event"]["message"]["id"].is_string() =>
        {
            working(layer)
        }
        "user" if !parent => {
            let content = &row["message"]["content"];
            let texts: Vec<_> = content
                .as_str()
                .into_iter()
                .chain(
                    content
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|b| b["text"].as_str()),
                )
                .collect();
            if texts.contains(&"[Request interrupted by user]") {
                layer.interrupted = true;
            } else if !texts.is_empty() && row["isSynthetic"] != true && row["isReplay"] != true {
                working(layer);
            }
        }
        "amux.claude_sdk.permission_required"
        | "amux.claude_sdk.elicitation_required"
        | "amux.claude_sdk.dialog_required" => working(layer),
        "system" if row["subtype"] == "status" && row["status"] == "compacting" => working(layer),
        _ => {}
    }
}

fn working(layer: &mut ClaudeSdkLayer) {
    layer.turn = TurnState::Working;
    layer.interrupted = false;
}

/// Independent relation over public values, including cached fleet attention.
/// This intentionally does not re-run the classifier to compute an expectation.
pub(crate) fn check_projection_invariant(model: &Model, agent: AgentId, out: &mut Vec<Violation>) {
    let Some(card) = model.agent(agent) else {
        return;
    };
    let phase = phase(model, agent);
    let gate = send_gate(model, agent);
    let attention = model.effective_attention(card);
    let (expected_attention, expected_gate) = match phase {
        SdkPhase::Unavailable => (Attention::Unknown, SendGate::Unavailable),
        SdkPhase::Unknown => (Attention::Unknown, SendGate::Unknown),
        SdkPhase::Replaying => (Attention::Unknown, SendGate::Replaying),
        SdkPhase::Exited => (Attention::NeedsYou { why: Why::Finished }, SendGate::Exited),
        SdkPhase::Idle | SdkPhase::Interrupted => (Attention::Idle, SendGate::Ready),
        SdkPhase::Working => (Attention::Working, SendGate::Working),
        SdkPhase::Finished | SdkPhase::Errored => {
            (Attention::NeedsYou { why: Why::Finished }, SendGate::Ready)
        }
        SdkPhase::NeedsYou {
            why: AskWhy::Permission | AskWhy::Plan,
            ..
        } => (
            Attention::NeedsYou {
                why: Why::Permission,
            },
            SendGate::NeedsYou,
        ),
        SdkPhase::NeedsYou { .. } => (
            Attention::NeedsYou { why: Why::Question },
            SendGate::NeedsYou,
        ),
    };
    let expected_gate =
        if !matches!(phase, SdkPhase::Unavailable | SdkPhase::Exited) && card.agent.readonly {
            SendGate::ReadOnly
        } else if !matches!(
            phase,
            SdkPhase::Unavailable | SdkPhase::Exited | SdkPhase::Unknown | SdkPhase::Replaying
        ) && card
            .claude_sdk()
            .is_some_and(|l| l.in_flight.is_some() || l.echo.is_some())
        {
            SendGate::InputInFlight
        } else {
            expected_gate
        };
    if attention != expected_attention
        || gate != expected_gate
        || (model.host_online(card.agent.host_id) && card.attention != expected_attention)
    {
        out.push(Violation::ClaudeSdkProjection {
            agent,
            phase,
            attention,
            gate,
        });
    }
}

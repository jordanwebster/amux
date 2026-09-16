//! Pending SDK obligations with reducer-local answer state.

use ::fold::claude_sdk::{AskKind, AskObservation, AskWhy, observe_ask};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ClaudeSdkLayer;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub id: u64,
    pub request_id: String,
    pub kind: AskKind,
    pub input: Value,
    pub suggestions: Vec<Value>,
    pub state: AskState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AskState {
    Pending,
    AnsweredOptimistic {
        op: crate::OpId,
        answer: super::SdkAnswer,
    },
    SendFailed {
        message: String,
    },
}

impl Ask {
    pub fn why(&self) -> AskWhy {
        self.kind.why()
    }

    pub(super) fn channel(&self) -> &'static str {
        match self.kind {
            AskKind::Permission { .. } | AskKind::Plan { .. } | AskKind::Question { .. } => {
                "permission"
            }
            AskKind::Elicitation { .. } => "elicitation",
            AskKind::Dialog { .. } => "dialog",
        }
    }
}

pub(super) fn observe(layer: &mut ClaudeSdkLayer, row: &Value) {
    if layer.exited {
        return;
    }
    match observe_ask(row) {
        Some(AskObservation::Resolved {
            channel,
            request_id,
        }) => {
            layer
                .asks
                .retain(|ask| ask.request_id != request_id || ask.channel() != channel);
        }
        Some(AskObservation::Required {
            channel,
            request_id,
            kind,
            input,
            suggestions,
        }) => {
            if layer
                .asks
                .iter()
                .any(|ask| ask.request_id == request_id && ask.channel() == channel)
            {
                return;
            }
            layer.asks.push_back(Ask {
                id: layer.next_ask_id,
                request_id,
                kind,
                input,
                suggestions,
                state: AskState::Pending,
            });
            layer.next_ask_id += 1;
        }
        None => {}
    }
}

/// A dialog payload a person can answer from a chat: a message and the
/// labelled choices it offers, in the payload's own order.
///
/// No dialog kind has ever been recorded, so this is deliberately the one
/// shape the answer encoder can express. Deriving it here rather than in each
/// panel keeps a dialog from being offered as answerable on screen and then
/// refused at dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogChoices {
    pub message: String,
    pub options: Vec<DialogChoice>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogChoice {
    pub label: String,
    pub description: Option<String>,
}

/// The choices a dialog payload offers, or `None` when its shape is not one
/// this build can answer.
pub fn dialog_choices(payload: &Value) -> Option<DialogChoices> {
    let message = payload["message"].as_str()?;
    let options = payload["options"].as_array().filter(|options| {
        !options.is_empty()
            && options
                .iter()
                .all(|option| option["label"].as_str().is_some_and(|s| !s.is_empty()))
    })?;
    Some(DialogChoices {
        message: message.to_owned(),
        options: options
            .iter()
            .map(|option| DialogChoice {
                label: option["label"].as_str().unwrap_or_default().to_owned(),
                description: option["description"].as_str().map(str::to_owned),
            })
            .collect(),
    })
}

/// What an unanswerable payload holds, in bounded words. Raw JSON is never
/// shown: a person deciding whether to cancel needs to know the shape of what
/// they are declining, not its bytes.
pub fn dialog_payload_summary(payload: &Value) -> String {
    match payload {
        Value::Object(fields) if fields.is_empty() => "an empty object".to_string(),
        Value::Object(fields) => {
            let named: Vec<String> = fields.keys().take(6).map(|key| sanitize(key)).collect();
            let more = fields.len().saturating_sub(named.len());
            let list = named.join(", ");
            let ellipsis = if more > 0 { ", …" } else { "" };
            format!(
                "object with {} field{} ({list}{ellipsis})",
                fields.len(),
                if fields.len() == 1 { "" } else { "s" }
            )
        }
        Value::Array(items) => format!(
            "list of {} item{}",
            items.len(),
            if items.len() == 1 { "" } else { "s" }
        ),
        Value::String(text) => format!("text: {}", sanitize(text)),
        Value::Null => "nothing".to_string(),
        other => format!("value: {}", sanitize(&other.to_string())),
    }
}

/// One bounded, control-free fragment of a provider's own words.
fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(40)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if text.chars().count() > 40 {
        format!("{cleaned}…")
    } else {
        cleaned
    }
}

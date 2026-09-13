//! Pending provider obligations are independent of the retained transcript.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ClaudeSdkLayer;
use crate::claude::facts::{
    QuestionFact, SuggestionDestination, SuggestionFact, SuggestionKind, ToolInvocation, invocation,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub id: u64,
    pub request_id: String,
    pub kind: AskKind,
    /// Original tool input is needed when an answer updates only some fields.
    pub input: Value,
    /// Preserve provider updates in full; display facts omit opaque rule data.
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskKind {
    Permission {
        tool_name: String,
        invocation: ToolInvocation,
        suggestions: Vec<SuggestionFact>,
    },
    Plan {
        plan: Option<String>,
        plan_file_path: Option<String>,
    },
    Question {
        questions: Vec<QuestionFact>,
    },
    Elicitation {
        server: Option<String>,
        message: String,
        form: ElicitationForm,
    },
    Dialog {
        dialog_kind: String,
        payload: Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskWhy {
    Permission,
    Plan,
    Question,
    Elicitation,
    Dialog,
}

impl Ask {
    pub fn why(&self) -> AskWhy {
        match self.kind {
            AskKind::Permission { .. } => AskWhy::Permission,
            AskKind::Plan { .. } => AskWhy::Plan,
            AskKind::Question { .. } => AskWhy::Question,
            AskKind::Elicitation { .. } => AskWhy::Elicitation,
            AskKind::Dialog { .. } => AskWhy::Dialog,
        }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", content = "content", rename_all = "snake_case")]
pub enum ElicitationForm {
    Fields(Vec<ElicitationField>),
    Unsupported { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElicitationField {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub required: bool,
    pub kind: ElicitationFieldKind,
    pub default: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "values", rename_all = "snake_case")]
pub enum ElicitationFieldKind {
    String,
    Number,
    Integer,
    Boolean,
    Enum(Vec<Value>),
}

impl ElicitationForm {
    pub fn from_schema(schema: &Value) -> Self {
        match fields(schema) {
            Ok(fields) => Self::Fields(fields),
            Err(reason) => Self::Unsupported { reason },
        }
    }
}

/// One field per property of the schema, ordered by field name. A JSON
/// object's keys do not keep the order they were written in once the row
/// has been read, so the declaration order the server intended is not
/// available here; name order is the one order that is stable across
/// every reading of the same schema.
fn fields(schema: &Value) -> Result<Vec<ElicitationField>, String> {
    let root = schema.as_object().ok_or("form schema is not an object")?;
    if schema["type"] != "object" {
        return Err("form schema must describe an object".into());
    }
    for key in root.keys() {
        if !matches!(
            key.as_str(),
            "type"
                | "properties"
                | "required"
                | "title"
                | "description"
                | "$schema"
                | "additionalProperties"
        ) {
            return Err(format!("unsupported form schema keyword: {key}"));
        }
    }
    if root
        .get("additionalProperties")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err("additional property schemas are not supported".into());
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or("form schema has no properties object")?;
    let required = match root.get("required") {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| v.as_str().ok_or("required field name is not text"))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("required fields must be an array".into()),
    };
    if required.iter().any(|name| !properties.contains_key(*name)) {
        return Err("required field has no property schema".into());
    }
    properties
        .iter()
        .map(|(name, property)| {
            let object = property
                .as_object()
                .ok_or_else(|| format!("{name}: property schema is not an object"))?;
            for key in object.keys() {
                if !matches!(
                    key.as_str(),
                    "type" | "title" | "description" | "default" | "enum"
                ) {
                    return Err(format!("{name}: unsupported field keyword: {key}"));
                }
            }
            let kind = match property["type"].as_str() {
                Some("string") => ElicitationFieldKind::String,
                Some("number") => ElicitationFieldKind::Number,
                Some("integer") => ElicitationFieldKind::Integer,
                Some("boolean") => ElicitationFieldKind::Boolean,
                _ => {
                    return Err(format!(
                        "{name}: only text, number, boolean and enum fields are supported"
                    ));
                }
            };
            let accepts = |value: &Value| match kind {
                ElicitationFieldKind::String => value.is_string(),
                ElicitationFieldKind::Number => value.is_number(),
                ElicitationFieldKind::Integer => value.is_i64() || value.is_u64(),
                ElicitationFieldKind::Boolean => value.is_boolean(),
                ElicitationFieldKind::Enum(_) => unreachable!(),
            };
            let choices = match object.get("enum") {
                None => None,
                Some(Value::Array(values)) if !values.is_empty() && values.iter().all(&accepts) => {
                    Some(values.clone())
                }
                Some(_) => return Err(format!("{name}: enum choices do not match the field type")),
            };
            let default = object.get("default").cloned();
            if default
                .as_ref()
                .is_some_and(|v| !accepts(v) || choices.as_ref().is_some_and(|c| !c.contains(v)))
            {
                return Err(format!("{name}: default does not match the field"));
            }
            Ok(ElicitationField {
                name: name.clone(),
                title: text(property, "title"),
                description: text(property, "description"),
                required: required.contains(&name.as_str()),
                kind: choices.map(ElicitationFieldKind::Enum).unwrap_or(kind),
                default,
            })
        })
        .collect()
}

pub(super) fn observe(layer: &mut ClaudeSdkLayer, row: &Value) {
    if layer.exited {
        return;
    }
    let Some(tag) = row["type"]
        .as_str()
        .and_then(|t| t.strip_prefix("amux.claude_sdk."))
    else {
        return;
    };
    let Some((channel, action)) = tag.rsplit_once('_') else {
        return;
    };
    if !matches!(channel, "permission" | "elicitation" | "dialog") {
        return;
    }
    let Some(request_id) = row["request_id"].as_str() else {
        return;
    };
    if action == "resolved" {
        layer
            .asks
            .retain(|ask| ask.request_id != request_id || ask.channel() != channel);
        return;
    }
    if action != "required"
        || layer
            .asks
            .iter()
            .any(|ask| ask.request_id == request_id && ask.channel() == channel)
    {
        return;
    }
    let suggestions: Vec<Value> = row["suggestions"].as_array().cloned().unwrap_or_default();
    let input = row["input"].clone();
    let kind = match channel {
        "permission" => {
            let tool_name = text(row, "tool_name").unwrap_or_default();
            match invocation(&tool_name, &input) {
                ToolInvocation::Plan {
                    plan,
                    plan_file_path,
                } => AskKind::Plan {
                    plan,
                    plan_file_path,
                },
                ToolInvocation::Question { questions } => AskKind::Question { questions },
                invocation => AskKind::Permission {
                    tool_name,
                    invocation,
                    suggestions: suggestions
                        .iter()
                        .map(|v| SuggestionFact {
                            kind: v["type"].as_str().map(SuggestionKind::from_wire),
                            destination: v["destination"]
                                .as_str()
                                .map(SuggestionDestination::from_wire),
                            directories: v["directories"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect(),
                        })
                        .collect(),
                },
            }
        }
        "elicitation" => AskKind::Elicitation {
            server: text(row, "server"),
            message: text(row, "message").unwrap_or_default(),
            form: ElicitationForm::from_schema(&row["schema"]),
        },
        "dialog" => AskKind::Dialog {
            dialog_kind: text(row, "dialog_kind").unwrap_or_default(),
            payload: row["payload"].clone(),
        },
        _ => unreachable!(),
    };
    layer.asks.push_back(Ask {
        id: layer.next_ask_id,
        request_id: request_id.into(),
        kind,
        input,
        suggestions,
        state: AskState::Pending,
    });
    layer.next_ask_id += 1;
}

fn text(row: &Value, key: &str) -> Option<String> {
    row[key].as_str().map(str::to_owned)
}

/// A dialog payload a person can answer from a chat: a message and the
/// labelled choices it offers, in the payload's own order.
///
/// No dialog kind has ever been recorded, so this is deliberately the one
/// shape the answer encoder can express. Deriving it here rather than in
/// each panel is what keeps a dialog from being offered as answerable on
/// screen and then refused at dispatch.
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

/// The choices a dialog payload offers, or `None` when its shape is not
/// one this build can answer.
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

/// What an unanswerable payload holds, in bounded words. Raw JSON is
/// never shown: a person deciding whether to cancel needs to know the
/// shape of what they are declining, not its bytes.
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

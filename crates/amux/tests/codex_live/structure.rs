//! Parsed Codex row predicates exposed as live-spec accessors.

use anyhow::{Context, Result};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Row {
    pub seq: u64,
    pub json: Value,
}

impl Row {
    pub fn parse(seq: u64, raw: &[u8]) -> Result<Self> {
        Ok(Self {
            seq,
            json: serde_json::from_slice(raw).context("parse Codex structured row")?,
        })
    }

    pub fn row_type(&self) -> Option<&str> {
        self.json.get("type").and_then(Value::as_str)
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.json.get("threadId").and_then(Value::as_str)
    }

    pub fn completed_agent_text(&self) -> Option<&str> {
        (self.row_type() == Some("item/completed")
            && self.json.pointer("/item/type").and_then(Value::as_str) == Some("agentMessage"))
        .then(|| self.json.pointer("/item/text").and_then(Value::as_str))
        .flatten()
    }

    pub fn message(&self) -> Option<(&str, &str)> {
        (self.row_type() == Some("amux.codex_message"))
            .then(|| {
                Some((
                    self.json.get("kind")?.as_str()?,
                    self.json.get("text")?.as_str()?,
                ))
            })
            .flatten()
    }
}

#[derive(Clone, Debug)]
pub enum Matcher {
    Type(&'static str),
    InputOk(Vec<u8>),
    GapReason(&'static str),
    TurnCompleted(&'static str),
    AgentTextContains(String),
    MessageContains { kind: &'static str, text: String },
}

impl Matcher {
    pub fn matches(&self, row: &Row) -> bool {
        match self {
            Self::Type(expected) => row.row_type() == Some(expected),
            Self::InputOk(input_id) => {
                row.row_type() == Some("amux.input_result")
                    && row.json.get("ok").is_some_and(Value::is_object)
                    && row.json.get("input_id") == Some(&bytes_value(input_id))
            }
            Self::GapReason(reason) => {
                row.row_type() == Some("amux.codex_gap")
                    && row.json.get("reason").and_then(Value::as_str) == Some(*reason)
            }
            Self::TurnCompleted(status) => {
                row.row_type() == Some("turn/completed")
                    && row.json.pointer("/turn/status").and_then(Value::as_str) == Some(*status)
            }
            Self::AgentTextContains(expected) => row
                .completed_agent_text()
                .is_some_and(|text| text.contains(expected)),
            Self::MessageContains { kind, text } => {
                row.message().is_some_and(|(actual_kind, actual_text)| {
                    actual_kind == *kind && actual_text.contains(text)
                })
            }
        }
    }
}

fn bytes_value(bytes: &[u8]) -> Value {
    Value::Array(bytes.iter().copied().map(Value::from).collect())
}

pub fn find_match(rows: &[Row], from: usize, matcher: &Matcher) -> Option<usize> {
    rows.iter()
        .enumerate()
        .skip(from)
        .find(|(_, row)| matcher.matches(row))
        .map(|(index, _)| index)
}

//! Parsed Codex row predicates shared by the live harness and offline tests.

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

    pub fn turn_id(&self) -> Option<&str> {
        self.json.pointer("/turn/id").and_then(Value::as_str)
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
}

#[derive(Clone, Debug)]
pub enum Matcher {
    Type(&'static str),
    InputOk(Vec<u8>),
    ApprovalRequired,
    ApprovalResolved(Value),
    GapReason(&'static str),
    TurnStarted,
    TurnCompleted(&'static str),
    AgentTextContains(String),
    AgentMessageContains { kind: &'static str, text: String },
    CommandCompleted(&'static str),
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
            Self::ApprovalRequired => row.row_type() == Some("amux.codex_approval_required"),
            Self::ApprovalResolved(request_id) => {
                row.row_type() == Some("amux.codex_approval_resolved")
                    && row.json.get("request_id") == Some(request_id)
                    && row.json.get("reason").and_then(Value::as_str) == Some("answered")
            }
            Self::GapReason(reason) => {
                row.row_type() == Some("amux.codex_gap")
                    && row.json.get("reason").and_then(Value::as_str) == Some(*reason)
            }
            Self::TurnStarted => row.row_type() == Some("turn/started") && row.turn_id().is_some(),
            Self::TurnCompleted(status) => {
                row.row_type() == Some("turn/completed")
                    && row.json.pointer("/turn/status").and_then(Value::as_str) == Some(*status)
            }
            Self::AgentTextContains(expected) => row
                .completed_agent_text()
                .is_some_and(|text| text.contains(expected)),
            Self::AgentMessageContains { kind, text } => {
                row.row_type() == Some("amux.codex_message")
                    && row.json.get("kind").and_then(Value::as_str) == Some(*kind)
                    && row
                        .json
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|body| body.contains(text.as_str()))
            }
            Self::CommandCompleted(status) => {
                row.row_type() == Some("item/completed")
                    && row.json.pointer("/item/type").and_then(Value::as_str)
                        == Some("commandExecution")
                    && row.json.pointer("/item/status").and_then(Value::as_str) == Some(*status)
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

#[allow(dead_code)]
pub fn parse_jsonl(input: &str) -> Result<Vec<Row>> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| Row::parse(index as u64 + 1, line.as_bytes()))
        .collect()
}

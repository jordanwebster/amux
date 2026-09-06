//! Session facts persist independently of the retained conversation feed.

pub use amux::claude_sdk_io::{ContextMeter, ContextMeterSource, ContextUsage, McpServerFact};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ClaudeSdkLayer;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFacts {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub context: Option<ContextMeter>,
    pub mcp_servers: Vec<McpServerFact>,
    pub slash_commands: Vec<String>,
    pub terminal_slash_commands: Vec<String>,
}

pub(super) fn observe(layer: &mut ClaudeSdkLayer, row: &Value) {
    if !row["parent_tool_use_id"].is_null() {
        return;
    }
    match row["type"].as_str().unwrap_or("") {
        "amux.claude_sdk.session_facts" => {
            layer.session.model = row["model"].as_str().map(str::to_owned);
            layer.session.effort = row["effort"].as_str().map(str::to_owned);
            layer.session.terminal_slash_commands =
                serde_json::from_value(row["terminal_slash_commands"].clone()).unwrap_or_default();
            layer.session.slash_commands =
                serde_json::from_value(row["slash_commands"].clone()).unwrap_or_default();
            layer.session.permission_mode = row["permission_mode"].as_str().map(str::to_owned);
            layer.session.context = serde_json::from_value(row["context"].clone()).ok();
            layer.session.mcp_servers =
                serde_json::from_value(row["mcp_servers"].clone()).unwrap_or_default();
        }
        "system" if row["subtype"] == "init" => {
            layer.session.model = row["model"].as_str().map(str::to_owned);
            layer.session.permission_mode = row["permissionMode"].as_str().map(str::to_owned);
            layer.session.mcp_servers =
                serde_json::from_value(row["mcp_servers"].clone()).unwrap_or_default();
            layer.session.slash_commands =
                serde_json::from_value(row["slash_commands"].clone()).unwrap_or_default();
            layer.session.terminal_slash_commands =
                serde_json::from_value(row["terminal_slash_commands"].clone()).unwrap_or_default();
        }
        "amux.claude_sdk.context_breakdown" => {
            layer.context_breakdown = serde_json::from_value(row["usage"].clone()).ok();
        }
        "amux.claude_sdk.ready" => {
            layer.session = SessionFacts::default();
            layer.context_breakdown = None;
        }
        "conversation_reset" => {
            layer.session.context = None;
            layer.context_breakdown = None;
        }
        "system" if row["subtype"] == "compact_boundary" => {
            // The separately requested breakdown describes the old context.
            layer.session.context = None;
            layer.context_breakdown = None;
        }
        _ => {}
    }
}

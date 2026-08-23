//! Shared model-facing contract for amux agent tools.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const AGENTS_DESCRIPTION: &str = "List the amux fleet, including agent kinds, hosts, liveness, current work, parents, and which agent is you.";
pub const SEND_DESCRIPTION: &str = "Send a text message to another amux agent by name. When a message comes from an `amux:` address, reply only with this tool, using the agent name from that address; Claude's native SendMessage cannot route that address. Use amux for cross-kind communication, such as Claude to Codex, and keep native Claude messaging for same-kind work.";
pub const SPAWN_DESCRIPTION: &str = "Create a Claude or Codex child agent with an initial prompt. Use amux spawn for cross-kind delegation and keep Claude's native Agent tool for same-kind work. When cwd is omitted, the child inherits your working directory; it also inherits your permission mode or approval and sandbox policy.";
pub const STOP_DESCRIPTION: &str = "Stop one of your amux child agents by name.";
pub const STATUS_DESCRIPTION: &str = "Set or clear your current amux work status so agents and humans can find the right collaborator.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentToolRequest {
    Agents,
    Send {
        to: String,
        text: String,
        context: Option<Uuid>,
    },
    Spawn {
        kind: AgentSpawnKind,
        prompt: String,
        name: Option<String>,
        cwd: Option<PathBuf>,
    },
    Stop {
        name: String,
    },
    Status {
        working_on: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentSpawnKind {
    Claude,
    Codex,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArguments {
    to: String,
    text: String,
    #[serde(default)]
    context: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArguments {
    kind: AgentSpawnKind,
    prompt: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopArguments {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArguments {
    working_on: Option<String>,
}

pub fn definitions() -> Vec<AgentToolDefinition> {
    vec![
        AgentToolDefinition {
            name: "agents",
            description: AGENTS_DESCRIPTION,
            input_schema: object_schema(json!({}), &[]),
        },
        AgentToolDefinition {
            name: "send",
            description: SEND_DESCRIPTION,
            input_schema: object_schema(
                json!({
                    "to": { "type": "string" },
                    "text": { "type": "string" },
                    "context": { "type": "string", "format": "uuid" }
                }),
                &["to", "text"],
            ),
        },
        AgentToolDefinition {
            name: "spawn",
            description: SPAWN_DESCRIPTION,
            input_schema: object_schema(
                json!({
                    "kind": { "type": "string", "enum": ["claude", "codex"] },
                    "prompt": { "type": "string" },
                    "name": { "type": "string" },
                    "cwd": { "type": "string" }
                }),
                &["kind", "prompt"],
            ),
        },
        AgentToolDefinition {
            name: "stop",
            description: STOP_DESCRIPTION,
            input_schema: object_schema(json!({ "name": { "type": "string" } }), &["name"]),
        },
        AgentToolDefinition {
            name: "status",
            description: STATUS_DESCRIPTION,
            input_schema: object_schema(
                json!({ "working_on": { "type": ["string", "null"] } }),
                &["working_on"],
            ),
        },
    ]
}

pub fn parse_call(name: &str, arguments: Value) -> Result<AgentToolRequest> {
    match name {
        "agents" => {
            ensure_empty_arguments(&arguments)?;
            Ok(AgentToolRequest::Agents)
        }
        "send" => {
            let args: SendArguments = parse_arguments(arguments)?;
            Ok(AgentToolRequest::Send {
                to: args.to,
                text: args.text,
                context: args.context,
            })
        }
        "spawn" => {
            let args: SpawnArguments = parse_arguments(arguments)?;
            Ok(AgentToolRequest::Spawn {
                kind: args.kind,
                prompt: args.prompt,
                name: args.name,
                cwd: args.cwd,
            })
        }
        "stop" => {
            let args: StopArguments = parse_arguments(arguments)?;
            Ok(AgentToolRequest::Stop { name: args.name })
        }
        "status" => {
            if arguments.get("working_on").is_none() {
                return Err(anyhow!("status requires working_on (a string or null)"));
            }
            let args: StatusArguments = parse_arguments(arguments)?;
            Ok(AgentToolRequest::Status {
                working_on: args.working_on,
            })
        }
        name => Err(anyhow!("unknown tool: {name}")),
    }
}

fn parse_arguments<T>(arguments: Value) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(arguments).context("invalid tool arguments")
}

fn ensure_empty_arguments(arguments: &Value) -> Result<()> {
    match arguments.as_object() {
        Some(arguments) if arguments.is_empty() => Ok(()),
        Some(_) => Err(anyhow!("agents takes no arguments")),
        None => Err(anyhow!("tool arguments must be an object")),
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

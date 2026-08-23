use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use amux::agent_tools::{
    AgentSpawnKind as SpawnKind, AgentToolRequest as ToolRequest, definitions as tool_definitions,
    parse_call as parse_tool_call,
};
use amux::{
    Agent, AgentIdentifier, AgentParent, AgentType, Client, Config, CreateAgentRequest,
    SendMessageRequest, SetAgentStatusRequest,
};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::client_common::require_running_client;

const JSONRPC_VERSION: &str = "2.0";
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default = "empty_object")]
    params: Value,
}

#[async_trait]
trait ToolBackend: Send + Sync {
    async fn call(&self, request: ToolRequest) -> Result<Value>;
}

struct ClientBackend {
    client: Client,
    caller: Option<Uuid>,
}

pub(super) async fn serve_claude(config: &Config) -> Result<()> {
    let client = require_running_client(config, None).await?;
    let backend = Arc::new(ClientBackend {
        client,
        caller: caller_from_env(std::env::var_os("AMUX_AGENT_ID"))?,
    });
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(stdin.lock(), stdout.lock(), backend).await
}

async fn serve<R, W, B>(mut reader: R, mut writer: W, backend: Arc<B>) -> Result<()>
where
    R: BufRead,
    W: Write,
    B: ToolBackend,
{
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .context("failed to read MCP request")?;
        if read == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = handle_line(trimmed, backend.as_ref()).await {
            serde_json::to_writer(&mut writer, &response)
                .context("failed to encode MCP response")?;
            writer
                .write_all(b"\n")
                .context("failed to terminate MCP response")?;
            writer.flush().context("failed to flush MCP response")?;
        }
    }
}

async fn handle_line(line: &str, backend: &dyn ToolBackend) -> Option<Value> {
    let request = match serde_json::from_str::<RpcRequest>(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(rpc_error(
                Value::Null,
                -32700,
                format!("parse error: {error}"),
            ));
        }
    };
    let id = request.id?;
    if request.jsonrpc != JSONRPC_VERSION {
        return Some(rpc_error(
            id,
            -32600,
            format!("unsupported jsonrpc version: {}", request.jsonrpc),
        ));
    }
    Some(match request.method.as_str() {
        "initialize" => initialize_response(id, &request.params),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": tool_definitions() })),
        "tools/call" => match map_tool_call(request.params) {
            Ok(tool_request) => match backend.call(tool_request).await {
                Ok(output) => tool_result(id, output, false),
                Err(error) => tool_result(id, Value::String(format!("{error:#}")), true),
            },
            Err(error) => rpc_error(id, -32602, error.to_string()),
        },
        _ => rpc_error(id, -32601, format!("method not found: {}", request.method)),
    })
}

fn initialize_response(id: Value, params: &Value) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    rpc_result(
        id,
        json!({
            "protocolVersion": protocol_version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "amux",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn tool_result(id: Value, output: Value, is_error: bool) -> Value {
    let text = match output {
        Value::String(text) if is_error => text,
        output => serde_json::to_string(&output).expect("JSON values always serialize"),
    };
    rpc_result(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error
        }),
    )
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn map_tool_call(params: Value) -> Result<ToolRequest> {
    let call: ToolCallParams =
        serde_json::from_value(params).context("tools/call params must name a tool")?;
    parse_tool_call(&call.name, call.arguments)
}

fn empty_object() -> Value {
    json!({})
}

fn caller_from_env(value: Option<OsString>) -> Result<Option<Uuid>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow!("AMUX_AGENT_ID is not valid UTF-8"))?;
    Uuid::parse_str(&value)
        .map(Some)
        .context("AMUX_AGENT_ID is not a valid UUID")
}

fn caller_parent(
    caller: Option<Uuid>,
    host_for_agent: impl FnOnce(Uuid) -> Option<Uuid>,
) -> Result<Option<AgentParent>> {
    let Some(agent_id) = caller else {
        return Ok(None);
    };
    let host_id = host_for_agent(agent_id)
        .ok_or_else(|| anyhow!("amux agent identity is not present in the fleet"))?;
    Ok(Some(AgentParent { agent_id, host_id }))
}

fn send_request(
    caller: Option<Uuid>,
    to: String,
    text: String,
    context: Option<Uuid>,
) -> SendMessageRequest {
    SendMessageRequest {
        to: AgentIdentifier::from(to),
        text,
        context,
        from_agent_id: caller,
    }
}

fn status_request(
    caller: Option<Uuid>,
    working_on: Option<String>,
) -> Result<SetAgentStatusRequest> {
    let agent_id = caller.ok_or_else(|| anyhow!("amux agent identity is unavailable"))?;
    Ok(SetAgentStatusRequest {
        agent: AgentIdentifier::Id(agent_id),
        working_on,
    })
}

#[async_trait]
impl ToolBackend for ClientBackend {
    async fn call(&self, request: ToolRequest) -> Result<Value> {
        match request {
            ToolRequest::Agents => self.list_agents().await,
            ToolRequest::Send { to, text, context } => {
                let id = self
                    .client
                    .send_message(send_request(self.caller, to, text, context))
                    .await?;
                Ok(json!({ "id": id }))
            }
            ToolRequest::Spawn {
                kind,
                prompt,
                name,
                cwd,
            } => {
                let fleet = if self.caller.is_some() {
                    self.client.list_agents().await?
                } else {
                    Vec::new()
                };
                let caller_agent = self
                    .caller
                    .and_then(|caller| fleet.iter().find(|agent| agent.id == caller));
                let working_dir = spawn_working_dir(cwd, caller_agent)?;
                let parent = caller_parent(self.caller, |caller| {
                    fleet
                        .iter()
                        .find(|agent| agent.id == caller)
                        .map(|agent| agent.host_id)
                })?;
                let agent = self
                    .client
                    .create_agent(CreateAgentRequest {
                        agent_id: Uuid::new_v4(),
                        host_id: None,
                        name,
                        agent_type: match kind {
                            SpawnKind::Claude => AgentType::Claude,
                            SpawnKind::Codex => AgentType::Codex {
                                model: None,
                                approval_policy: None,
                                sandbox_policy: None,
                                resume_thread_id: None,
                            },
                        },
                        working_dir,
                        terminal_size: None,
                        args: Vec::new(),
                        parent,
                        initial_prompt: Some(prompt),
                    })
                    .await?;
                Ok(json!({
                    "name": display_agent_name(&agent),
                    "id": agent.id
                }))
            }
            ToolRequest::Stop { name } => {
                self.client.delete_agent(name).await?;
                Ok(json!({}))
            }
            ToolRequest::Status { working_on } => {
                self.client
                    .set_agent_status(status_request(self.caller, working_on)?)
                    .await?;
                Ok(json!({}))
            }
        }
    }
}

fn spawn_working_dir(cwd: Option<PathBuf>, caller: Option<&Agent>) -> Result<PathBuf> {
    cwd.or_else(|| caller.map(|agent| agent.working_dir.clone()))
        .map(Ok)
        .unwrap_or_else(|| {
            std::env::current_dir().context("failed to determine the child working directory")
        })
}

impl ClientBackend {
    async fn list_agents(&self) -> Result<Value> {
        let agents = self.client.list_agents().await?;
        let hosts = self.client.list_hosts().await?;
        let hosts: HashMap<_, _> = hosts.into_iter().map(|host| (host.id, host)).collect();
        let names: HashMap<_, _> = agents
            .iter()
            .map(|agent| (agent.id, display_agent_name(agent)))
            .collect();
        let fleet: Vec<_> = agents
            .iter()
            .map(|agent| {
                let host = hosts.get(&agent.host_id);
                json!({
                    "name": display_agent_name(agent),
                    "kind": agent.agent_type,
                    "host": host.map(|host| host.name.clone()).unwrap_or_else(|| agent.host_id.to_string()),
                    "alive": host.is_some_and(|host| host.online),
                    "working_on": agent.working_on.as_ref().map(|work| work.text.clone()),
                    "parent": agent.parent.map(|parent| {
                        names.get(&parent.agent_id).cloned().unwrap_or_else(|| parent.agent_id.to_string())
                    }),
                    "you": self.caller == Some(agent.id)
                })
            })
            .collect();
        Ok(Value::Array(fleet))
    }
}

fn display_agent_name(agent: &Agent) -> String {
    agent.name.clone().unwrap_or_else(|| agent.id.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingBackend {
        calls: Mutex<Vec<ToolRequest>>,
    }

    #[async_trait]
    impl ToolBackend for RecordingBackend {
        async fn call(&self, request: ToolRequest) -> Result<Value> {
            let output = match &request {
                ToolRequest::Agents => json!([]),
                ToolRequest::Send { .. } => json!({ "id": Uuid::from_u128(90) }),
                ToolRequest::Spawn { name, .. } => json!({
                    "name": name.as_deref().unwrap_or("generated"),
                    "id": Uuid::from_u128(91)
                }),
                ToolRequest::Stop { .. } | ToolRequest::Status { .. } => json!({}),
            };
            self.calls.lock().unwrap().push(request);
            Ok(output)
        }
    }

    fn response_lines(output: Vec<u8>) -> Vec<Value> {
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a2a_mcp_tools_list_exposes_the_five_agreed_schemas() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n"
        );
        let backend = Arc::new(RecordingBackend::default());
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, backend).await.unwrap();

        let responses = response_lines(output);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-03-26");
        let tools = responses[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["agents", "send", "spawn", "stop", "status"]
        );
        assert!(tools.iter().all(|tool| {
            tool["description"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
                && tool["inputSchema"]["type"] == "object"
                && tool["inputSchema"]["additionalProperties"] == false
        }));
        assert_eq!(tools[0]["inputSchema"]["properties"], json!({}));
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["to", "text"]));
        assert_eq!(
            tools[2]["inputSchema"]["properties"]["kind"]["enum"],
            json!(["claude", "codex"])
        );
        assert_eq!(
            tools[4]["inputSchema"]["properties"]["working_on"]["type"],
            json!(["string", "null"])
        );
    }

    #[test]
    fn a2a_tool_descriptions_match_golden_fixture() {
        let rendered = tool_definitions()
            .into_iter()
            .map(|tool| format!("{}\n{}\n", tool.name, tool.description))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            rendered,
            include_str!("../tests/fixtures/tool_descriptions.txt")
        );
    }

    #[tokio::test]
    async fn a2a_mcp_tool_calls_map_every_request_and_return_json_text() {
        let context = Uuid::from_u128(44);
        let input = format!(
            concat!(
                "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"agents\",\"arguments\":{{}}}}}}\n",
                "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{{\"name\":\"send\",\"arguments\":{{\"to\":\"worker\",\"text\":\"inspect this\",\"context\":\"{}\"}}}}}}\n",
                "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{{\"name\":\"spawn\",\"arguments\":{{\"kind\":\"codex\",\"prompt\":\"find the fault\",\"name\":\"probe\",\"cwd\":\"/tmp/work\"}}}}}}\n",
                "{{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{{\"name\":\"stop\",\"arguments\":{{\"name\":\"probe\"}}}}}}\n",
                "{{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{{\"name\":\"status\",\"arguments\":{{\"working_on\":null}}}}}}\n"
            ),
            context
        );
        let backend = Arc::new(RecordingBackend::default());
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, backend.clone())
            .await
            .unwrap();

        assert_eq!(
            *backend.calls.lock().unwrap(),
            vec![
                ToolRequest::Agents,
                ToolRequest::Send {
                    to: "worker".to_string(),
                    text: "inspect this".to_string(),
                    context: Some(context),
                },
                ToolRequest::Spawn {
                    kind: SpawnKind::Codex,
                    prompt: "find the fault".to_string(),
                    name: Some("probe".to_string()),
                    cwd: Some(PathBuf::from("/tmp/work")),
                },
                ToolRequest::Stop {
                    name: "probe".to_string(),
                },
                ToolRequest::Status { working_on: None },
            ]
        );
        let responses = response_lines(output);
        assert_eq!(responses.len(), 5);
        assert!(responses.iter().all(|response| {
            response["result"]["content"][0]["type"] == "text"
                && response["result"]["content"][0]["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .is_some()
                && response["result"]["isError"] == false
        }));
    }

    #[test]
    fn a2a_mcp_identity_authenticates_send_spawn_and_status() {
        let caller = Uuid::from_u128(101);
        let host = Uuid::from_u128(102);
        let context = Uuid::from_u128(103);

        assert_eq!(caller_from_env(None).unwrap(), None);
        assert_eq!(
            caller_from_env(Some(OsString::from(caller.to_string()))).unwrap(),
            Some(caller)
        );
        assert!(caller_from_env(Some(OsString::from("not-a-uuid"))).is_err());

        let send = send_request(
            Some(caller),
            "reviewer".to_string(),
            "please inspect".to_string(),
            Some(context),
        );
        assert_eq!(send.from_agent_id, Some(caller));
        assert_eq!(send.context, Some(context));

        assert_eq!(
            caller_parent(Some(caller), |agent_id| (agent_id == caller)
                .then_some(host))
            .unwrap(),
            Some(AgentParent {
                agent_id: caller,
                host_id: host,
            })
        );
        assert!(caller_parent(Some(caller), |_| None).is_err());

        let status = status_request(Some(caller), Some("reviewing".to_string())).unwrap();
        assert_eq!(status.agent, AgentIdentifier::Id(caller));
        assert_eq!(status.working_on.as_deref(), Some("reviewing"));
        assert!(status_request(None, None).is_err());
    }

    #[test]
    fn child_working_directory_defaults_to_the_calling_agent() {
        let caller = Agent {
            id: Uuid::from_u128(201),
            host_id: Uuid::from_u128(202),
            name: Some("parent".to_string()),
            command: "claude".to_string(),
            working_dir: PathBuf::from("/parent/work"),
            agent_type: "claude".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: chrono::Utc::now(),
            parent: None,
            working_on: None,
        };

        assert_eq!(
            spawn_working_dir(None, Some(&caller)).unwrap(),
            PathBuf::from("/parent/work")
        );
        assert_eq!(
            spawn_working_dir(Some(PathBuf::from("/override")), Some(&caller)).unwrap(),
            PathBuf::from("/override")
        );
    }
}

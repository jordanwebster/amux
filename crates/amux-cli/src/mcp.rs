use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use amux::agent_tools::{
    AgentSpawnKind as SpawnKind, AgentToolRequest as ToolRequest, claude_permission_args,
    definitions as tool_definitions, parse_call as parse_tool_call,
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
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

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

#[async_trait]
trait DaemonApi: Send + Sync {
    async fn list_agents(&self) -> Result<Vec<Agent>>;
    async fn list_hosts(&self) -> Result<Vec<amux::HostEntry>>;
    async fn send_message(&self, request: SendMessageRequest) -> Result<Uuid>;
    async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent>;
    async fn delete_child_agent(&self, target: AgentIdentifier, caller: Uuid) -> Result<()>;
    async fn set_agent_status(&self, request: SetAgentStatusRequest) -> Result<()>;
}

#[async_trait]
impl DaemonApi for Client {
    async fn list_agents(&self) -> Result<Vec<Agent>> {
        Ok(Client::list_agents(self).await?)
    }

    async fn list_hosts(&self) -> Result<Vec<amux::HostEntry>> {
        Ok(Client::list_hosts(self).await?)
    }

    async fn send_message(&self, request: SendMessageRequest) -> Result<Uuid> {
        Ok(Client::send_message(self, request).await?)
    }

    async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent> {
        Ok(Client::create_agent(self, request).await?)
    }

    async fn delete_child_agent(&self, target: AgentIdentifier, caller: Uuid) -> Result<()> {
        Ok(Client::delete_child_agent(self, target, caller).await?)
    }

    async fn set_agent_status(&self, request: SetAgentStatusRequest) -> Result<()> {
        Ok(Client::set_agent_status(self, request).await?)
    }
}

#[async_trait]
trait ClientConnector: Send + Sync {
    async fn connect(&self) -> Result<Arc<dyn DaemonApi>>;
}

struct ConfigConnector {
    config: Config,
}

#[async_trait]
impl ClientConnector for ConfigConnector {
    async fn connect(&self) -> Result<Arc<dyn DaemonApi>> {
        Ok(Arc::new(require_running_client(&self.config, None).await?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagedIdentity {
    agent_id: Uuid,
    host_id: Uuid,
}

struct ClientBackend {
    connector: Arc<dyn ClientConnector>,
    identity: Option<ManagedIdentity>,
}

pub(super) async fn serve_agent(config: &Config, socket_path: Option<&Path>) -> Result<()> {
    validate_route(config, socket_path)?;
    let identity = identity_from_env(
        std::env::var_os("AMUX_AGENT_ID"),
        std::env::var_os("AMUX_HOST_ID"),
    )?;
    let backend = Arc::new(ClientBackend {
        connector: Arc::new(ConfigConnector {
            config: config.clone(),
        }),
        identity,
    });
    backend.preflight().await?;
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

fn initialize_response(id: Value, _params: &Value) -> Value {
    rpc_result(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
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

fn validate_route(config: &Config, socket_path: Option<&Path>) -> Result<()> {
    if let Some(config_path) = config.path.as_deref() {
        if !config_path.is_absolute() {
            return Err(anyhow!("AMUX_CONFIG must resolve to an absolute path"));
        }
        config_path
            .to_str()
            .ok_or_else(|| anyhow!("AMUX_CONFIG is not valid UTF-8"))?;
        if !config_path.is_file() {
            return Err(anyhow!(
                "AMUX_CONFIG no longer exists: {}",
                config_path.display()
            ));
        }
    }

    let Some(socket_path) = socket_path else {
        return Ok(());
    };
    if !socket_path.is_absolute() {
        return Err(anyhow!("--socket-path must be absolute"));
    }
    socket_path
        .to_str()
        .ok_or_else(|| anyhow!("--socket-path is not valid UTF-8"))?;
    if config.socket_path != socket_path {
        return Err(anyhow!(
            "configured socket {} does not match --socket-path {}",
            config.socket_path.display(),
            socket_path.display()
        ));
    }
    Ok(())
}

fn uuid_from_env(name: &str, value: OsString) -> Result<Uuid> {
    let value = value
        .into_string()
        .map_err(|_| anyhow!("{name} is not valid UTF-8"))?;
    Uuid::parse_str(&value).with_context(|| format!("{name} is not a valid UUID"))
}

fn identity_from_env(
    agent_id: Option<OsString>,
    host_id: Option<OsString>,
) -> Result<Option<ManagedIdentity>> {
    match (agent_id, host_id) {
        (None, None) => Ok(None),
        (Some(agent_id), Some(host_id)) => Ok(Some(ManagedIdentity {
            agent_id: uuid_from_env("AMUX_AGENT_ID", agent_id)?,
            host_id: uuid_from_env("AMUX_HOST_ID", host_id)?,
        })),
        _ => Err(anyhow!(
            "AMUX_AGENT_ID and AMUX_HOST_ID must either both be set or both be absent"
        )),
    }
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

fn stop_request(caller: Option<Uuid>, name: String) -> Result<(AgentIdentifier, Uuid)> {
    let caller = caller.ok_or_else(|| anyhow!("amux agent identity is unavailable"))?;
    Ok((AgentIdentifier::from(name), caller))
}

async fn validate_identity(client: &dyn DaemonApi, identity: ManagedIdentity) -> Result<()> {
    let agents = client.list_agents().await?;
    let agent = agents
        .iter()
        .find(|agent| agent.id == identity.agent_id)
        .ok_or_else(|| anyhow!("amux agent identity is not present in the fleet"))?;
    if agent.host_id != identity.host_id {
        return Err(anyhow!(
            "amux agent identity belongs to host {}, not injected host {}",
            agent.host_id,
            identity.host_id
        ));
    }
    Ok(())
}

#[async_trait]
impl ToolBackend for ClientBackend {
    async fn call(&self, request: ToolRequest) -> Result<Value> {
        let client = self.connector.connect().await?;
        if let Some(identity) = self.identity {
            validate_identity(client.as_ref(), identity).await?;
        }
        let caller = self.identity.map(|identity| identity.agent_id);
        match request {
            ToolRequest::Agents => self.list_agents(client.as_ref()).await,
            ToolRequest::Send { to, text, context } => {
                let id = client
                    .send_message(send_request(caller, to, text, context))
                    .await?;
                Ok(json!({ "id": id }))
            }
            ToolRequest::Spawn {
                kind,
                prompt,
                name,
                cwd,
            } => {
                let fleet = if caller.is_some() {
                    client.list_agents().await?
                } else {
                    Vec::new()
                };
                let caller_agent = self
                    .identity
                    .map(|identity| identity.agent_id)
                    .and_then(|caller| fleet.iter().find(|agent| agent.id == caller));
                let working_dir = spawn_working_dir(cwd, caller_agent)?;
                let parent = caller_parent(caller, |caller| {
                    fleet
                        .iter()
                        .find(|agent| agent.id == caller)
                        .map(|agent| agent.host_id)
                })?;
                let caller_args = caller_agent
                    .map(|agent| agent.args.as_slice())
                    .unwrap_or(&[]);
                let (agent_type, args) = match kind {
                    SpawnKind::Claude => (AgentType::Claude, claude_permission_args(caller_args)),
                    SpawnKind::Codex => (
                        AgentType::Codex {
                            model: None,
                            approval_policy: None,
                            sandbox_policy: None,
                            resume_thread_id: None,
                        },
                        Vec::new(),
                    ),
                };
                let managed_prompt = parent.is_some().then_some(prompt.clone());
                let agent = client
                    .create_agent(CreateAgentRequest {
                        agent_id: Uuid::new_v4(),
                        host_id: None,
                        name,
                        agent_type,
                        working_dir,
                        terminal_size: None,
                        args,
                        parent,
                        initial_prompt: managed_prompt,
                    })
                    .await?;
                if caller.is_none() {
                    client
                        .send_message(send_request(None, agent.id.to_string(), prompt, None))
                        .await?;
                }
                Ok(json!({
                    "name": display_agent_name(&agent),
                    "id": agent.id
                }))
            }
            ToolRequest::Stop { name } => {
                let (target, caller) = stop_request(caller, name)?;
                client.delete_child_agent(target, caller).await?;
                Ok(json!({}))
            }
            ToolRequest::Status { working_on } => {
                client
                    .set_agent_status(status_request(caller, working_on)?)
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
    async fn preflight(&self) -> Result<()> {
        let client = self.connector.connect().await?;
        if let Some(identity) = self.identity {
            validate_identity(client.as_ref(), identity).await?;
        }
        Ok(())
    }

    async fn list_agents(&self, client: &dyn DaemonApi) -> Result<Value> {
        let agents = client.list_agents().await?;
        let hosts = client.list_hosts().await?;
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
                    "you": self.identity.is_some_and(|identity| identity.agent_id == agent.id)
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct RecordingBackend {
        calls: Mutex<Vec<ToolRequest>>,
    }

    struct FakeDaemon {
        agents: Vec<Agent>,
        hosts: Vec<amux::HostEntry>,
        fail_send: AtomicBool,
        send_calls: AtomicUsize,
        create_calls: AtomicUsize,
        standalone_spawn_shape: Mutex<Option<(Option<AgentParent>, Option<String>)>>,
    }

    impl FakeDaemon {
        fn new(agents: Vec<Agent>) -> Self {
            let hosts = agents
                .iter()
                .map(|agent| amux::HostEntry {
                    id: agent.host_id,
                    name: format!("host-{}", agent.host_id),
                    online: true,
                    version: Some("test".to_string()),
                    capabilities: Some(amux::Capabilities::default()),
                    trust_status: amux::HostTrustStatus::Trusted,
                    last_dial_error: None,
                })
                .collect();
            Self {
                agents,
                hosts,
                fail_send: AtomicBool::new(false),
                send_calls: AtomicUsize::new(0),
                create_calls: AtomicUsize::new(0),
                standalone_spawn_shape: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl DaemonApi for FakeDaemon {
        async fn list_agents(&self) -> Result<Vec<Agent>> {
            Ok(self.agents.clone())
        }

        async fn list_hosts(&self) -> Result<Vec<amux::HostEntry>> {
            Ok(self.hosts.clone())
        }

        async fn send_message(&self, _request: SendMessageRequest) -> Result<Uuid> {
            self.send_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_send.load(Ordering::SeqCst) {
                Err(anyhow!("response lost after mutation"))
            } else {
                Ok(Uuid::from_u128(301))
            }
        }

        async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent> {
            self.create_calls.fetch_add(1, Ordering::SeqCst);
            *self.standalone_spawn_shape.lock().unwrap() =
                Some((request.parent, request.initial_prompt));
            Ok(test_agent(
                request.agent_id,
                Uuid::from_u128(302),
                "spawned",
            ))
        }

        async fn delete_child_agent(&self, _target: AgentIdentifier, _caller: Uuid) -> Result<()> {
            Ok(())
        }

        async fn set_agent_status(&self, _request: SetAgentStatusRequest) -> Result<()> {
            Ok(())
        }
    }

    struct FakeConnector {
        daemon: Arc<FakeDaemon>,
        fail_first: usize,
        connects: AtomicUsize,
    }

    impl FakeConnector {
        fn new(daemon: Arc<FakeDaemon>, fail_first: usize) -> Self {
            Self {
                daemon,
                fail_first,
                connects: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ClientConnector for FakeConnector {
        async fn connect(&self) -> Result<Arc<dyn DaemonApi>> {
            let attempt = self.connects.fetch_add(1, Ordering::SeqCst);
            if attempt < self.fail_first {
                Err(anyhow!("daemon unavailable"))
            } else {
                Ok(self.daemon.clone())
            }
        }
    }

    fn test_agent(id: Uuid, host_id: Uuid, name: &str) -> Agent {
        Agent {
            id,
            host_id,
            name: Some(name.to_string()),
            command: "test".to_string(),
            working_dir: PathBuf::from("/work"),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: chrono::Utc::now(),
            parent: None,
            working_on: None,
        }
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
        assert_eq!(
            responses[0]["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
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
    fn a2a_mcp_identity_authenticates_send_spawn_stop_and_status() {
        let caller = Uuid::from_u128(101);
        let host = Uuid::from_u128(102);
        let context = Uuid::from_u128(103);

        assert_eq!(identity_from_env(None, None).unwrap(), None);
        assert_eq!(
            identity_from_env(
                Some(OsString::from(caller.to_string())),
                Some(OsString::from(host.to_string()))
            )
            .unwrap(),
            Some(ManagedIdentity {
                agent_id: caller,
                host_id: host,
            })
        );
        assert!(identity_from_env(Some(OsString::from(caller.to_string())), None).is_err());
        assert!(
            identity_from_env(
                Some(OsString::from("not-a-uuid")),
                Some(OsString::from(host.to_string()))
            )
            .is_err()
        );

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

        assert_eq!(
            stop_request(Some(caller), "child".to_string()).unwrap(),
            (AgentIdentifier::Name("child".to_string()), caller)
        );
        assert!(stop_request(None, "child".to_string()).is_err());

        let status = status_request(Some(caller), Some("reviewing".to_string())).unwrap();
        assert_eq!(status.agent, AgentIdentifier::Id(caller));
        assert_eq!(status.working_on.as_deref(), Some("reviewing"));
        assert!(status_request(None, None).is_err());
    }

    #[tokio::test]
    async fn a2a_mcp_managed_identity_preflight_rejects_stale_and_cross_host_pairs() {
        let agent_id = Uuid::from_u128(401);
        let host_id = Uuid::from_u128(402);
        let daemon = Arc::new(FakeDaemon::new(vec![test_agent(
            agent_id, host_id, "caller",
        )]));

        let matching = ClientBackend {
            connector: Arc::new(FakeConnector::new(daemon.clone(), 0)),
            identity: Some(ManagedIdentity { agent_id, host_id }),
        };
        matching.preflight().await.unwrap();

        let stale = ClientBackend {
            connector: Arc::new(FakeConnector::new(daemon.clone(), 0)),
            identity: Some(ManagedIdentity {
                agent_id: Uuid::from_u128(499),
                host_id,
            }),
        };
        assert!(
            stale
                .preflight()
                .await
                .unwrap_err()
                .to_string()
                .contains("not present")
        );

        let crossed = ClientBackend {
            connector: Arc::new(FakeConnector::new(daemon, 0)),
            identity: Some(ManagedIdentity {
                agent_id,
                host_id: Uuid::from_u128(498),
            }),
        };
        assert!(
            crossed
                .preflight()
                .await
                .unwrap_err()
                .to_string()
                .contains("not injected host")
        );
    }

    #[tokio::test]
    async fn a2a_mcp_each_call_reconnects_and_the_next_call_recovers() {
        let daemon = Arc::new(FakeDaemon::new(Vec::new()));
        let connector = Arc::new(FakeConnector::new(daemon, 1));
        let backend = ClientBackend {
            connector: connector.clone(),
            identity: None,
        };

        assert!(backend.call(ToolRequest::Agents).await.is_err());
        assert_eq!(
            backend.call(ToolRequest::Agents).await.unwrap(),
            Value::Array(Vec::new())
        );
        assert_eq!(connector.connects.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a2a_mcp_startup_preflight_fails_closed_when_the_daemon_is_unavailable() {
        let daemon = Arc::new(FakeDaemon::new(Vec::new()));
        let connector = Arc::new(FakeConnector::new(daemon, 1));
        let backend = ClientBackend {
            connector: connector.clone(),
            identity: None,
        };

        assert!(backend.preflight().await.is_err());
        assert_eq!(connector.connects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a2a_mcp_ambiguous_mutation_is_not_retried_in_the_same_call() {
        let daemon = Arc::new(FakeDaemon::new(Vec::new()));
        daemon.fail_send.store(true, Ordering::SeqCst);
        let connector = Arc::new(FakeConnector::new(daemon.clone(), 0));
        let backend = ClientBackend {
            connector: connector.clone(),
            identity: None,
        };

        let error = backend
            .call(ToolRequest::Send {
                to: "worker".to_string(),
                text: "do it once".to_string(),
                context: None,
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("response lost after mutation"));
        assert_eq!(connector.connects.load(Ordering::SeqCst), 1);
        assert_eq!(daemon.send_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a2a_mcp_standalone_spawn_creates_an_orphan_then_delivers_the_prompt() {
        let daemon = Arc::new(FakeDaemon::new(Vec::new()));
        let backend = ClientBackend {
            connector: Arc::new(FakeConnector::new(daemon.clone(), 0)),
            identity: None,
        };

        backend
            .call(ToolRequest::Spawn {
                kind: SpawnKind::Codex,
                prompt: "inspect this".to_string(),
                name: Some("probe".to_string()),
                cwd: Some(PathBuf::from("/work")),
            })
            .await
            .unwrap();

        assert_eq!(daemon.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(daemon.send_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *daemon.standalone_spawn_shape.lock().unwrap(),
            Some((None, None))
        );
    }

    #[test]
    fn a2a_mcp_explicit_socket_must_be_absolute_and_match_loaded_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("amux.yaml");
        std::fs::write(&config_path, "host_name: test\n").unwrap();
        let socket = dir.path().join("amux.sock");
        let config = Config {
            socket_path: socket.clone(),
            path: Some(config_path),
            ..Config::default()
        };

        validate_route(&config, Some(&socket)).unwrap();
        assert!(validate_route(&config, Some(Path::new("relative.sock"))).is_err());
        assert!(
            validate_route(&config, Some(&dir.path().join("other.sock")))
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
        std::fs::remove_file(config.path.as_ref().unwrap()).unwrap();
        assert!(
            validate_route(&config, Some(&socket))
                .unwrap_err()
                .to_string()
                .contains("no longer exists")
        );
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

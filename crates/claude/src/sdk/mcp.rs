use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::sdk::error::Error;
use crate::sdk::types::Extensions;

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

type ToolFuture =
    Pin<Box<dyn Future<Output = Result<SdkMcpToolResult, SdkMcpToolError>> + Send + 'static>>;
type ToolHandler = dyn Fn(serde_json::Value, SdkMcpToolCall) -> ToolFuture + Send + Sync;

/// Metadata supplied to an in-process MCP tool invocation.
///
/// The raw arguments and unmodeled call fields are retained even when the
/// handler chooses a typed Rust input.
#[derive(Debug, Clone)]
pub struct SdkMcpToolCall {
    pub request_id: serde_json::Value,
    pub raw_arguments: serde_json::Value,
    pub meta: Option<serde_json::Value>,
    pub extensions: Extensions,
}

/// Exact MCP `tools/call` result returned by an in-process tool.
///
/// MCP content is intentionally represented as JSON values: the protocol's
/// content union can grow, and this boundary must not discard a valid content
/// kind merely because this crate does not yet have a typed projection for it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SdkMcpToolResult {
    pub content: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl SdkMcpToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![serde_json::json!({ "type": "text", "text": text.into() })],
            structured_content: None,
            is_error: None,
            extensions: Extensions::new(),
        }
    }
}

/// Handler failure returned as a JSON-RPC error without fabricating a tool
/// result.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct SdkMcpToolError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

impl SdkMcpToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }
}

/// Optional metadata attached by [`tool`].
#[derive(Debug, Clone, Default)]
pub struct SdkMcpToolOptions {
    pub annotations: Option<serde_json::Value>,
    pub search_hint: Option<String>,
    pub always_load: bool,
}

/// A typed, in-process MCP tool definition.
#[derive(Clone)]
pub struct SdkMcpTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    annotations: Option<serde_json::Value>,
    meta: Extensions,
    handler: Arc<ToolHandler>,
}

impl fmt::Debug for SdkMcpTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SdkMcpTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("annotations", &self.annotations)
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

/// Define an in-process SDK MCP tool with a deserialized Rust input.
///
/// The JSON schema is sent unchanged in `tools/list`. Incoming arguments are
/// deserialized into `Input`; a mismatch becomes an MCP invalid-params error.
pub fn tool<Input, Handler, HandlerFuture>(
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: serde_json::Value,
    handler: Handler,
    options: SdkMcpToolOptions,
) -> Result<SdkMcpTool, Error>
where
    Input: DeserializeOwned + Send + 'static,
    Handler: Fn(Input, SdkMcpToolCall) -> HandlerFuture + Send + Sync + 'static,
    HandlerFuture: Future<Output = Result<SdkMcpToolResult, SdkMcpToolError>> + Send + 'static,
{
    let name = name.into();
    let description = description.into();
    if name.trim().is_empty() {
        return Err(Error::InvalidOptions(
            "SDK MCP tool name must be non-empty".into(),
        ));
    }
    if !input_schema.is_object() {
        return Err(Error::InvalidOptions(
            "SDK MCP tool input_schema must be a JSON object".into(),
        ));
    }
    if options
        .annotations
        .as_ref()
        .is_some_and(|annotations| !annotations.is_object())
    {
        return Err(Error::InvalidOptions(
            "SDK MCP tool annotations must be a JSON object".into(),
        ));
    }

    let mut meta = Extensions::new();
    if let Some(search_hint) = options.search_hint {
        meta.insert(
            "anthropic/searchHint".into(),
            serde_json::Value::String(search_hint),
        );
    }
    if options.always_load {
        meta.insert("anthropic/alwaysLoad".into(), serde_json::Value::Bool(true));
    }
    let handler = Arc::new(handler);
    let erased = Arc::new(move |raw: serde_json::Value, call: SdkMcpToolCall| {
        let handler = handler.clone();
        let parsed = serde_json::from_value::<Input>(raw);
        Box::pin(async move {
            let input = parsed.map_err(|error| SdkMcpToolError {
                code: -32602,
                message: format!("invalid tool arguments: {error}"),
                data: None,
            })?;
            handler(input, call).await
        }) as ToolFuture
    });

    Ok(SdkMcpTool {
        name,
        description,
        input_schema,
        annotations: options.annotations,
        meta,
        handler: erased,
    })
}

/// Options for [`create_sdk_mcp_server`].
#[derive(Debug, Clone)]
pub struct CreateSdkMcpServerOptions {
    pub name: String,
    pub version: Option<String>,
    pub instructions: Option<String>,
    pub tools: Vec<SdkMcpTool>,
    pub always_load: bool,
}

/// Live in-process MCP server configured through [`QueryOptions`](crate::sdk::QueryOptions).
#[derive(Debug, Clone)]
pub struct SdkMcpServer {
    name: String,
    version: String,
    instructions: Option<String>,
    tools: Arc<Vec<SdkMcpTool>>,
    always_load: bool,
}

/// Create a live SDK MCP server config for `QueryOptions::mcp_servers`.
pub fn create_sdk_mcp_server(
    options: CreateSdkMcpServerOptions,
) -> Result<crate::sdk::options::McpServerConfig, Error> {
    if options.name.trim().is_empty() {
        return Err(Error::InvalidOptions(
            "SDK MCP server name must be non-empty".into(),
        ));
    }
    let version = options.version.unwrap_or_else(|| "1.0.0".into());
    if version.trim().is_empty() {
        return Err(Error::InvalidOptions(
            "SDK MCP server version must be non-empty".into(),
        ));
    }
    let mut names = std::collections::HashSet::new();
    for tool in &options.tools {
        if !names.insert(tool.name.as_str()) {
            return Err(Error::InvalidOptions(format!(
                "duplicate SDK MCP tool name `{}`",
                tool.name
            )));
        }
    }
    Ok(crate::sdk::options::McpServerConfig::Sdk(SdkMcpServer {
        name: options.name,
        version,
        instructions: options.instructions,
        tools: Arc::new(options.tools),
        always_load: options.always_load,
    }))
}

impl SdkMcpServer {
    pub(crate) fn configured_name(&self) -> &str {
        &self.name
    }

    pub(crate) async fn handle_message(
        &self,
        message: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        let id = message.get("id")?.clone();
        if id.is_null() {
            return None;
        }
        let Some(method) = message.get("method").and_then(serde_json::Value::as_str) else {
            return Some(jsonrpc_error(
                id,
                -32600,
                "MCP request omitted string method",
                None,
            ));
        };
        let params = message
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        let result = match method {
            "initialize" => self.initialize(&params),
            "ping" => Ok(serde_json::json!({})),
            "tools/list" => self.list_tools(&params),
            "tools/call" => self.call_tool(id.clone(), params).await,
            _ => Err(SdkMcpToolError {
                code: -32601,
                message: format!("unsupported MCP method `{method}`"),
                data: None,
            }),
        };
        Some(match result {
            Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => jsonrpc_error(id, error.code, &error.message, error.data),
        })
    }

    fn initialize(&self, params: &serde_json::Value) -> Result<serde_json::Value, SdkMcpToolError> {
        let version = params
            .get("protocolVersion")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid_params("initialize omitted protocolVersion"))?;
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
            return Err(invalid_params(format!(
                "unsupported MCP protocol version `{version}`"
            )));
        }
        let mut result = serde_json::json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": self.name, "version": self.version }
        });
        if let Some(instructions) = &self.instructions {
            result["instructions"] = serde_json::Value::String(instructions.clone());
        }
        Ok(result)
    }

    fn list_tools(&self, params: &serde_json::Value) -> Result<serde_json::Value, SdkMcpToolError> {
        if params.get("cursor").is_some_and(|cursor| !cursor.is_null()) {
            return Err(invalid_params(
                "SDK MCP tools/list does not expose a pagination cursor",
            ));
        }
        let tools = self
            .tools
            .iter()
            .map(|tool| {
                let mut value = serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                });
                if let Some(annotations) = &tool.annotations {
                    value["annotations"] = annotations.clone();
                }
                let mut meta = tool.meta.clone();
                if self.always_load {
                    meta.insert("anthropic/alwaysLoad".into(), serde_json::Value::Bool(true));
                }
                if !meta.is_empty() {
                    value["_meta"] = serde_json::Value::Object(meta);
                }
                value
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({ "tools": tools }))
    }

    async fn call_tool(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SdkMcpToolError> {
        let object = params
            .as_object()
            .ok_or_else(|| invalid_params("tools/call params must be an object"))?;
        let name = object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid_params("tools/call omitted string name"))?;
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .ok_or_else(|| SdkMcpToolError {
                code: -32601,
                message: format!("SDK MCP tool `{name}` was not found"),
                data: None,
            })?;
        let raw_arguments = object
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let meta = object.get("_meta").cloned();
        let extensions = object
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "name" | "arguments" | "_meta"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let call = SdkMcpToolCall {
            request_id,
            raw_arguments: raw_arguments.clone(),
            meta,
            extensions,
        };
        let result = (tool.handler)(raw_arguments, call).await?;
        serde_json::to_value(result).map_err(|error| SdkMcpToolError::new(error.to_string()))
    }
}

fn invalid_params(message: impl Into<String>) -> SdkMcpToolError {
    SdkMcpToolError {
        code: -32602,
        message: message.into(),
        data: None,
    }
}

fn jsonrpc_error(
    id: serde_json::Value,
    code: i64,
    message: &str,
    data: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut error = serde_json::json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

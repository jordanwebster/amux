// Serde default enum serialization format: {"VariantName": data} or "VariantName" for unit variants
// Exception: StructuredLog uses #[serde(tag = "type")] so it's {type: "VariantName", ...fields}

// Agent info returned in list
export interface AgentInfo {
  agent_id: string
  alias: string | null
  command: string
  working_dir: string
}

// Structured log entries - uses tag-based format: {type: "UserMessage", content: "...", ...}
export interface UserMessage {
  type: "UserMessage"
  content: string
  timestamp: string
  uuid: string
}

export interface AssistantMessage {
  type: "AssistantMessage"
  content: string
  timestamp: string
  uuid: string
}

// Permission tool types - externally tagged (serde default format)
// Format: { "Edit": { file_path, old_string, new_string } }
export interface EditPermissionToolData {
  file_path: string
  old_string: string
  new_string: string
}

export type PermissionTool = { Edit: EditPermissionToolData }

// Permission request - nested inside StructuredLog
export interface PermissionRequest {
  type: "PermissionRequest"
  tool: PermissionTool
}

// Helper to check if tool is Edit type
export function isEditTool(tool: PermissionTool): tool is { Edit: EditPermissionToolData } {
  return "Edit" in tool
}

export type StructuredLog = UserMessage | AssistantMessage | PermissionRequest

// Helper to extract message type
export function isUserMessage(log: StructuredLog): log is UserMessage {
  return log.type === "UserMessage"
}

export function isAssistantMessage(log: StructuredLog): log is AssistantMessage {
  return log.type === "AssistantMessage"
}

export function isPermissionRequest(log: StructuredLog): log is PermissionRequest {
  return log.type === "PermissionRequest"
}

// Permission response values (sent to server to respond to permission request)
export type PermissionResponse = "Yes" | "YesAll" | "No"

// Client -> Server messages
export type ClientMessage =
  | "ListAgents"
  | { Connect: { host_id: string } }
  | { Subscribe: { src_host: string; dst_host: string; agent_id: string; rows: number; cols: number } }
  | { SubmitInput: { src_host: string; dst_host: string; agent_id: string; data: number[] } }
  | { PermissionRequestResponse: { src_host: string; dst_host: string; agent_id: string; response: PermissionResponse } }

// Server -> Client messages
export type ServerMessage =
  | { ConnectResponse: { success: boolean; error: string | null; host_id: string } }
  | { ListAgentsResult: { agents: AgentInfo[] } }
  | { SubscribeResult: { src_host: string; dst_host: string; agent_id: string; success: boolean; error: string | null } }
  | { StructuredOutput: { src_host: string; dst_host: string; agent_id: string; entry: StructuredLog } }
  | "AgentEnded"
  | { Error: { code: number; message: string } }

// Type guards for server messages
export function isConnectResponse(msg: ServerMessage): msg is { ConnectResponse: { success: boolean; error: string | null; host_id: string } } {
  return typeof msg === "object" && "ConnectResponse" in msg
}

export function isListAgentsResult(msg: ServerMessage): msg is { ListAgentsResult: { agents: AgentInfo[] } } {
  return typeof msg === "object" && "ListAgentsResult" in msg
}

export function isSubscribeResult(msg: ServerMessage): msg is { SubscribeResult: { src_host: string; dst_host: string; agent_id: string; success: boolean; error: string | null } } {
  return typeof msg === "object" && "SubscribeResult" in msg
}

export function isStructuredOutput(msg: ServerMessage): msg is { StructuredOutput: { src_host: string; dst_host: string; agent_id: string; entry: StructuredLog } } {
  return typeof msg === "object" && "StructuredOutput" in msg
}

export function isError(msg: ServerMessage): msg is { Error: { code: number; message: string } } {
  return typeof msg === "object" && "Error" in msg
}

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

export type SubscribeMode = "Raw" | "Structured"

// Protocol-level errors (matches Rust ProtocolError serde format)
export type ProtocolError =
  | { ServerError: string }
  | "LinkNameTaken"
  | { NoRouteFound: string }
  | "InvalidCredentials"

// --- Routable message payloads (without src/dst, those are in the wrapper) ---
export type RoutableMessage =
  | { Subscribe: { agent_id: string; rows: number; cols: number; mode?: SubscribeMode } }
  | { SubscribeResult: { agent_id: string; success: boolean; error: ProtocolError | null } }
  | { InputBytes: { agent_id: string; data: number[] } }
  | { SubmitInput: { agent_id: string; data: number[] } }
  | { Output: { agent_id: string; data: number[] } }
  | { StructuredOutput: { agent_id: string; entry: StructuredLog } }
  | { PermissionRequestResponse: { agent_id: string; response: PermissionResponse } }
  | { Error: ProtocolError }

// --- Local message payloads (no routing) ---
export type LocalMessage =
  | "ListAgents"
  | { Connect: { link_name: string; token?: string | null } }
  | { ListAgentsResult: { agents: AgentInfo[] } }
  | { CreateAgentResult: { success: boolean; error: ProtocolError | null } }
  | "AgentEnded"
  | { Error: { message: string } }
  | "Shutdown"
  | "Debug"
  | { ConnectResponse: { success: boolean; error: ProtocolError | null } }
  | { HookEvent: { hook: unknown } }
  | { HookEventResult: { success: boolean; error: ProtocolError | null } }
  | { DebugResult: { info: unknown } }

// --- Top-level Message type ---
// Wire format: { Routable: { src, dst, message: RoutableMessage } } | { Local: LocalMessage }
export type ClientMessage =
  | { Local: LocalMessage }
  | { Routable: { src: string; dst: string; message: RoutableMessage } }

export type ServerMessage =
  | { Local: LocalMessage }
  | { Routable: { src: string; dst: string; message: RoutableMessage } }

// --- Type guards ---

// Top-level: Routable vs Local
export function isRoutable(msg: ServerMessage): msg is { Routable: { src: string; dst: string; message: RoutableMessage } } {
  return typeof msg === "object" && msg !== null && "Routable" in msg
}

export function isLocal(msg: ServerMessage): msg is { Local: LocalMessage } {
  return typeof msg === "object" && msg !== null && "Local" in msg
}

// Local message guards
export function isConnectResponse(local: LocalMessage): local is { ConnectResponse: { success: boolean; error: ProtocolError | null } } {
  return typeof local === "object" && local !== null && "ConnectResponse" in local
}

export function isListAgentsResult(local: LocalMessage): local is { ListAgentsResult: { agents: AgentInfo[] } } {
  return typeof local === "object" && local !== null && "ListAgentsResult" in local
}

export function isLocalError(local: LocalMessage): local is { Error: { message: string } } {
  return typeof local === "object" && local !== null && "Error" in local
}

// Routable message guards
export function isSubscribeResult(msg: RoutableMessage): msg is { SubscribeResult: { agent_id: string; success: boolean; error: ProtocolError | null } } {
  return typeof msg === "object" && msg !== null && "SubscribeResult" in msg
}

export function isStructuredOutput(msg: RoutableMessage): msg is { StructuredOutput: { agent_id: string; entry: StructuredLog } } {
  return typeof msg === "object" && msg !== null && "StructuredOutput" in msg
}

export function isRoutableError(msg: RoutableMessage): msg is { Error: ProtocolError } {
  return typeof msg === "object" && msg !== null && "Error" in msg
}

// Serde default enum serialization format: {"VariantName": data} or "VariantName" for unit variants
// Exception: ClaudeStructuredOutput uses #[serde(tag = "type")] so it's {type: "VariantName", ...fields}
// Wrapper enums use serde's default (externally tagged): {"Claude": {type: "...", ...}}

// Agent returned in list
export interface Agent {
  id: string
  name: string | null
  command: string
  working_dir: string
}

// Claude structured output entries - uses tag-based format: {type: "UserMessage", content: "...", ...}
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

// Permission request - nested inside ClaudeStructuredOutput
export interface PermissionRequest {
  type: "PermissionRequest"
  tool: PermissionTool
}

// Helper to check if tool is Edit type
export function isEditTool(tool: PermissionTool): tool is { Edit: EditPermissionToolData } {
  return "Edit" in tool
}

export type ClaudeStructuredOutput = UserMessage | AssistantMessage | PermissionRequest

// Helper to extract message type
export function isUserMessage(log: ClaudeStructuredOutput): log is UserMessage {
  return log.type === "UserMessage"
}

export function isAssistantMessage(log: ClaudeStructuredOutput): log is AssistantMessage {
  return log.type === "AssistantMessage"
}

export function isPermissionRequest(log: ClaudeStructuredOutput): log is PermissionRequest {
  return log.type === "PermissionRequest"
}

// Permission response values (sent to server to respond to permission request)
export type PermissionResponse = "Yes" | "YesAll" | "No"

// Protocol-level errors (matches Rust ProtocolError serde format)
export type ProtocolError =
  | { ServerError: string }
  | "LinkNameTaken"
  | { NoRouteFound: string }
  | "InvalidCredentials"

// --- Routable message payloads (without src/dst, those are in the wrapper) ---
export type RoutableMessage =
  | { SubscribeRaw: { agent_id: string; terminal_size: { rows: number; cols: number } | null } }
  | { SubscribeStructured: { agent_id: string } }
  | { SubscribeRawResult: { agent_id: string; success: boolean; error: ProtocolError | null } }
  | { SubscribeStructuredResult: { agent_id: string; success: boolean; error: ProtocolError | null } }
  | { RawInput: { agent_id: string; data: number[] } }
  | { StructuredInput: { agent_id: string; data: StructuredInput } }
  | { RawOutput: { agent_id: string; data: number[] } }
  | { StructuredOutput: { agent_id: string; data: StructuredOutputWrapper } }
  | { Error: ProtocolError }

// Wrapper enums matching Rust's externally-tagged serde format
export type StructuredOutputWrapper = { Claude: ClaudeStructuredOutput }
export type StructuredInput = { Claude: ClaudeStructuredInput }

// Claude-specific structured input (matches Rust ClaudeStructuredInput)
export type ClaudeStructuredInput =
  | { PermissionResponse: PermissionResponse }
  | { SubmitMessage: { data: number[] } }

// --- Direct message payloads (no routing) ---
export type DirectMessage =
  | "ListAgents"
  | { Connect: { link_name: string; token?: string | null } }
  | { ListAgentsResult: { agents: Agent[] } }
  | { CreateAgentResult: { success: boolean; error: ProtocolError | null } }
  | "AgentEnded"
  | { Error: { message: string } }
  | "Shutdown"
  | "Debug"
  | { ConnectResult: { success: boolean; error: ProtocolError | null } }
  | { HookEvent: { hook: unknown } }
  | { HookEventResult: { success: boolean; error: ProtocolError | null } }
  | { DebugResult: { info: unknown } }

// --- Top-level Message type ---
// Wire format: { Routable: { src, dst, message: RoutableMessage } } | { Direct: DirectMessage }
export type ClientMessage =
  | { Direct: DirectMessage }
  | { Routable: { src: string; dst: string; message: RoutableMessage } }

export type ServerMessage =
  | { Direct: DirectMessage }
  | { Routable: { src: string; dst: string; message: RoutableMessage } }

// --- Type guards ---

// Top-level: Routable vs Direct
export function isRoutable(msg: ServerMessage): msg is { Routable: { src: string; dst: string; message: RoutableMessage } } {
  return typeof msg === "object" && msg !== null && "Routable" in msg
}

export function isDirect(msg: ServerMessage): msg is { Direct: DirectMessage } {
  return typeof msg === "object" && msg !== null && "Direct" in msg
}

// Direct message guards
export function isConnectResult(direct: DirectMessage): direct is { ConnectResult: { success: boolean; error: ProtocolError | null } } {
  return typeof direct === "object" && direct !== null && "ConnectResult" in direct
}

export function isListAgentsResult(direct: DirectMessage): direct is { ListAgentsResult: { agents: Agent[] } } {
  return typeof direct === "object" && direct !== null && "ListAgentsResult" in direct
}

export function isDirectError(direct: DirectMessage): direct is { Error: { message: string } } {
  return typeof direct === "object" && direct !== null && "Error" in direct
}

// Routable message guards
export function isSubscribeStructuredResult(msg: RoutableMessage): msg is { SubscribeStructuredResult: { agent_id: string; success: boolean; error: ProtocolError | null } } {
  return typeof msg === "object" && msg !== null && "SubscribeStructuredResult" in msg
}

export function isStructuredOutput(msg: RoutableMessage): msg is { StructuredOutput: { agent_id: string; data: StructuredOutputWrapper } } {
  return typeof msg === "object" && msg !== null && "StructuredOutput" in msg
}

export function isRoutableError(msg: RoutableMessage): msg is { Error: ProtocolError } {
  return typeof msg === "object" && msg !== null && "Error" in msg
}

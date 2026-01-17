import { create } from "zustand"
import type { AgentInfo, StructuredLog } from "../types/protocol"

export type ConnectionStatus = "disconnected" | "connecting" | "connected" | "error"

interface AppState {
  // Connection
  connectionStatus: ConnectionStatus
  serverHostId: string | null
  clientHostId: string

  // Agents
  agents: AgentInfo[]
  selectedAgentId: string | null

  // Messages per agent (keyed by agent_id)
  messagesByAgent: Record<string, StructuredLog[]>

  // Actions
  setConnectionStatus: (status: ConnectionStatus) => void
  setServerHostId: (hostId: string) => void
  setAgents: (agents: AgentInfo[]) => void
  selectAgent: (agentId: string | null) => void
  addMessage: (agentId: string, message: StructuredLog) => void
  clearMessages: (agentId: string) => void
}

export const useAppStore = create<AppState>((set) => ({
  connectionStatus: "disconnected",
  serverHostId: null,
  clientHostId: `dashboard-${Date.now()}`,
  agents: [],
  selectedAgentId: null,
  messagesByAgent: {},

  setConnectionStatus: (status) => set({ connectionStatus: status }),
  setServerHostId: (hostId) => set({ serverHostId: hostId }),
  setAgents: (agents) => set({ agents }),
  selectAgent: (agentId) => set({ selectedAgentId: agentId }),
  addMessage: (agentId, message) =>
    set((state) => ({
      messagesByAgent: {
        ...state.messagesByAgent,
        [agentId]: [...(state.messagesByAgent[agentId] || []), message],
      },
    })),
  clearMessages: (agentId) =>
    set((state) => ({
      messagesByAgent: {
        ...state.messagesByAgent,
        [agentId]: [],
      },
    })),
}))

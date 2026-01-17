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

  // Subscriptions - tracks which agents we've already subscribed to
  subscribedAgents: Set<string>

  // Messages per agent (keyed by agent_id)
  messagesByAgent: Record<string, StructuredLog[]>

  // Actions
  setConnectionStatus: (status: ConnectionStatus) => void
  setServerHostId: (hostId: string) => void
  setAgents: (agents: AgentInfo[]) => void
  selectAgent: (agentId: string | null) => void
  addMessage: (agentId: string, message: StructuredLog) => void
  clearMessages: (agentId: string) => void
  markSubscribed: (agentId: string) => void
  isSubscribed: (agentId: string) => boolean
}

export const useAppStore = create<AppState>((set, get) => ({
  connectionStatus: "disconnected",
  serverHostId: null,
  clientHostId: `dashboard-${Date.now()}`,
  agents: [],
  selectedAgentId: null,
  subscribedAgents: new Set(),
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
  markSubscribed: (agentId) =>
    set((state) => ({
      subscribedAgents: new Set(state.subscribedAgents).add(agentId),
    })),
  isSubscribed: (agentId) => get().subscribedAgents.has(agentId),
}))

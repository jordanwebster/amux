import { RefreshCw, Circle } from "lucide-react"
import { useAppStore } from "../store/appStore"
import { useWebSocketContext } from "../contexts/WebSocketContext"
import { cn } from "@/lib/utils"

export function AgentSidebar() {
  const { agents, selectedAgentId, connectionStatus, selectAgent, clearMessages } =
    useAppStore()
  const { subscribeToAgent, refreshAgents } = useWebSocketContext()

  const handleSelectAgent = (agentId: string) => {
    if (agentId !== selectedAgentId) {
      selectAgent(agentId)
      clearMessages(agentId)
      subscribeToAgent(agentId)
    }
  }

  const statusColor = {
    disconnected: "text-zinc-500",
    connecting: "text-yellow-500",
    connected: "text-green-500",
    error: "text-red-500",
  }

  return (
    <div className="w-64 bg-[var(--color-sidebar)] text-[var(--color-sidebar-foreground)] flex flex-col h-full">
      {/* Header */}
      <div className="p-4 border-b border-[var(--color-sidebar-border)] flex items-center justify-between">
        <h1 className="font-semibold">Agents</h1>
        <button
          onClick={refreshAgents}
          className="p-1.5 rounded hover:bg-[var(--color-sidebar-muted)] transition-colors"
          title="Refresh agents"
        >
          <RefreshCw className="w-4 h-4" />
        </button>
      </div>

      {/* Agent list */}
      <div className="flex-1 overflow-y-auto p-2">
        {agents.length === 0 ? (
          <p className="text-sm text-zinc-500 p-2">No agents running</p>
        ) : (
          agents.map((agent) => (
            <button
              key={agent.agent_id}
              onClick={() => handleSelectAgent(agent.agent_id)}
              className={cn(
                "w-full text-left p-3 rounded-lg mb-1 transition-colors",
                selectedAgentId === agent.agent_id
                  ? "bg-[var(--color-sidebar-muted)]"
                  : "hover:bg-[var(--color-sidebar-muted)]/50"
              )}
            >
              <div className="font-medium text-sm truncate">
                {agent.alias || agent.agent_id}
              </div>
              <div className="text-xs text-zinc-500 truncate mt-0.5">
                {agent.working_dir}
              </div>
            </button>
          ))
        )}
      </div>

      {/* Connection status */}
      <div className="p-4 border-t border-[var(--color-sidebar-border)] flex items-center gap-2 text-sm">
        <Circle className={cn("w-2 h-2 fill-current", statusColor[connectionStatus])} />
        <span className="text-zinc-400 capitalize">{connectionStatus}</span>
      </div>
    </div>
  )
}

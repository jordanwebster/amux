import { createContext, useContext, type ReactNode } from "react"
import { useWebSocket } from "../hooks/useWebSocket"

interface WebSocketContextType {
  subscribeToAgent: (agentId: string) => void
  refreshAgents: () => void
  sendInput: (agentId: string, text: string) => void
}

const WebSocketContext = createContext<WebSocketContextType | null>(null)

export function WebSocketProvider({ children }: { children: ReactNode }) {
  const websocket = useWebSocket()

  return (
    <WebSocketContext.Provider value={websocket}>
      {children}
    </WebSocketContext.Provider>
  )
}

export function useWebSocketContext() {
  const context = useContext(WebSocketContext)
  if (!context) {
    throw new Error("useWebSocketContext must be used within a WebSocketProvider")
  }
  return context
}

import { useEffect, useRef, useCallback } from "react"
import { useAppStore } from "../store/appStore"
import type { ClientMessage, ServerMessage, PermissionResponse } from "../types/protocol"
import {
  isConnectResponse,
  isListAgentsResult,
  isSubscribeResult,
  isStructuredOutput,
  isError,
} from "../types/protocol"

const WS_URL = "ws://localhost:9002"

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null)
  const {
    clientHostId,
    serverHostId,
    setConnectionStatus,
    setServerHostId,
    setAgents,
    addMessage,
  } = useAppStore()

  // Send JSON message
  const send = useCallback((message: ClientMessage) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(message))
    }
  }, [])

  // Connect to WebSocket
  useEffect(() => {
    setConnectionStatus("connecting")

    const ws = new WebSocket(WS_URL)
    wsRef.current = ws

    ws.onopen = () => {
      // Send Connect handshake (serde format: {"Connect": {"host_id": "..."}})
      send({ Connect: { host_id: clientHostId } })
    }

    ws.onmessage = (event) => {
      const msg: ServerMessage = JSON.parse(event.data)

      if (isConnectResponse(msg)) {
        if (msg.ConnectResponse.success) {
          setConnectionStatus("connected")
          setServerHostId(msg.ConnectResponse.host_id)
          // Request agent list
          send("ListAgents")
        } else {
          console.error("Connect failed:", msg.ConnectResponse.error)
          setConnectionStatus("error")
        }
      } else if (isListAgentsResult(msg)) {
        setAgents(msg.ListAgentsResult.agents)
      } else if (isSubscribeResult(msg)) {
        if (!msg.SubscribeResult.success) {
          console.error("Subscribe failed:", msg.SubscribeResult.error)
        }
      } else if (isStructuredOutput(msg)) {
        addMessage(msg.StructuredOutput.agent_id, msg.StructuredOutput.entry)
      } else if (msg === "AgentEnded") {
        console.log("Agent session ended")
      } else if (isError(msg)) {
        console.error("Server error:", msg.Error.message)
      }
    }

    ws.onerror = () => setConnectionStatus("error")
    ws.onclose = () => setConnectionStatus("disconnected")

    return () => ws.close()
  }, [clientHostId, send, setConnectionStatus, setServerHostId, setAgents, addMessage])

  // Subscribe to agent
  const subscribeToAgent = useCallback(
    (agentId: string) => {
      if (!serverHostId) return

      send({
        Subscribe: {
          src_host: clientHostId,
          dst_host: serverHostId,
          agent_id: agentId,
          rows: 24,
          cols: 80,
        },
      })
    },
    [send, clientHostId, serverHostId]
  )

  // Refresh agent list
  const refreshAgents = useCallback(() => {
    send("ListAgents")
  }, [send])

  // Send input to agent
  const sendInput = useCallback(
    (agentId: string, text: string) => {
      if (!serverHostId) return

      // Convert text to bytes (UTF-8 encoded as number array)
      // Don't append newline - server will send Enter separately after the input
      const data = Array.from(new TextEncoder().encode(text))

      send({
        SubmitInput: {
          src_host: clientHostId,
          dst_host: serverHostId,
          agent_id: agentId,
          data,
        },
      })
    },
    [send, clientHostId, serverHostId]
  )

  // Send permission response to agent
  const sendPermissionResponse = useCallback(
    (agentId: string, response: PermissionResponse) => {
      if (!serverHostId) return

      send({
        PermissionRequestResponse: {
          src_host: clientHostId,
          dst_host: serverHostId,
          agent_id: agentId,
          response,
        },
      })
    },
    [send, clientHostId, serverHostId]
  )

  return { subscribeToAgent, refreshAgents, sendInput, sendPermissionResponse }
}

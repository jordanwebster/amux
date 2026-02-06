import { useEffect, useRef, useCallback } from "react"
import { useAppStore } from "../store/appStore"
import type { ClientMessage, ServerMessage, PermissionResponse } from "../types/protocol"
import {
  isRoutable,
  isLocal,
  isConnectResponse,
  isListAgentsResult,
  isLocalError,
  isSubscribeResult,
  isStructuredOutput,
  isRoutableError,
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
      // Send Connect handshake: { Local: { Connect: { link_name: "..." } } }
      send({ Local: { Connect: { link_name: clientHostId } } })
    }

    ws.onmessage = (event) => {
      const msg: ServerMessage = JSON.parse(event.data)

      if (isLocal(msg)) {
        const local = msg.Local

        if (isConnectResponse(local)) {
          if (local.ConnectResponse.success) {
            setConnectionStatus("connected")
            setServerHostId(clientHostId)
            // Request agent list
            send({ Local: "ListAgents" })
          } else {
            console.error("Connect failed:", local.ConnectResponse.error)
            setConnectionStatus("error")
          }
        } else if (isListAgentsResult(local)) {
          setAgents(local.ListAgentsResult.agents)
        } else if (local === "AgentEnded") {
          console.log("Agent session ended")
        } else if (isLocalError(local)) {
          console.error("Server error:", local.Error.message)
        }
      } else if (isRoutable(msg)) {
        const routable = msg.Routable.message

        if (isSubscribeResult(routable)) {
          if (!routable.SubscribeResult.success) {
            console.error("Subscribe failed:", routable.SubscribeResult.error)
          }
        } else if (isStructuredOutput(routable)) {
          addMessage(routable.StructuredOutput.agent_id, routable.StructuredOutput.entry)
        } else if (isRoutableError(routable)) {
          console.error("Route error:", routable.Error)
        }
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
        Routable: {
          src: clientHostId,
          dst: "",
          message: {
            Subscribe: {
              agent_id: agentId,
              rows: 24,
              cols: 80,
              mode: "Structured",
            },
          },
        },
      })
    },
    [send, clientHostId, serverHostId]
  )

  // Refresh agent list
  const refreshAgents = useCallback(() => {
    send({ Local: "ListAgents" })
  }, [send])

  // Send input to agent
  const sendInput = useCallback(
    (agentId: string, text: string) => {
      if (!serverHostId) return

      // Convert text to bytes (UTF-8 encoded as number array)
      // Don't append newline - server will send Enter separately after the input
      const data = Array.from(new TextEncoder().encode(text))

      send({
        Routable: {
          src: clientHostId,
          dst: "",
          message: {
            SubmitInput: {
              agent_id: agentId,
              data,
            },
          },
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
        Routable: {
          src: clientHostId,
          dst: "",
          message: {
            PermissionRequestResponse: {
              agent_id: agentId,
              response,
            },
          },
        },
      })
    },
    [send, clientHostId, serverHostId]
  )

  return { subscribeToAgent, refreshAgents, sendInput, sendPermissionResponse }
}

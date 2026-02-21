import { useEffect, useRef, useCallback } from "react"
import { useAppStore } from "../store/appStore"
import type { ClientMessage, ServerMessage, PermissionResponse } from "../types/protocol"
import {
  isRoutable,
  isDirect,
  isConnectResult,
  isListAgentsResult,
  isDirectError,
  isSubscribeStructuredResult,
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
      // Send Connect handshake: { Direct: { Connect: { link_name: "..." } } }
      send({ Direct: { Connect: { link_name: clientHostId } } })
    }

    ws.onmessage = (event) => {
      const msg: ServerMessage = JSON.parse(event.data)

      if (isDirect(msg)) {
        const direct = msg.Direct

        if (isConnectResult(direct)) {
          if (direct.ConnectResult.success) {
            setConnectionStatus("connected")
            setServerHostId(clientHostId)
            // Request agent list
            send({ Direct: "ListAgents" })
          } else {
            console.error("Connect failed:", direct.ConnectResult.error)
            setConnectionStatus("error")
          }
        } else if (isListAgentsResult(direct)) {
          setAgents(direct.ListAgentsResult.agents)
        } else if (direct === "AgentEnded") {
          console.log("Agent session ended")
        } else if (isDirectError(direct)) {
          console.error("Server error:", direct.Error.message)
        }
      } else if (isRoutable(msg)) {
        const routable = msg.Routable.message

        if (isSubscribeStructuredResult(routable)) {
          if (!routable.SubscribeStructuredResult.success) {
            console.error("Subscribe failed:", routable.SubscribeStructuredResult.error)
          }
        } else if (isStructuredOutput(routable)) {
          // Unwrap: StructuredOutput.data.Claude -> ClaudeStructuredOutput
          addMessage(routable.StructuredOutput.agent_id, routable.StructuredOutput.data.Claude)
        } else if (isRoutableError(routable)) {
          console.error("Route error:", routable.Error)
        }
      }
    }

    ws.onerror = () => setConnectionStatus("error")
    ws.onclose = () => setConnectionStatus("disconnected")

    return () => ws.close()
  }, [clientHostId, send, setConnectionStatus, setServerHostId, setAgents, addMessage])

  // Subscribe to agent (structured mode)
  const subscribeToAgent = useCallback(
    (agentId: string) => {
      if (!serverHostId) return

      send({
        Routable: {
          src: clientHostId,
          dst: "",
          message: {
            SubscribeStructured: {
              agent_id: agentId,
            },
          },
        },
      })
    },
    [send, clientHostId, serverHostId]
  )

  // Refresh agent list
  const refreshAgents = useCallback(() => {
    send({ Direct: "ListAgents" })
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
            StructuredInput: {
              agent_id: agentId,
              data: { Claude: { SubmitMessage: { data } } },
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
            StructuredInput: {
              agent_id: agentId,
              data: { Claude: { PermissionResponse: response } },
            },
          },
        },
      })
    },
    [send, clientHostId, serverHostId]
  )

  return { subscribeToAgent, refreshAgents, sendInput, sendPermissionResponse }
}

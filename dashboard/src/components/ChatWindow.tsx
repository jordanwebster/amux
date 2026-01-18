import { useEffect, useRef, useCallback } from "react"
import { useAppStore } from "../store/appStore"
import { useWebSocketContext } from "../contexts/WebSocketContext"
import { ScrollArea } from "./ui/scroll-area"
import { Message } from "./Message"
import { isPermissionRequest } from "../types/protocol"
import type { PermissionResponse } from "../types/protocol"

export function ChatWindow() {
  const { selectedAgentId, messagesByAgent } = useAppStore()
  const { sendPermissionResponse } = useWebSocketContext()
  const bottomRef = useRef<HTMLDivElement>(null)

  const messages = selectedAgentId ? messagesByAgent[selectedAgentId] || [] : []

  // Auto-scroll to bottom on new messages
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" })
  }, [messages])

  // Handler for permission responses
  const handlePermissionResponse = useCallback(
    (response: PermissionResponse) => {
      if (selectedAgentId) {
        sendPermissionResponse(selectedAgentId, response)
      }
    },
    [selectedAgentId, sendPermissionResponse]
  )

  if (!selectedAgentId) {
    return (
      <div className="flex-1 flex items-center justify-center text-[var(--color-muted-foreground)]">
        <p>Select an agent to view the conversation</p>
      </div>
    )
  }

  if (messages.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-[var(--color-muted-foreground)]">
        <p>No messages yet</p>
      </div>
    )
  }

  // Generate stable keys - use uuid for messages, index-based for permission requests
  const getMessageKey = (msg: typeof messages[number], index: number) => {
    if (isPermissionRequest(msg)) {
      return `permission-${index}`
    }
    return msg.uuid
  }

  return (
    <ScrollArea className="flex-1">
      <div className="max-w-3xl mx-auto p-6">
        {messages.map((msg, index) => (
          <Message
            key={getMessageKey(msg, index)}
            message={msg}
            onPermissionResponse={handlePermissionResponse}
          />
        ))}
        <div ref={bottomRef} />
      </div>
    </ScrollArea>
  )
}

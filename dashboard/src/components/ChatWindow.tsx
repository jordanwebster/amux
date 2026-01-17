import { useEffect, useRef } from "react"
import { useAppStore } from "../store/appStore"
import { ScrollArea } from "./ui/scroll-area"
import { Message } from "./Message"

export function ChatWindow() {
  const { selectedAgentId, messagesByAgent } = useAppStore()
  const bottomRef = useRef<HTMLDivElement>(null)

  const messages = selectedAgentId ? messagesByAgent[selectedAgentId] || [] : []

  // Auto-scroll to bottom on new messages
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" })
  }, [messages])

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

  return (
    <ScrollArea className="flex-1">
      <div className="max-w-3xl mx-auto p-6">
        {messages.map((msg) => (
          <Message key={msg.uuid} message={msg} />
        ))}
        <div ref={bottomRef} />
      </div>
    </ScrollArea>
  )
}

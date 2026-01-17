import { useState, type KeyboardEvent } from "react"
import { Send } from "lucide-react"
import { useWebSocketContext } from "../contexts/WebSocketContext"
import { useAppStore } from "../store/appStore"

export function InputArea() {
  const [input, setInput] = useState("")
  const { sendInput } = useWebSocketContext()
  const { selectedAgentId, connectionStatus } = useAppStore()

  const canSend = connectionStatus === "connected" && selectedAgentId && input.trim()

  const handleSend = () => {
    if (!canSend || !selectedAgentId) return

    sendInput(selectedAgentId, input.trim())
    setInput("")
  }

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault()
      handleSend()
    }
  }

  const isDisabled = connectionStatus !== "connected" || !selectedAgentId

  return (
    <div className="border-t border-[var(--color-border)] p-4">
      <div className="max-w-3xl mx-auto">
        <div className="flex items-center gap-3 bg-[var(--color-muted)] rounded-2xl px-4 py-3">
          <input
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={
              isDisabled
                ? "Select an agent to send messages"
                : "Type a message..."
            }
            disabled={isDisabled}
            className="flex-1 bg-transparent outline-none text-[var(--color-foreground)] placeholder:text-[var(--color-muted-foreground)] disabled:cursor-not-allowed"
          />
          <button
            onClick={handleSend}
            disabled={!canSend}
            className="p-2 rounded-full bg-zinc-600 text-white disabled:opacity-50 disabled:cursor-not-allowed hover:bg-zinc-500 transition-colors"
          >
            <Send className="w-4 h-4" />
          </button>
        </div>
      </div>
    </div>
  )
}

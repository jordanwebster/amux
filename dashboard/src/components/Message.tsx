import type { StructuredLog } from "../types/protocol"
import { isUserMessage } from "../types/protocol"
import { MarkdownContent } from "./MarkdownContent"

interface MessageProps {
  message: StructuredLog
}

export function Message({ message }: MessageProps) {
  if (isUserMessage(message)) {
    return (
      <div className="flex justify-end mb-4">
        <div className="bg-[var(--color-user-bubble)] text-white rounded-2xl px-4 py-2 max-w-[80%]">
          <p className="whitespace-pre-wrap">{message.content}</p>
        </div>
      </div>
    )
  }

  // AssistantMessage - no bubble, left-aligned
  return (
    <div className="mb-4 max-w-[85%]">
      <MarkdownContent content={message.content} />
    </div>
  )
}

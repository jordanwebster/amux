import type { ClaudeStructuredOutput, PermissionResponse } from "../types/protocol"
import { isUserMessage, isPermissionRequest } from "../types/protocol"
import { MarkdownContent } from "./MarkdownContent"
import { PermissionRequestCard } from "./PermissionRequestCard"

interface MessageProps {
  message: ClaudeStructuredOutput
  onPermissionResponse?: (response: PermissionResponse) => void
}

export function Message({ message, onPermissionResponse }: MessageProps) {
  if (isUserMessage(message)) {
    return (
      <div className="flex justify-end mb-4">
        <div className="bg-[var(--color-user-bubble)] text-white rounded-2xl px-4 py-2 max-w-[80%]">
          <p className="whitespace-pre-wrap">{message.content}</p>
        </div>
      </div>
    )
  }

  if (isPermissionRequest(message)) {
    return (
      <div className="mb-4">
        <PermissionRequestCard
          request={message}
          onRespond={onPermissionResponse || (() => {})}
        />
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

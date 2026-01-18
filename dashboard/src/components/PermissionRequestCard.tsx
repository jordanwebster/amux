import { useState } from "react"
import type { PermissionRequest, PermissionResponse } from "../types/protocol"
import { isEditTool } from "../types/protocol"
import { DiffView } from "./DiffView"

interface PermissionRequestCardProps {
  request: PermissionRequest
  onRespond: (response: PermissionResponse) => void
}

export function PermissionRequestCard({ request, onRespond }: PermissionRequestCardProps) {
  const [responded, setResponded] = useState(false)
  const [selectedResponse, setSelectedResponse] = useState<PermissionResponse | null>(null)

  const handleRespond = (response: PermissionResponse) => {
    if (responded) return
    setResponded(true)
    setSelectedResponse(response)
    onRespond(response)
  }

  // Currently only Edit tool is supported
  if (!isEditTool(request.tool)) {
    return (
      <div className="bg-yellow-500/10 border border-yellow-500/30 rounded-lg p-4">
        <div className="text-yellow-400">Unknown permission request type</div>
      </div>
    )
  }

  const { file_path, old_string, new_string } = request.tool.Edit

  return (
    <div className="bg-[var(--color-surface)] border border-[var(--color-border)] rounded-lg p-4 max-w-2xl">
      {/* Header */}
      <div className="flex items-center gap-2 mb-3">
        <svg
          className="w-5 h-5 text-yellow-500"
          fill="none"
          stroke="currentColor"
          viewBox="0 0 24 24"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-2.5L13.732 4c-.77-.833-1.964-.833-2.732 0L4.082 16.5c-.77.833.192 2.5 1.732 2.5z"
          />
        </svg>
        <span className="font-medium text-[var(--color-text)]">Edit Permission Request</span>
      </div>

      {/* File path */}
      <div className="mb-3">
        <span className="text-[var(--color-text-muted)] text-sm">File: </span>
        <code className="text-[var(--color-text)] bg-[var(--color-bg)] px-2 py-0.5 rounded text-sm">
          {file_path}
        </code>
      </div>

      {/* Diff view */}
      <div className="mb-4">
        <DiffView oldText={old_string} newText={new_string} />
      </div>

      {/* Action buttons */}
      <div className="flex gap-2">
        <button
          onClick={() => handleRespond("Yes")}
          disabled={responded}
          className={`px-4 py-2 rounded font-medium transition-colors ${
            responded
              ? selectedResponse === "Yes"
                ? "bg-green-600 text-white"
                : "bg-gray-600 text-gray-400 cursor-not-allowed"
              : "bg-green-600 hover:bg-green-700 text-white"
          }`}
        >
          Yes
        </button>
        <button
          onClick={() => handleRespond("YesAll")}
          disabled={responded}
          className={`px-4 py-2 rounded font-medium transition-colors ${
            responded
              ? selectedResponse === "YesAll"
                ? "bg-blue-600 text-white"
                : "bg-gray-600 text-gray-400 cursor-not-allowed"
              : "bg-blue-600 hover:bg-blue-700 text-white"
          }`}
        >
          Yes (all)
        </button>
        <button
          onClick={() => handleRespond("No")}
          disabled={responded}
          className={`px-4 py-2 rounded font-medium transition-colors ${
            responded
              ? selectedResponse === "No"
                ? "bg-red-600 text-white"
                : "bg-gray-600 text-gray-400 cursor-not-allowed"
              : "bg-red-600 hover:bg-red-700 text-white"
          }`}
        >
          No
        </button>
      </div>

      {/* Status indicator */}
      {responded && (
        <div className="mt-3 text-sm text-[var(--color-text-muted)]">
          Response sent: {selectedResponse === "Yes" ? "Accepted" : selectedResponse === "YesAll" ? "Accepted all" : "Denied"}
        </div>
      )}
    </div>
  )
}

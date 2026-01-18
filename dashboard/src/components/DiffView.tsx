interface DiffViewProps {
  oldText: string
  newText: string
}

export function DiffView({ oldText, newText }: DiffViewProps) {
  return (
    <div className="font-mono text-sm space-y-2">
      {/* Old text - red background */}
      <div className="bg-red-500/20 border border-red-500/30 rounded p-3">
        <div className="text-red-400 text-xs mb-1 font-sans">Removed:</div>
        <pre className="whitespace-pre-wrap text-red-300 overflow-x-auto">
          {oldText || "(empty)"}
        </pre>
      </div>

      {/* New text - green background */}
      <div className="bg-green-500/20 border border-green-500/30 rounded p-3">
        <div className="text-green-400 text-xs mb-1 font-sans">Added:</div>
        <pre className="whitespace-pre-wrap text-green-300 overflow-x-auto">
          {newText || "(empty)"}
        </pre>
      </div>
    </div>
  )
}

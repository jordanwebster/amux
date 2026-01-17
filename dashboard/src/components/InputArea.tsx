import { Send } from "lucide-react"

export function InputArea() {
  return (
    <div className="border-t border-[var(--color-border)] p-4">
      <div className="max-w-3xl mx-auto">
        <div className="flex items-center gap-3 bg-[var(--color-muted)] rounded-2xl px-4 py-3">
          <input
            type="text"
            placeholder="Input not supported yet (read-only mode)"
            disabled
            className="flex-1 bg-transparent outline-none text-[var(--color-foreground)] placeholder:text-[var(--color-muted-foreground)] disabled:cursor-not-allowed"
          />
          <button
            disabled
            className="p-2 rounded-full bg-zinc-600 text-white disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Send className="w-4 h-4" />
          </button>
        </div>
      </div>
    </div>
  )
}

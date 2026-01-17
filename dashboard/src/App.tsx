import { AgentSidebar } from "./components/AgentSidebar"
import { ChatWindow } from "./components/ChatWindow"
import { InputArea } from "./components/InputArea"

function App() {
  return (
    <div className="flex h-full">
      <AgentSidebar />
      <main className="flex-1 flex flex-col bg-[var(--color-background)]">
        <ChatWindow />
        <InputArea />
      </main>
    </div>
  )
}

export default App

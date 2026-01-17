import { AgentSidebar } from "./components/AgentSidebar"
import { ChatWindow } from "./components/ChatWindow"
import { InputArea } from "./components/InputArea"
import { WebSocketProvider } from "./contexts/WebSocketContext"

function App() {
  return (
    <WebSocketProvider>
      <div className="flex h-full">
        <AgentSidebar />
        <main className="flex-1 flex flex-col bg-[var(--color-background)]">
          <ChatWindow />
          <InputArea />
        </main>
      </div>
    </WebSocketProvider>
  )
}

export default App

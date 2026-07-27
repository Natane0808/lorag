import { createSignal } from 'solid-js'
import Navbar from './components/Navbar'
import ChatView from './components/ChatView'
import HistorySidebar from './components/HistorySidebar'

export default function App() {
  const [sessionId, setSessionId] = createSignal<string | null>(null)
  const [sidebarOpen, setSidebarOpen] = createSignal(false)
  const [chatKey, setChatKey] = createSignal(0)
  const [sidebarRefresh, setSidebarRefresh] = createSignal(0)

  const handleNewChat = () => {
    setSessionId(null)
    setChatKey((k) => k + 1)
    setSidebarOpen(false)
  }

  const handleSelectSession = (sid: string) => {
    setSessionId(sid)
    setChatKey((k) => k + 1)
    setSidebarOpen(false)
  }

  const handleChatComplete = () => {
    setSidebarRefresh((k) => k + 1)
  }

  return (
    <div class="flex flex-col h-screen">
      <Navbar
        onNewChat={handleNewChat}
        onToggleSidebar={() => setSidebarOpen((v) => !v)}
      />
      <div class="flex flex-1 overflow-hidden">
        <HistorySidebar
          activeSessionId={sessionId()}
          onSelectSession={handleSelectSession}
          onNewChat={handleNewChat}
          open={sidebarOpen()}
          onToggle={() => setSidebarOpen((v) => !v)}
          refreshKey={sidebarRefresh()}
        />
        <main class="flex-1 overflow-hidden flex flex-col">
          <ChatView
            sessionId={sessionId()}
            onSessionId={setSessionId}
            onChatComplete={handleChatComplete}
            resetKey={chatKey()}
          />
        </main>
      </div>
    </div>
  )
}

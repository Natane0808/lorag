import { createSignal, For, createEffect, createMemo, Show, onCleanup } from 'solid-js'
import type { JSX } from 'solid-js'
import MessageBubble from './MessageBubble'
import { streamChat, fetchSessionMessages } from '../api/client'

interface Message {
  role: 'user' | 'assistant'
  content: string
  time: string
}

interface ChatViewProps {
  sessionId: string | null
  onSessionId: (sid: string) => void
  onChatComplete?: () => void
  resetKey: number
}

const SUGGESTED_PROMPTS = [
  { q: '总结我所有文档的核心要点', label: '总结知识库', hint: '先抓大纲，再做摘要' },
  { q: '最近的文档里有哪些关键信息？', label: '查找关键信息', hint: '带原句引用' },
  { q: '对比不同文档之间的差异', label: '对比文档', hint: '逐项对照差异' },
  { q: '根据这些资料生成几道测试题', label: '生成测验', hint: '含答案与解析' },
]

export default function ChatView(props: ChatViewProps) {
  const [messages, setMessages] = createSignal<Message[]>([])
  const [input, setInput] = createSignal('')
  const [loading, setLoading] = createSignal(false)
  let scrollRef!: HTMLDivElement

  const scrollToBottom = () => {
    scrollRef?.scrollTo({ top: scrollRef.scrollHeight, behavior: 'smooth' })
  }

  // Load existing messages when sessionId is provided, or reset on new chat.
  // Uses onCleanup to cancel stale async fetches — if the effect re-runs
  // (e.g. user clicks "new chat" before fetch completes), the old fetch
  // result is discarded so it doesn't overwrite the cleared messages.
  createEffect(() => {
    const sid = props.sessionId
    void props.resetKey // track new-chat events (sessionId stays null but key changes)

    if (sid) {
      let cancelled = false
      onCleanup(() => {
        cancelled = true
      })
      fetchSessionMessages(sid)
        .then((records) => {
          if (cancelled) return
          if (records.length > 0) {
            const msgs: Message[] = records.map((r) => ({
              role: r.role === 'user' ? 'user' : 'assistant',
              content: r.content,
              time: '',
            }))
            setMessages(msgs)
          }
        })
        .catch(() => {})
    } else {
      setMessages([])
      setInput('')
      setLoading(false) // reset stale loading from previous session
    }
  })

  createEffect(() => {
    messages()
    scrollToBottom()
  })

  const sendMessage = async (text: string) => {
    if (!text.trim() || loading()) return

    const now = new Date().toLocaleTimeString('zh-CN', {
      hour: '2-digit',
      minute: '2-digit',
    })

    setInput('')

    setMessages((prev) => [
      ...prev,
      { role: 'user', content: text, time: now },
    ])
    setLoading(true)

    // Compute AI message index inside updater callback to avoid off-by-one
    // caused by Solid batching signal updates in event handlers.
    let aiIdx = 0
    setMessages((prev) => {
      aiIdx = prev.length // AI placeholder will be at index `prev.length`
      return [
        ...prev,
        { role: 'assistant', content: '', time: '' },
      ]
    })

    let content = ''

    await streamChat(
      text,
      props.sessionId,
      (token) => {
        content += token
        setMessages((prev) => {
          const next = [...prev]
          if (next[aiIdx]) {
            next[aiIdx] = { ...next[aiIdx], content }
          }
          return next
        })
      },
      () => {
        const doneTime = new Date().toLocaleTimeString('zh-CN', {
          hour: '2-digit',
          minute: '2-digit',
        })
        setMessages((prev) => {
          const next = [...prev]
          if (next[aiIdx]) {
            next[aiIdx] = { ...next[aiIdx], time: doneTime }
          }
          return next
        })
        setLoading(false)
        props.onChatComplete?.()
      },
      (err) => {
        setMessages((prev) => {
          const next = [...prev]
          if (next[aiIdx]) {
            next[aiIdx] = { ...next[aiIdx], content: `⚠️ ${err}` }
          }
          return next
        })
        setLoading(false)
      },
      (sid) => {
        if (!props.sessionId) {
          props.onSessionId(sid)
        }
      },
    )
  }

  const handleSend = () => {
    sendMessage(input())
  }

  const handlePromptClick = (q: string) => {
    setInput(q)
    sendMessage(q)
  }

  const handleKeyDown: JSX.EventHandler<HTMLTextAreaElement, KeyboardEvent> = (
    e,
  ) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault()
      handleSend()
    }
  }

  const showWelcome = createMemo(() => messages().length === 0 && !loading())

  return (
    <div class="flex flex-col h-full">
      {/* Messages area */}
      <div
        ref={scrollRef!}
        class="flex-1 overflow-y-auto"
      >
        <div class="max-w-[760px] mx-auto px-4">
          {/* Welcome */}
          {showWelcome() && (
            <div class="flex flex-col items-center justify-center min-h-full py-12 px-4">
              <h1 class="text-2xl sm:text-3xl font-semibold tracking-tight mb-2">
                问点什么
              </h1>
              <p class="text-sm text-base-content/50 mb-8">
                lorag 在本地运行 — 你的文件不会离开这台机器
              </p>

              {/* Prompt grid */}
              <div class="grid grid-cols-1 sm:grid-cols-2 gap-3 w-full max-w-lg mb-6">
                <For each={SUGGESTED_PROMPTS}>
                  {(p) => (
                    <button
                      class="text-left p-4 bg-base-200 border border-base-300 rounded-xl
                             hover:border-accent hover:bg-accent/5 transition-colors
                             flex flex-col gap-1"
                      onClick={() => handlePromptClick(p.q)}
                    >
                      <span class="text-sm font-medium">{p.label}</span>
                      <span class="text-xs text-base-content/50">{p.hint}</span>
                    </button>
                  )}
                </For>
              </div>

              {/* Welcome foot chips */}
              <div class="flex flex-wrap gap-2 justify-center">
                <span class="px-2.5 py-1 text-xs border border-base-300 rounded-full text-base-content/50 bg-base-200">
                  仅本地 · 无需联网
                </span>
                <span class="px-2.5 py-1 text-xs border border-base-300 rounded-full text-base-content/50 bg-base-200">
                  支持追问对话
                </span>
                <span class="px-2.5 py-1 text-xs border border-base-300 rounded-full text-base-content/50 bg-base-200">
                  主题可切换
                </span>
              </div>
            </div>
          )}

          {/* Messages list */}
          <Show when={!showWelcome()}>
            <div class="flex flex-col gap-6 py-6">
              <For each={messages()}>
                {(msg) => (
                  <MessageBubble
                    role={msg.role}
                    content={msg.content}
                    time={msg.time}
                  />
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>

      {/* Composer */}
      <div class="border-t border-base-300 bg-base-100 p-3">
        <div class="max-w-[760px] mx-auto">
          <div
            class={`flex items-end gap-2 bg-base-200 border rounded-2xl px-4 py-2.5 transition-colors ${
              loading() ? '' : 'focus-within:border-accent focus-within:shadow-[0_0_0_3px_var(--color-accent)/0.12]'
            }`}
          >
            <textarea
              class="flex-1 bg-transparent border-none outline-none resize-none text-sm leading-relaxed
                     placeholder:text-base-content/35 py-1 max-h-[180px]"
              rows={2}
              placeholder="问任何事…"
              value={input()}
              onInput={(e) => setInput(e.currentTarget.value)}
              onKeyDown={handleKeyDown}
              disabled={loading()}
            />
            <button
              class={`btn btn-sm btn-square rounded-xl flex-shrink-0 ${
                loading()
                  ? 'btn-ghost'
                  : 'btn-primary'
              }`}
              disabled={!input().trim() && !loading()}
              onClick={handleSend}
              aria-label="发送"
            >
              {loading() ? (
                <span class="loading loading-spinner loading-xs" />
              ) : (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                  <line x1="12" y1="19" x2="12" y2="5"/>
                  <polyline points="5 12 12 5 19 12"/>
                </svg>
              )}
            </button>
          </div>
          <div class="text-center text-[11px] text-base-content/30 mt-2 font-mono tracking-wide">
            ⏎ 发送 &nbsp;·&nbsp; ⇧⏎ 换行
          </div>
        </div>
      </div>
    </div>
  )
}

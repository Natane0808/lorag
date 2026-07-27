import { createSignal } from 'solid-js'

interface MessageBubbleProps {
  role: 'user' | 'assistant'
  content: string
  time?: string
}

export default function MessageBubble(props: MessageBubbleProps) {
  const isUser = props.role === 'user'
  const [copied, setCopied] = createSignal(false)

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(props.content)
      setCopied(true)
      setTimeout(() => setCopied(false), 1800)
    } catch {
      // silently ignore
    }
  }

  return (
    <div
      class={`flex gap-3 max-w-[85%] ${isUser ? 'self-end flex-row-reverse' : 'self-start'}`}
    >
      {/* Avatar */}
      <div
        class={`w-7 h-7 rounded-lg flex items-center justify-center flex-shrink-0 text-xs font-semibold ${
          isUser
            ? 'bg-primary/20 text-primary rounded-full'
            : 'bg-accent/15 text-accent'
        }`}
      >
        {isUser ? (
          '你'
        ) : (
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <rect x="3" y="4" width="18" height="16" rx="4"/>
            <path d="M7 10h10M7 13h7M7 16h5" opacity="0.45"/>
            <circle cx="16" cy="16" r="2" fill="currentColor" stroke="none"/>
          </svg>
        )}
      </div>

      {/* Body */}
      <div class="flex-1 min-w-0">
        <div
          class={`text-sm leading-relaxed whitespace-pre-wrap break-words ${
            isUser
              ? 'bg-base-200 px-3.5 py-2.5 rounded-2xl rounded-br-md'
              : ''
          }`}
        >
          {props.content || (
            <span class="flex items-center gap-1.5 py-1">
              <span class="w-1.5 h-1.5 bg-base-content/30 rounded-full animate-bounce" style="animation-delay: 0ms" />
              <span class="w-1.5 h-1.5 bg-base-content/30 rounded-full animate-bounce" style="animation-delay: 120ms" />
              <span class="w-1.5 h-1.5 bg-base-content/30 rounded-full animate-bounce" style="animation-delay: 240ms" />
            </span>
          )}
        </div>

        {/* Meta row (AI only) */}
        {!isUser && props.content && props.time && (
          <div class="flex items-center gap-1 mt-1.5">
            <button
              class="btn btn-ghost btn-xs text-base-content/40 hover:text-base-content/70"
              onClick={handleCopy}
              title="复制"
            >
              {copied() ? '已复制' : '复制'}
            </button>
            <span class="text-xs text-base-content/30">{props.time}</span>
          </div>
        )}

        {/* Meta row (user only) */}
        {isUser && props.time && (
          <div class="flex justify-end mt-0.5">
            <span class="text-xs text-base-content/30">{props.time}</span>
          </div>
        )}
      </div>
    </div>
  )
}

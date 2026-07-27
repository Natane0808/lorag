import { createEffect, createMemo, createSignal, onCleanup } from 'solid-js'
import { marked } from 'marked'
import { extractMermaidBlocks, restoreMermaidBlocks } from '../utils/markdown'
import { applyHtml, renderMermaidBlocks } from '../utils/mermaid'

interface MessageBubbleProps {
  role: 'user' | 'assistant'
  content: string
  time?: string
}

/// Convert markdown text to HTML. Complete ```mermaid … ``` blocks are
/// extracted before parsing so `marked` doesn't tokenize their internals,
/// then reinserted as `<div class="mermaid-pending">` placeholders for the
/// mermaid renderer to pick up later.
function renderMarkdownHtml(text: string): string {
  if (!text) return ''
  const { cleaned, blocks } = extractMermaidBlocks(text)
  const html = marked.parse(cleaned, { breaks: true }) as string
  return restoreMermaidBlocks(html, blocks)
}

export default function MessageBubble(props: MessageBubbleProps) {
  const isUser = props.role === 'user'
  const [copied, setCopied] = createSignal(false)
  let contentRef!: HTMLDivElement

  const html = createMemo(() => renderMarkdownHtml(props.content))

  // Apply HTML imperatively (not via Solid's `innerHTML={}` binding) so we
  // can preserve already-rendered mermaid SVGs across streaming re-renders.
  // The shared `svgCache` in utils/mermaid.ts is keyed by source code, so
  // identical blocks re-mount instantly without forcing a re-render.
  createEffect(() => {
    const h = html()
    if (!contentRef) return
    applyHtml(contentRef, h)
    if (h) renderMermaidBlocks(contentRef)
  })

  onCleanup(() => {
    if (contentRef) contentRef.innerHTML = ''
  })

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
            ? 'bg-primary/25 text-primary rounded-full'
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
        {/* Bubble */}
        <div
          class={`text-sm leading-relaxed break-words px-4 py-3 md-content
            ${
              isUser
                ? 'bg-primary/15 text-base-content rounded-2xl rounded-br-sm'
                : 'bg-base-200 text-base-content rounded-2xl rounded-bl-sm border border-base-300/60'
            }`}
        >
          {props.content ? (
            <div ref={contentRef!} />
          ) : (
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

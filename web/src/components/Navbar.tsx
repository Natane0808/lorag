import { createSignal, onMount } from 'solid-js'
import { fetchStatus } from '../api/client'

interface NavbarProps {
  onNewChat: () => void
  onToggleSidebar: () => void
}

export default function Navbar(props: NavbarProps) {
  const [dark, setDark] = createSignal(
    window.matchMedia('(prefers-color-scheme: dark)').matches,
  )
  const [sourceCount, setSourceCount] = createSignal<number | null>(null)

  onMount(async () => {
    try {
      const s = await fetchStatus()
      setSourceCount(s.sources_count)
    } catch {
      // silently ignore
    }
  })

  const toggleTheme = () => {
    const next = !dark()
    setDark(next)
    document.documentElement.setAttribute(
      'data-theme',
      next ? 'dark' : 'light',
    )
  }

  return (
    <header class="flex items-center justify-between px-6 py-3.5 border-b border-base-300 bg-base-100 flex-shrink-0">
      <div class="flex items-center gap-3 min-w-0">
        {/* Hamburger — mobile only */}
        <button
          class="btn btn-ghost btn-sm btn-square lg:hidden"
          aria-label="对话历史"
          title="对话历史"
          onClick={props.onToggleSidebar}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <line x1="3" y1="6" x2="21" y2="6"/>
            <line x1="3" y1="12" x2="21" y2="12"/>
            <line x1="3" y1="18" x2="21" y2="18"/>
          </svg>
        </button>

        {/* Brand mark */}
        <span
          class="w-7 h-7 flex items-center justify-center bg-accent/15 text-accent rounded-lg flex-shrink-0"
          aria-hidden="true"
        >
          <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <rect x="2.5" y="3" width="19" height="18" rx="5"/>
            <path d="M7 9.5h10M7 13h7M7 16.5h5" opacity="0.45"/>
            <circle cx="17.5" cy="17.5" r="2.6" fill="currentColor" stroke="none"/>
          </svg>
        </span>
        <span class="text-base font-semibold tracking-tight hidden sm:inline">lorag</span>

        {/* KB indicator */}
        {sourceCount() !== null && (
          <span class="hidden sm:inline-flex items-center gap-1.5 ml-2 px-2 py-1 border border-base-300 rounded-full text-xs text-base-content/70 bg-base-200">
            <span class="w-1.5 h-1.5 bg-accent rounded-full flex-shrink-0" />
            <span class="font-medium">{sourceCount()} 文档</span>
          </span>
        )}
      </div>

      <div class="flex items-center gap-1.5">
        {/* New chat */}
        <button
          class="btn btn-ghost btn-sm btn-square"
          aria-label="新建对话"
          title="新建对话"
          onClick={props.onNewChat}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <line x1="12" y1="5" x2="12" y2="19"/>
            <line x1="5" y1="12" x2="19" y2="12"/>
          </svg>
        </button>

        {/* Theme toggle */}
        <label class="swap swap-rotate">
          <input
            type="checkbox"
            class="theme-controller"
            checked={dark()}
            onChange={toggleTheme}
          />
          <span class="swap-on text-lg">🌙</span>
          <span class="swap-off text-lg">☀️</span>
        </label>
      </div>
    </header>
  )
}

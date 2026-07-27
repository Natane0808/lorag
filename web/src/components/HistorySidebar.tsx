import { createSignal, For, onMount, Show, createEffect } from 'solid-js'
import { fetchSessions, deleteSession } from '../api/client'
import type { SessionInfo } from '../api/client'

interface HistorySidebarProps {
  activeSessionId: string | null
  onSelectSession: (sid: string) => void
  onNewChat: () => void
  open: boolean
  onToggle: () => void
  refreshKey?: number
}

function formatTime(iso: string): string {
  try {
    const d = new Date(iso)
    const now = new Date()
    const diffDays = Math.floor((now.getTime() - d.getTime()) / 86400000)

    if (diffDays === 0) {
      return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
    } else if (diffDays === 1) {
      return '昨天'
    } else if (diffDays < 7) {
      return `${diffDays} 天前`
    } else {
      return d.toLocaleDateString('zh-CN', { month: 'short', day: 'numeric' })
    }
  } catch {
    return ''
  }
}

/// Group sessions by date label for sidebar grouping (参考 demo 分组设计)
function groupByDate(sessions: SessionInfo[]): { label: string; items: SessionInfo[] }[] {
  const now = new Date()
  const today: SessionInfo[] = []
  const yesterday: SessionInfo[] = []
  const thisWeek: SessionInfo[] = []
  const older: SessionInfo[] = []

  for (const s of sessions) {
    const d = new Date(s.updated_at)
    const diffDays = Math.floor((now.getTime() - d.getTime()) / 86400000)
    if (diffDays === 0) {
      today.push(s)
    } else if (diffDays === 1) {
      yesterday.push(s)
    } else if (diffDays < 7) {
      thisWeek.push(s)
    } else {
      older.push(s)
    }
  }

  const groups: { label: string; items: SessionInfo[] }[] = []
  if (today.length) groups.push({ label: '今天', items: today })
  if (yesterday.length) groups.push({ label: '昨天', items: yesterday })
  if (thisWeek.length) groups.push({ label: '本周', items: thisWeek })
  if (older.length) groups.push({ label: '更早', items: older })
  return groups
}

export default function HistorySidebar(props: HistorySidebarProps) {
  const [sessions, setSessions] = createSignal<SessionInfo[]>([])
  const [loading, setLoading] = createSignal(false)
  const [deletingId, setDeletingId] = createSignal<string | null>(null)

  const load = async () => {
    setLoading(true)
    try {
      const list = await fetchSessions()
      setSessions(list)
    } catch {
      // silently ignore
    } finally {
      setLoading(false)
    }
  }

  // Load on mount + auto-refresh when active session or chat completion changes
  onMount(load)
  createEffect(() => {
    void props.activeSessionId // track changes (new chat, session switch)
    void props.refreshKey      // track chat completions (messages persisted)
    load()
  })

  const handleSelectSession = async (sid: string) => {
    props.onSelectSession(sid)
    await load()
  }

  const handleDelete = async (e: MouseEvent, sid: string) => {
    e.stopPropagation()
    if (deletingId() === sid) return // already confirming
    if (!confirm('确定要删除这个对话吗？')) return
    setDeletingId(sid)
    try {
      await deleteSession(sid)
      // If we deleted the active session, navigate to new chat
      if (props.activeSessionId === sid) {
        props.onNewChat()
      }
      await load()
    } catch {
      // silently ignore
    } finally {
      setDeletingId(null)
    }
  }

  const groups = () => groupByDate(sessions())

  return (
    <>
      {/* Backdrop overlay — mobile only */}
      <Show when={props.open}>
        <div
          class="fixed inset-0 bg-black/20 z-20 lg:hidden"
          onClick={props.onToggle}
        />
      </Show>

      {/* Sidebar — always visible on desktop, toggleable on mobile */}
      <aside
        class={`w-72 bg-base-200 border-r border-base-300 flex flex-col flex-shrink-0
                max-lg:fixed max-lg:inset-y-0 max-lg:left-0 max-lg:z-30
                max-lg:transform max-lg:transition-transform max-lg:duration-200
                ${props.open ? 'max-lg:translate-x-0' : 'max-lg:-translate-x-full'}`}
      >
        {/* Sidebar header */}
        <div class="flex items-center justify-between px-4 py-3 border-b border-base-300">
          <span class="text-sm font-semibold text-base-content/70">对话历史</span>
          <button
            class="btn btn-ghost btn-xs btn-square"
            onClick={props.onNewChat}
            aria-label="新建对话"
            title="新建对话"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <line x1="12" y1="5" x2="12" y2="19"/>
              <line x1="5" y1="12" x2="19" y2="12"/>
            </svg>
          </button>
        </div>

        {/* Session list — grouped by date */}
        <div class="flex-1 overflow-y-auto p-3">
          {loading() && (
            <div class="flex justify-center py-6">
              <span class="loading loading-spinner loading-sm text-base-content/30" />
            </div>
          )}

          {!loading() && sessions().length === 0 && (
            <div class="px-2 py-6 text-center text-sm text-base-content/40">
              暂无对话记录
            </div>
          )}

          <For each={groups()}>
            {(group) => (
              <div class="mb-3">
                <div class="px-2 pb-1.5 text-[10px] font-semibold uppercase tracking-widest text-base-content/30">
                  {group.label}
                </div>
                <For each={group.items}>
                  {(s) => (
                    <div
                      class={`group w-full text-left px-3 py-2.5 rounded-lg transition-colors
                               hover:bg-base-300/40 flex items-center gap-2 cursor-pointer
                               ${s.session_id === props.activeSessionId ? 'bg-base-300/60' : ''}`}
                      onClick={() => handleSelectSession(s.session_id)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter' || e.key === ' ') {
                          e.preventDefault()
                          handleSelectSession(s.session_id)
                        }
                      }}
                      role="button"
                      tabindex={0}
                    >
                      {/* Content */}
                      <div class="flex-1 min-w-0">
                        <div class="text-[13px] font-medium truncate leading-tight">
                          {s.title || '新对话'}
                        </div>
                        <div class="flex items-center gap-1.5 mt-0.5">
                          <span class="text-[11px] text-base-content/30 font-mono">
                            {s.message_count} 条
                          </span>
                          <span class="text-[9px] text-base-content/20">·</span>
                          <span class="text-[11px] text-base-content/30">
                            {formatTime(s.updated_at)}
                          </span>
                        </div>
                      </div>

                      {/* Delete button — visible on hover */}
                      <button
                        class={`flex-shrink-0 w-6 h-6 rounded-md flex items-center justify-center
                                 opacity-0 group-hover:opacity-60 hover:opacity-100! transition-opacity
                                 text-base-content/40 hover:text-error hover:bg-error/10
                                 ${deletingId() === s.session_id ? 'opacity-100 pointer-events-none' : ''}`}
                        onClick={(e) => handleDelete(e, s.session_id)}
                        aria-label="删除对话"
                        title="删除对话"
                      >
                        {deletingId() === s.session_id ? (
                          <span class="loading loading-spinner loading-xs text-error" />
                        ) : (
                          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                            <polyline points="3 6 5 6 21 6"/>
                            <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>
                            <line x1="10" y1="11" x2="10" y2="17"/>
                            <line x1="14" y1="11" x2="14" y2="17"/>
                          </svg>
                        )}
                      </button>
                    </div>
                  )}
                </For>
              </div>
            )}
          </For>
        </div>
      </aside>
    </>
  )
}

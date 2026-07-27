/// API client for lorag backend (Rust axum server).
/// In dev mode, Vite proxies /api/* to localhost:3000.

export interface StatusResponse {
  llm_model: string
  embed_model: string
  rerank_model: string
  sources_count: number
  chunks_count: number
}

export interface SessionInfo {
  session_id: string
  title: string
  message_count: number
  updated_at: string
}

export interface MessageRecord {
  role: string
  content: string
}

/// Stream SSE events from /api/chat or /api/query.
async function streamSse(
  endpoint: string,
  body: Record<string, unknown>,
  onToken: (token: string) => void,
  onDone: () => void,
  onError: (msg: string) => void,
  onSession?: (sid: string) => void,
): Promise<void> {
  try {
    const res = await fetch(endpoint, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })

    if (!res.ok) {
      const text = await res.text()
      onError(`HTTP ${res.status}: ${text}`)
      return
    }

    const reader = res.body?.getReader()
    if (!reader) {
      onError('No response body')
      return
    }

    const decoder = new TextDecoder()
    let buffer = ''

    while (true) {
      const { done, value } = await reader.read()
      if (done) break

      buffer += decoder.decode(value, { stream: true })
      const lines = buffer.split('\n')
      buffer = lines.pop() ?? ''

      for (const line of lines) {
        if (line.startsWith('data: ')) {
          const data = line.slice(6).trim()
          if (data === '[DONE]') {
            onDone()
            return
          }
          try {
            const parsed = JSON.parse(data)
            if (parsed.token) {
              onToken(parsed.token)
            } else if (parsed.session_id && onSession) {
              onSession(parsed.session_id)
            }
          } catch {
            if (data) onToken(data)
          }
        }
      }
    }

    onDone()
  } catch (err) {
    onError(err instanceof Error ? err.message : String(err))
  }
}

/// POST /api/chat — streaming multi-turn chat
export function streamChat(
  message: string,
  sessionId: string | null,
  onToken: (t: string) => void,
  onDone: () => void,
  onError: (m: string) => void,
  onSession?: (sid: string) => void,
) {
  return streamSse(
    '/api/chat',
    { message, session_id: sessionId },
    onToken,
    onDone,
    onError,
    onSession,
  )
}

/// POST /api/query — one-shot RAG query
export function streamRagQuery(
  question: string,
  onToken: (t: string) => void,
  onDone: () => void,
  onError: (m: string) => void,
) {
  return streamSse(
    '/api/query',
    { question },
    onToken,
    onDone,
    onError,
  )
}

/// GET /api/status — system info
export async function fetchStatus(): Promise<StatusResponse> {
  const res = await fetch('/api/status')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json()
}

/// GET /api/sessions — session history list
export async function fetchSessions(): Promise<SessionInfo[]> {
  const res = await fetch('/api/sessions')
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json()
}

/// GET /api/sessions/:id — load messages for a session
export async function fetchSessionMessages(sessionId: string): Promise<MessageRecord[]> {
  const res = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}`)
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json()
}

/// DELETE /api/sessions/:id — delete a session and all its messages
export async function deleteSession(sessionId: string): Promise<{ deleted: number }> {
  const res = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}`, {
    method: 'DELETE',
  })
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json()
}

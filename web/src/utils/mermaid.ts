/// Mermaid rendering for chat bubbles.
///
/// `mermaid.render()` is async and generates a unique DOM id per call. Calling
/// it concurrently for several pending blocks can clobber each other, so we
/// funnel all render jobs through a serial microtask queue.
///
/// During streaming the same `<div class="mermaid-pending">` gets re-created
/// over and over as the bubble re-parses incoming tokens — without caching,
/// every chunk would force the user to wait through a fresh render. We keep
/// a small `svgCache` keyed by source code so the second-and-later passes
/// restore the previously rendered SVG immediately and only kick off a render
/// for genuinely new blocks.

import mermaid from 'mermaid'

let initialized = false
let counter = 0
const queue: Array<() => Promise<void>> = []
let running = false
const svgCache = new Map<string, string>()

function isDark(): boolean {
  return (
    typeof document !== 'undefined' &&
    document.documentElement.classList.contains('dark')
  )
}

function init(): void {
  if (initialized) return
  initialized = true
  mermaid.initialize({
    startOnLoad: false,
    theme: isDark() ? 'dark' : 'default',
    securityLevel: 'strict',
    fontFamily: 'inherit',
  })
}

async function processQueue(): Promise<void> {
  if (running) return
  running = true
  while (queue.length > 0) {
    const job = queue.shift()
    if (job) await job()
  }
  running = false
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

/**
 * Apply rendered HTML into `container` while preserving previously rendered
 * mermaid SVGs. After streaming re-renders wipe and re-create the
 * `.mermaid-pending` nodes, this restores the cached SVG into any node whose
 * `data-source` we already know how to draw.
 */
export function applyHtml(container: HTMLElement, html: string): void {
  if (!html) {
    container.innerHTML = ''
    return
  }
  container.innerHTML = html
  for (const node of Array.from(
    container.querySelectorAll<HTMLElement>('.mermaid-pending'),
  )) {
    const src = node.dataset.source ?? ''
    const cached = svgCache.get(src)
    if (cached) {
      node.innerHTML = cached
      node.classList.remove('mermaid-pending')
      node.classList.add('mermaid-rendered')
    }
  }
}

/**
 * Scan `container` for any remaining `.mermaid-pending` nodes and kick off
 * a render job for each. Idempotent — already-rendered/error nodes are
 * ignored. Returns immediately; the actual SVG injection happens in the
 * microtask queue.
 */
export function renderMermaidBlocks(container: HTMLElement): void {
  init()
  const blocks = Array.from(
    container.querySelectorAll<HTMLElement>('.mermaid-pending'),
  )
  if (blocks.length === 0) return

  for (const block of blocks) {
    const code = block.dataset.source ?? ''
    if (!code) continue
    const id = `mermaid-${++counter}-${Date.now().toString(36)}`
    block.classList.remove('mermaid-pending')
    block.classList.add('mermaid-loading')
    block.innerHTML = `<span class="text-xs opacity-60">⏳ 渲染图表…</span>`

    const source = code
    queue.push(async () => {
      try {
        const { svg } = await mermaid.render(id, source)
        svgCache.set(source, svg)
        if (!block.isConnected) return
        block.classList.remove('mermaid-loading')
        block.innerHTML = svg
        block.classList.add('mermaid-rendered')
      } catch (err) {
        if (!block.isConnected) return
        block.classList.remove('mermaid-loading')
        block.classList.add('mermaid-error')
        const msg = escapeHtml(
          err instanceof Error ? err.message : String(err),
        )
        block.innerHTML =
          `<div class="text-xs text-error mb-1 font-medium">⚠️ Mermaid 渲染失败：${msg}</div>` +
          `<pre class="text-xs whitespace-pre-wrap opacity-70 overflow-x-auto">${escapeHtml(source)}</pre>`
      }
    })
  }
  void processQueue()
}

/** Test/debug helper: clear the in-memory SVG cache. */
export function _resetMermaidCache(): void {
  svgCache.clear()
}

/// Markdown helpers for the chat bubble.
///
/// `marked` will happily tokenize everything inside a fenced code block, but it
/// has no understanding of Mermaid syntax — so an unfinished mermaid block in
/// the middle of streaming can confuse parsing. We pre-extract complete
/// ```mermaid … ``` blocks, replace them with neutral placeholders, run marked
/// over the rest, then substitute the placeholders back as <div class="mermaid-pending">

interface MermaidBlock {
  /** opaque placeholder string inserted into the markdown text */
  token: string
  /** original mermaid source (trimmed of trailing whitespace) */
  code: string
}

/**
 * Extract complete ```mermaid … ``` blocks from `text` and replace each with
 * a placeholder token. Incomplete blocks (no closing fence yet, e.g. mid-
 * stream) are left untouched so `marked` will render the raw fence as a
 * normal code block until the closing fence arrives.
 *
 * ⚠️ Token characters must avoid any markdown-significant glyphs (`_` `*`
 * `` ` `` `[` `]` `(` `)` `#` `>` `<` `!` `|` `~`). Earlier used `{{__MERMAID_0__}}`
 * and got mangled by `marked` — `__MERMAID_0__` matched GFM strong emphasis
 * and was rendered as `<strong>MERMAID_0</strong>`, leaving `{{`/`}}` visible
 * and the placeholder unfindable by the restore regex.
 */
export function extractMermaidBlocks(text: string): {
  cleaned: string
  blocks: MermaidBlock[]
} {
  const blocks: MermaidBlock[] = []
  let counter = 0
  const cleaned = text.replace(/```mermaid\s*\n([\s\S]*?)```/g, (_m, code) => {
    const token = `M10MERMAIDTOKEN${counter++}END`
    blocks.push({ token, code: code.replace(/\s+$/, '') })
    return token
  })
  return { cleaned, blocks }
}

/**
 * Inverse of `extractMermaidBlocks`: swap each placeholder token in the
 * rendered HTML for a `<div class="mermaid-pending">` element whose
 * `data-source` attribute carries the original Mermaid source (HTML-escaped).
 * The actual rendering happens later in `utils/mermaid.ts`.
 */
export function restoreMermaidBlocks(html: string, blocks: MermaidBlock[]): string {
  let result = html
  for (const { token, code } of blocks) {
    const escaped = code
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
    const replacement = `<div class="mermaid-pending" data-source="${escaped}"></div>`
    // The token may have been wrapped in a <p> by marked (block elements
    // are wrapped in <p> when surrounded by blank lines). Strip the wrapper
    // so the mermaid container becomes a direct child of .md-content.
    result = result.replace(
      new RegExp(`<p>${escapeRegex(token)}</p>\\n?`),
      replacement,
    )
    // Fallback: token may not be paragraph-wrapped (e.g. at start/end of input).
    result = result.replace(new RegExp(escapeRegex(token), 'g'), replacement)
  }
  return result
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

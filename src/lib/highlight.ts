import hljs from "highlight.js/lib/common";

/** Maximum character length highlighted synchronously as a whole file (~350 KB). */
export const MAX_HIGHLIGHT_CHARS = 350_000;

export function escapeHtml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/**
 * Split `highlight.js` HTML output into one self-contained HTML string per line,
 * balancing `<span ...>` tags that cross newline boundaries (such as block comments
 * and multi-line template strings) so each line row renders independently.
 */
export function splitHighlightedHtmlByLine(html: string): string[] {
  const rawLines = html.split("\n");
  const openStack: string[] = [];
  const out: string[] = [];
  const tagPattern = /<(\/?)span(\b[^>]*)?>/g;

  for (const raw of rawLines) {
    const prefix = openStack.join("");
    tagPattern.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = tagPattern.exec(raw)) !== null) {
      if (match[1] === "/") {
        openStack.pop();
      } else {
        openStack.push(match[0]);
      }
    }
    const suffix = "</span>".repeat(openStack.length);
    out.push(prefix + raw + suffix);
  }
  return out;
}

/** Highlight `code` into an array of per-line HTML strings. Falls back to plain escaped lines when huge or unknown. */
export function highlightLines(code: string, language?: string | null): string[] {
  if (code.length === 0) return [];
  if (code.length > MAX_HIGHLIGHT_CHARS) {
    return code.split("\n").map(escapeHtml);
  }
  try {
    const html =
      language && hljs.getLanguage(language)
        ? hljs.highlight(code, { language, ignoreIllegals: true }).value
        : escapeHtml(code);
    return splitHighlightedHtmlByLine(html);
  } catch {
    return code.split("\n").map(escapeHtml);
  }
}

export interface LineMatch {
  /** 1-based line number. */
  line: number;
  /** Number of occurrences on this line. */
  count: number;
}

/** Case-insensitive search across raw text lines, returning 1-based matching line numbers. */
export function findInLines(lines: string[], query: string): LineMatch[] {
  const needle = query.toLowerCase();
  if (!needle) return [];
  const found: LineMatch[] = [];
  for (let i = 0; i < lines.length; i++) {
    const hay = lines[i].toLowerCase();
    let at = 0;
    let count = 0;
    while (true) {
      const idx = hay.indexOf(needle, at);
      if (idx === -1) break;
      count++;
      at = idx + Math.max(needle.length, 1);
    }
    if (count > 0) found.push({ line: i + 1, count });
  }
  return found;
}

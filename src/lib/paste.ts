export type PasteKind = "svg" | "html" | "json" | "xml" | "markdown" | "text";

export interface DetectedPaste {
  kind: PasteKind;
  ext: string;
  label: string;
  lines: number;
}

/** Thresholds above which a pasted plain-text snippet collapses into a clickable chip instead of flooding the composer. */
export const PASTE_COLLAPSE_CHARS = 1200;
export const PASTE_COLLAPSE_LINES = 12;
/** Complete SVG / HTML snippets collapse even when shorter so they immediately appear as typed `.svg` / `.html` chips. */
export const PASTE_RICH_MIN_CHARS = 200;

export function countLines(text: string): number {
  if (!text) return 0;
  let lines = 1;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) lines++;
  }
  return lines;
}

/** Infer the file kind and extension of pasted plain text (`svg`, `html`, `json`, `xml`, `markdown`, or `text`). */
export function detectPasteKind(raw: string): DetectedPaste {
  const trimmed = raw.trim();
  const lines = countLines(raw);

  if (
    /^(?:<\?xml[\s\S]*?\?>\s*)?(?:<!--[\s\S]*?-->\s*)*<svg[\s>]/i.test(trimmed) &&
    /<\/svg>\s*$/i.test(trimmed)
  ) {
    return { kind: "svg", ext: "svg", label: "SVG", lines };
  }

  if (
    /^<!doctype\s+html\b/i.test(trimmed) ||
    (/^<html[\s>]/i.test(trimmed) && /<\/html>\s*$/i.test(trimmed)) ||
    (/^<(?:head|body|div|section|article|main|header|footer|nav|table|form|style)[\s>]/i.test(trimmed) &&
      /<\/(?:html|body|div|section|article|main|header|footer|nav|table|form)>\s*$/i.test(trimmed) &&
      (trimmed.match(/<[a-z][^>]*>/gi)?.length ?? 0) >= 4)
  ) {
    return { kind: "html", ext: "html", label: "HTML", lines };
  }

  if (/^<\?xml[\s?]/i.test(trimmed)) {
    return { kind: "xml", ext: "xml", label: "XML", lines };
  }

  if ((trimmed.startsWith("{") && trimmed.endsWith("}")) || (trimmed.startsWith("[") && trimmed.endsWith("]"))) {
    if (trimmed.length <= 512_000) {
      try {
        JSON.parse(trimmed);
        return { kind: "json", ext: "json", label: "JSON", lines };
      } catch {
        // Not valid JSON; fall through.
      }
    } else if (/^[\[{]\s*"/.test(trimmed)) {
      return { kind: "json", ext: "json", label: "JSON", lines };
    }
  }

  let mdSignals = 0;
  if (/^---\r?\n[\s\S]+?\r?\n---\r?\n/.test(trimmed)) mdSignals += 2;
  if (/^#{1,6}\s+\S/m.test(trimmed)) mdSignals++;
  if (/^```[\w-]*\s*$/m.test(trimmed)) mdSignals++;
  if (/^\|.+\|\s*\r?\n\|[\s:|-]+\|/m.test(trimmed)) mdSignals++;
  if ((trimmed.match(/^(?:[-*+]|\d+\.)\s+\S/gm)?.length ?? 0) >= 3) mdSignals++;
  if (/^>\s+\S/m.test(trimmed)) mdSignals++;
  if (/\[[^\]\n]+\]\(https?:\/\/[^)\s]+\)/.test(trimmed)) mdSignals++;
  if (mdSignals >= 2) {
    return { kind: "markdown", ext: "md", label: "Markdown", lines };
  }

  return { kind: "text", ext: "txt", label: "文本", lines };
}

/** Whether a pasted text snippet should collapse into a clickable attachment chip instead of filling the textarea. */
export function shouldCollapsePaste(raw: string): boolean {
  const trimmed = raw.trim();
  if (!trimmed) return false;
  const detected = detectPasteKind(raw);
  if ((detected.kind === "svg" || detected.kind === "html" || detected.kind === "markdown") && trimmed.length >= PASTE_RICH_MIN_CHARS) {
    return true;
  }
  return raw.length >= PASTE_COLLAPSE_CHARS || detected.lines >= PASTE_COLLAPSE_LINES;
}

/** Pick a collision-free filename like `pasted-1.svg`, `pasted-2.html`, or `pasted-1.txt`. */
export function nextPastedFilename(ext: string, existingNames: Iterable<string>): string {
  const taken = new Set(existingNames);
  for (let n = 1; ; n++) {
    const candidate = `pasted-${n}.${ext}`;
    if (!taken.has(candidate)) return candidate;
  }
}

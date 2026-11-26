/**
 * Pure parser and classifier for shell command output shown inside ToolCard.
 * Handles ANSI SGR styling (preserving state across lines), strips non-SGR control
 * sequences, and detects unified/normal diff outputs for semantic highlighting.
 */

export type OutputKind = "unified-diff" | "normal-diff" | "plain";

export type LineKind =
  | "normal"
  | "diff-meta"
  | "diff-hunk"
  | "diff-sep"
  | "diff-add"
  | "diff-del"
  | "log-error"
  | "log-warn";

export interface AnsiStyle {
  /** Standard 0-15 ANSI color index, or a CSS rgb(...) string for 256/truecolor. */
  fg?: number | string;
  bg?: number | string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
}

export interface AnsiSegment {
  text: string;
  style?: AnsiStyle;
  /** Semantic token highlight for plain (non-ANSI) text, e.g. file paths with optional :line:col. */
  token?: "path";
}

export interface ParsedLine {
  segments: AnsiSegment[];
  kind: LineKind;
  hasAnsi: boolean;
}

// Matches SGR (\x1b[...m), other CSI (\x1b[...letter), and OSC (\x1b]...BEL/ST) sequences.
// biome-ignore lint/suspicious/noControlCharactersInRegex: intentionally parsing ANSI escape sequences
const ANSI_TOKEN_RE = /\x1b(?:\[([0-9;?]*)([A-Za-z])|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])/g;

const NORMAL_DIFF_HUNK_RE = /^(\d+)(?:,(\d+))?([acd])(\d+)(?:,(\d+))?$/;
const UNIFIED_DIFF_HUNK_RE = /^@@ -\d+(?:,\d+)? \+\d+(?:,\d+)? @@/;

function formatNormalDiffHunk(raw: string): string {
  const match = NORMAL_DIFF_HUNK_RE.exec(raw);
  if (!match) return raw;
  const oldStart = Number.parseInt(match[1], 10);
  const oldEnd = match[2] ? Number.parseInt(match[2], 10) : oldStart;
  const op = match[3];
  const newStart = Number.parseInt(match[4], 10);
  const newEnd = match[5] ? Number.parseInt(match[5], 10) : newStart;
  const fmt = (start: number, count: number) => (count === 1 ? `${start}` : `${start},${count}`);

  if (op === "d") {
    return `@@ -${fmt(oldStart, oldEnd - oldStart + 1)} +${newStart},0 @@`;
  }
  if (op === "a") {
    return `@@ -${oldStart},0 +${fmt(newStart, newEnd - newStart + 1)} @@`;
  }
  return `@@ -${fmt(oldStart, oldEnd - oldStart + 1)} +${fmt(newStart, newEnd - newStart + 1)} @@`;
}
const LOG_ERROR_RE = /^(?:error(?:\[[^\]]+\])?:|fatal:|FAILED\b|panic:|Traceback \(most recent call last\):)/i;
const LOG_WARN_RE = /^(?:warning(?:\[[^\]]+\])?:|warn:)/i;
// Matches relative/absolute source file paths (with at least one slash and a known-style extension) and optional :line(:col).
const FILE_PATH_RE = /(?<![A-Za-z0-9_.:/-])((?:~\/|\/(?!\/)|\.\.?\/|[A-Za-z0-9_.-]+\/)[A-Za-z0-9_./-]*\.[A-Za-z0-9]{1,8}(?::\d+(?::\d+)?)?)/g;

function splitPathTokens(text: string): AnsiSegment[] {
  const out: AnsiSegment[] = [];
  FILE_PATH_RE.lastIndex = 0;
  let last = 0;
  let match = FILE_PATH_RE.exec(text);
  while (match !== null) {
    const start = match.index;
    const hit = match[1];
    if (start > last) out.push({ text: text.slice(last, start) });
    out.push({ text: hit, token: "path" });
    last = start + hit.length;
    match = FILE_PATH_RE.exec(text);
  }
  if (last === 0) return [{ text }];
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}

function ansi256ToRgb(code: number): string | undefined {
  if (code < 0 || code > 255) return undefined;
  if (code < 16) return undefined; // handled as standard 0-15 index
  if (code < 232) {
    const idx = code - 16;
    const r = Math.floor(idx / 36);
    const g = Math.floor((idx % 36) / 6);
    const b = idx % 6;
    const toByte = (v: number) => (v === 0 ? 0 : 55 + v * 40);
    return `rgb(${toByte(r)}, ${toByte(g)}, ${toByte(b)})`;
  }
  const gray = 8 + (code - 232) * 10;
  return `rgb(${gray}, ${gray}, ${gray})`;
}

function isEmptyStyle(style: AnsiStyle): boolean {
  return (
    style.fg === undefined &&
    style.bg === undefined &&
    !style.bold &&
    !style.dim &&
    !style.italic &&
    !style.underline
  );
}

function applySgr(current: AnsiStyle, rawParams: string): AnsiStyle {
  const next: AnsiStyle = { ...current };
  const params = rawParams === "" ? [0] : rawParams.split(";").map((part) => (part === "" ? 0 : Number.parseInt(part, 10)));

  for (let i = 0; i < params.length; i++) {
    const code = params[i];
    if (Number.isNaN(code)) continue;
    if (code === 0) {
      next.fg = undefined;
      next.bg = undefined;
      next.bold = undefined;
      next.dim = undefined;
      next.italic = undefined;
      next.underline = undefined;
    } else if (code === 1) {
      next.bold = true;
    } else if (code === 2) {
      next.dim = true;
    } else if (code === 3) {
      next.italic = true;
    } else if (code === 4) {
      next.underline = true;
    } else if (code === 22) {
      next.bold = undefined;
      next.dim = undefined;
    } else if (code === 23) {
      next.italic = undefined;
    } else if (code === 24) {
      next.underline = undefined;
    } else if (code >= 30 && code <= 37) {
      next.fg = code - 30;
    } else if (code === 39) {
      next.fg = undefined;
    } else if (code >= 40 && code <= 47) {
      next.bg = code - 40;
    } else if (code === 49) {
      next.bg = undefined;
    } else if (code >= 90 && code <= 97) {
      next.fg = code - 90 + 8;
    } else if (code >= 100 && code <= 107) {
      next.bg = code - 100 + 8;
    } else if (code === 38 || code === 48) {
      const target = code === 38 ? "fg" : "bg";
      const mode = params[i + 1];
      if (mode === 5 && i + 2 < params.length) {
        const colorCode = params[i + 2];
        if (colorCode >= 0 && colorCode < 16) {
          next[target] = colorCode;
        } else {
          next[target] = ansi256ToRgb(colorCode);
        }
        i += 2;
      } else if (mode === 2 && i + 4 < params.length) {
        const r = Math.max(0, Math.min(255, params[i + 2] ?? 0));
        const g = Math.max(0, Math.min(255, params[i + 3] ?? 0));
        const b = Math.max(0, Math.min(255, params[i + 4] ?? 0));
        next[target] = `rgb(${r}, ${g}, ${b})`;
        i += 4;
      }
    }
  }
  return next;
}

/** Strip all ANSI escape codes from a single line for classification and plain copying. */
export function stripAnsi(text: string): string {
  return text.replace(ANSI_TOKEN_RE, "");
}

/** Detect whether command output looks like a unified diff, normal diff, or plain output. */
export function detectOutputKind(lines: string[]): OutputKind {
  const sampleLimit = Math.min(lines.length, 200);
  let unifiedSignals = 0;
  let normalHunks = 0;
  let normalMarkers = 0;

  for (let i = 0; i < sampleLimit; i++) {
    const clean = stripAnsi(lines[i].replace(/\r$/, ""));
    if (UNIFIED_DIFF_HUNK_RE.test(clean) || clean.startsWith("diff --git ")) {
      return "unified-diff";
    }
    if (clean.startsWith("--- ") || clean.startsWith("+++ ")) {
      unifiedSignals++;
    }
    if (NORMAL_DIFF_HUNK_RE.test(clean)) {
      normalHunks++;
    } else if (clean.startsWith("< ") || clean.startsWith("> ")) {
      normalMarkers++;
    }
  }

  if (unifiedSignals >= 2) return "unified-diff";
  if (normalHunks >= 1 && normalMarkers >= 1) return "normal-diff";
  return "plain";
}

function classifyPlainLine(clean: string, kind: OutputKind, hasAnsi: boolean): LineKind {
  if (kind === "unified-diff") {
    if (UNIFIED_DIFF_HUNK_RE.test(clean)) return "diff-hunk";
    if (
      clean.startsWith("diff --git ") ||
      clean.startsWith("index ") ||
      clean.startsWith("--- ") ||
      clean.startsWith("+++ ")
    ) {
      return "diff-meta";
    }
    if (clean.startsWith("+")) return "diff-add";
    if (clean.startsWith("-")) return "diff-del";
    return "normal";
  }

  if (kind === "normal-diff") {
    if (NORMAL_DIFF_HUNK_RE.test(clean)) return "diff-hunk";
    if (clean === "---") return "diff-sep";
    if (clean.startsWith("> ") || clean === ">") return "diff-add";
    if (clean.startsWith("< ") || clean === "<") return "diff-del";
    return "normal";
  }

  if (!hasAnsi) {
    const trimmed = clean.trimStart();
    if (LOG_ERROR_RE.test(trimmed)) return "log-error";
    if (LOG_WARN_RE.test(trimmed)) return "log-warn";
  }
  return "normal";
}

/** Parse lines with persistent ANSI SGR state and semantic line classification. */
export function parseTerminalLines(lines: string[], kind: OutputKind): ParsedLine[] {
  let state: AnsiStyle = {};
  const result: ParsedLine[] = [];

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i].replace(/\r$/, "");
    const segments: AnsiSegment[] = [];
    let hasAnsi = !isEmptyStyle(state);
    let lastIndex = 0;
    let plain = "";

    ANSI_TOKEN_RE.lastIndex = 0;
    let match = ANSI_TOKEN_RE.exec(raw);
    while (match !== null) {
      if (match.index > lastIndex) {
        const chunk = raw.slice(lastIndex, match.index);
        plain += chunk;
        segments.push(isEmptyStyle(state) ? { text: chunk } : { text: chunk, style: { ...state } });
      }
      const params = match[1];
      const finalChar = match[2];
      if (finalChar === "m" && params !== undefined) {
        state = applySgr(state, params);
        if (!isEmptyStyle(state)) hasAnsi = true;
      }
      lastIndex = ANSI_TOKEN_RE.lastIndex;
      match = ANSI_TOKEN_RE.exec(raw);
    }

    if (lastIndex < raw.length) {
      const tail = raw.slice(lastIndex);
      plain += tail;
      segments.push(isEmptyStyle(state) ? { text: tail } : { text: tail, style: { ...state } });
    }

    if (segments.length === 0) {
      segments.push({ text: "" });
    }

    const lineKind = classifyPlainLine(plain, kind, hasAnsi);
    const finalSegments =
      kind === "plain" && !hasAnsi && segments.length === 1 && segments[0].text.includes("/")
        ? splitPathTokens(segments[0].text)
        : segments;

    if (kind === "normal-diff") {
      if (lineKind === "diff-hunk" && finalSegments[0]) {
        finalSegments[0] = { ...finalSegments[0], text: formatNormalDiffHunk(plain) };
      } else if (lineKind === "diff-add" || lineKind === "diff-del") {
        const marker = lineKind === "diff-add" ? "+" : "-";
        const first = finalSegments[0];
        if (first && (first.text.startsWith(">") || first.text.startsWith("<"))) {
          finalSegments[0] = { ...first, text: marker + first.text.slice(1) };
        }
      }
    }

    result.push({
      segments: finalSegments,
      kind: lineKind,
      hasAnsi,
    });
  }

  return result;
}

import { common, createLowlight } from "lowlight";

type HastText = { type: "text"; value: string };
type HastElement = {
  type: "element";
  tagName: string;
  properties?: { className?: unknown };
  children?: HastNode[];
};
type HastRoot = { type: "root"; children?: HastNode[] };
type HastNode = HastText | HastElement | { type: string; children?: HastNode[]; value?: string };

/** Single shared `lowlight` grammar registry used across both CodeViewer/DiffViewer and Markdown code blocks. */
export const lowlight = createLowlight(common);

/** Maximum character length highlighted synchronously as a whole file (~350 KB). */
export const MAX_HIGHLIGHT_CHARS = 350_000;
/** Maximum line count highlighted synchronously as a whole file so multi-thousand-line files mount in <30ms. */
export const MAX_HIGHLIGHT_LINES = 4_000;

const HTML_ESCAPE_RE = /[&<>"]/;

export function escapeHtml(text: string): string {
  if (!HTML_ESCAPE_RE.test(text)) return text;
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function hastNodesToHtml(nodes: HastNode[] | undefined): string {
  if (!nodes || nodes.length === 0) return "";
  let out = "";
  for (const node of nodes) {
    if (node.type === "text") {
      out += escapeHtml((node as HastText).value);
    } else if (node.type === "element") {
      const el = node as HastElement;
      const rawCls = el.properties?.className;
      const cls = Array.isArray(rawCls)
        ? rawCls.map(String).join(" ")
        : typeof rawCls === "string"
          ? rawCls
          : "";
      const open = cls ? `<${el.tagName} class="${escapeHtml(cls)}">` : `<${el.tagName}>`;
      out += `${open}${hastNodesToHtml(el.children)}</${el.tagName}>`;
    } else if ("children" in node && node.children) {
      out += hastNodesToHtml(node.children);
    }
  }
  return out;
}

function extractHastText(node: HastNode): string {
  if (node.type === "text") return (node as HastText).value;
  if (!("children" in node) || !node.children) return "";
  let out = "";
  for (const child of node.children) out += extractHastText(child);
  return out;
}

function detectCodeLanguage(el: HastElement): string | false | undefined {
  const list = el.properties?.className;
  if (!Array.isArray(list)) return undefined;
  let found: string | undefined;
  for (const item of list) {
    const val = String(item);
    if (val === "no-highlight" || val === "nohighlight") return false;
    if (!found && val.startsWith("lang-")) found = val.slice(5);
    if (!found && val.startsWith("language-")) found = val.slice(9);
  }
  return found;
}

/** Rehype plugin highlighting `<pre><code class="language-...">` blocks via the shared `lowlight` instance. */
export function rehypeSharedHighlight() {
  const visit = (node: HastNode, parent?: HastNode) => {
    if (
      node.type === "element" &&
      (node as HastElement).tagName === "code" &&
      parent?.type === "element" &&
      (parent as HastElement).tagName === "pre"
    ) {
      const codeEl = node as HastElement;
      const lang = detectCodeLanguage(codeEl);
      if (lang && lowlight.registered(lang)) {
        const props = codeEl.properties ?? {};
        const classes = Array.isArray(props.className) ? [...props.className] : [];
        if (!classes.includes("hljs")) classes.unshift("hljs");
        props.className = classes;
        codeEl.properties = props;
        const text = extractHastText(codeEl);
        try {
          const highlighted = lowlight.highlight(lang, text);
          if (highlighted.children.length > 0) {
            codeEl.children = highlighted.children as unknown as HastNode[];
          }
        } catch {
          // Leave unhighlighted text intact on unexpected grammar error
        }
      }
      return;
    }
    if ("children" in node && node.children) {
      for (const child of node.children) visit(child, node);
    }
  };
  return (tree: HastRoot) => {
    visit(tree);
  };
}

/**
 * Split `highlight.js` / `lowlight` HTML output into one self-contained HTML string per line,
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
    let match: RegExpExecArray | null = tagPattern.exec(raw);
    while (match !== null) {
      if (match[1] === "/") {
        openStack.pop();
      } else {
        openStack.push(match[0]);
      }
      match = tagPattern.exec(raw);
    }
    const suffix = "</span>".repeat(openStack.length);
    out.push(prefix + raw + suffix);
  }
  return out;
}

/** Highlight `code` into an array of per-line HTML strings. Falls back to plain escaped lines when huge or unknown. */
export function highlightLines(code: string, language?: string | null): string[] {
  if (code.length === 0) return [];
  const rawLines = code.split("\n");
  if (code.length > MAX_HIGHLIGHT_CHARS || rawLines.length > MAX_HIGHLIGHT_LINES) {
    return rawLines.map(escapeHtml);
  }
  try {
    if (language && lowlight.registered(language)) {
      const root = lowlight.highlight(language, code);
      const html = hastNodesToHtml(root.children as unknown as HastNode[]);
      return splitHighlightedHtmlByLine(html);
    }
    return rawLines.map(escapeHtml);
  } catch {
    return rawLines.map(escapeHtml);
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

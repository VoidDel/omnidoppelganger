import { Fragment, type ReactNode, isValidElement, memo, useEffect, useRef, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { api } from "../lib/api";
import { rehypeSharedHighlight } from "../lib/highlight";
import { normalizeMath } from "../lib/math";
import type { DiffHunk } from "../lib/types";
import { Icon } from "./Icon";

const EXTERNAL = /^(https?:|mailto:)/i;

function Link({ href, children }: { href?: string; children?: ReactNode }) {
  if (!href || !EXTERNAL.test(href)) return <span title={href}>{children}</span>;
  return (
    <a
      href={href}
      title={href}
      onClick={(event) => {
        event.preventDefault();
        api.openExternal(href).catch((error) => console.error(error));
      }}
    >
      {children}
    </a>
  );
}

function useCopy(): [boolean, (text: string) => void] {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const copy = (text: string) => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(true);
        window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setCopied(false), 1500);
      },
      (error) => console.error(error),
    );
  };
  return [copied, copy];
}

function CodeBlock({ children }: { children?: ReactNode }) {
  const block = useRef<HTMLPreElement>(null);
  const written = isValidElement<{ className?: string }>(children)
    ? /language-([\w+#.-]+)/.exec(children.props.className ?? "")?.[1]
    : undefined;
  const [copied, copyText] = useCopy();
  const copy = () => copyText(block.current?.textContent ?? "");
  return (
    <div className="code-block">
      <div className="code-tools">
        {written && <span className="lang">{written}</span>}
        <button type="button" className="button text small copy" onClick={copy}>
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      <pre ref={block}>{children}</pre>
    </div>
  );
}

function Table({ children }: { children?: ReactNode }) {
  return (
    <div className="table-wrap">
      <table>{children}</table>
    </div>
  );
}

export const markdownParts = { a: Link, pre: CodeBlock, table: Table };
export const markdownSyntax: Parameters<typeof Markdown>[0]["remarkPlugins"] = [remarkGfm];
export const markdownPlugins: Parameters<typeof Markdown>[0]["rehypePlugins"] = [rehypeSharedHighlight];

type RemarkPlugins = Parameters<typeof Markdown>[0]["remarkPlugins"];
type RehypePlugins = Parameters<typeof Markdown>[0]["rehypePlugins"];

interface LoadedMathPlugins {
  remark: RemarkPlugins;
  rehype: RehypePlugins;
}

let cachedMathPlugins: LoadedMathPlugins | null = null;
let mathPluginsPromise: Promise<LoadedMathPlugins> | null = null;

export function hasMathDelimiters(text: string): boolean {
  return text.includes("$") || text.includes("\\(") || text.includes("\\[");
}

export function loadMathPlugins(): Promise<LoadedMathPlugins> {
  if (cachedMathPlugins) return Promise.resolve(cachedMathPlugins);
  if (!mathPluginsPromise) {
    mathPluginsPromise = import("./MathPlugins").then(({ remarkMath, rehypeKatex }) => {
      cachedMathPlugins = {
        remark: [remarkGfm, remarkMath],
        rehype: [rehypeSharedHighlight, [rehypeKatex, { throwOnError: false, strict: false }]],
      };
      return cachedMathPlugins;
    });
  }
  return mathPluginsPromise;
}

function stripInlineCode(line: string): string {
  return line.replace(/`+[^`]*`+/g, "");
}

function countDoubleDollars(line: string): number {
  let count = 0;
  for (let i = 0; i < line.length - 1; i++) {
    if (line[i] === "\\" && (line[i + 1] === "$" || line[i + 1] === "\\")) {
      i++;
      continue;
    }
    if (line[i] === "$" && line[i + 1] === "$") {
      count++;
      i++;
    }
  }
  return count;
}

function updateBracketMath(line: string, open: boolean): boolean {
  let state = open;
  for (let i = 0; i < line.length - 1; i++) {
    if (line[i] === "\\" && line[i + 1] === "\\") {
      i++;
      continue;
    }
    if (line[i] === "\\" && line[i + 1] === "[") {
      state = true;
      i++;
    } else if (line[i] === "\\" && line[i + 1] === "]") {
      state = false;
      i++;
    }
  }
  return state;
}

function isListOrIndentedLine(line: string): boolean {
  return /^ {2,}\S/.test(line) || /^ {0,3}(?:[-+*]|\d+[.)])\s/.test(line);
}

/**
 * Split streaming Markdown at top-level `\n\n` paragraph boundaries outside fenced code blocks (` ``` ` / `~~~`)
 * and display math blocks (`$$` / `\[...\]`). Closed prefix chunks are memo-cached; only `tail` re-parses per frame.
 */
export function splitStreamingMarkdown(text: string): { frozen: string[]; tail: string } {
  if (!text.includes("\n\n")) return { frozen: [], tail: text };

  const lines = text.split("\n");
  const frozen: string[] = [];
  let chunkStartLine = 0;
  let fence: { char: "`" | "~"; len: number } | null = null;
  let inDollarMath = false;
  let inBracketMath = false;

  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    const trimmedStart = line.replace(/^ {0,3}/, "");

    if (fence !== null) {
      const closeRe = fence.char === "`" ? new RegExp(`^\`{${fence.len},}[ \\t]*$`) : new RegExp(`^~{${fence.len},}[ \\t]*$`);
      if (closeRe.test(trimmedStart)) {
        fence = null;
      }
      i++;
      continue;
    }

    if (!inDollarMath && !inBracketMath) {
      const backtickMatch = /^(`{3,})([^`]*)$/.exec(trimmedStart);
      const tildeMatch = !backtickMatch ? /^(~{3,})(.*)$/.exec(trimmedStart) : null;
      if (backtickMatch) {
        fence = { char: "`", len: backtickMatch[1].length };
        i++;
        continue;
      }
      if (tildeMatch) {
        fence = { char: "~", len: tildeMatch[1].length };
        i++;
        continue;
      }
    }

    if (line.trim() === "" && !inDollarMath && !inBracketMath) {
      let blankEnd = i;
      while (blankEnd < lines.length && lines[blankEnd].trim() === "") {
        blankEnd++;
      }
      if (blankEnd > chunkStartLine && blankEnd < lines.length) {
        const prevChunkLines = lines.slice(chunkStartLine, i);
        const nextLine = lines[blankEnd];
        const prevIsList = prevChunkLines.some(isListOrIndentedLine);
        const nextContinuesList = prevIsList && isListOrIndentedLine(nextLine);
        if (!nextContinuesList && prevChunkLines.some((l) => l.trim() !== "")) {
          frozen.push(prevChunkLines.join("\n"));
          chunkStartLine = blankEnd;
          i = blankEnd;
          continue;
        }
      }
      i = blankEnd;
      continue;
    }

    const stripped = stripInlineCode(line);
    inBracketMath = updateBracketMath(stripped, inBracketMath);
    if (countDoubleDollars(stripped) % 2 === 1) {
      inDollarMath = !inDollarMath;
    }
    i++;
  }

  return {
    frozen,
    tail: lines.slice(chunkStartLine).join("\n"),
  };
}

/** Markdown renderer that lazily loads KaTeX + remark-math + rehype-katex only when math delimiters appear. */
export const MarkdownContent = memo(function MarkdownContent({ text }: { text: string }) {
  const needsMath = hasMathDelimiters(text);
  const [mathPlugins, setMathPlugins] = useState<LoadedMathPlugins | null>(() => (needsMath ? cachedMathPlugins : null));

  useEffect(() => {
    if (!needsMath) return;
    if (cachedMathPlugins) {
      if (mathPlugins !== cachedMathPlugins) setMathPlugins(cachedMathPlugins);
      return;
    }
    let alive = true;
    loadMathPlugins().then((loaded) => {
      if (alive) setMathPlugins(loaded);
    });
    return () => {
      alive = false;
    };
  }, [needsMath, mathPlugins]);

  const activeMath = needsMath ? (mathPlugins ?? cachedMathPlugins) : null;
  const content = activeMath ? normalizeMath(text) : text;

  return (
    <Markdown
      components={markdownParts}
      remarkPlugins={activeMath ? activeMath.remark : markdownSyntax}
      rehypePlugins={activeMath ? activeMath.rehype : markdownPlugins}
    >
      {content}
    </Markdown>
  );
});

const FrozenMarkdownBlock = memo(function FrozenMarkdownBlock({ text }: { text: string }) {
  return <MarkdownContent text={text} />;
});

/** Streaming Markdown renderer that freezes completed prefix blocks and only re-parses the active tail. */
export function StreamingMarkdown({ text }: { text: string }) {
  const { frozen, tail } = splitStreamingMarkdown(text);
  const hasTail = tail.trim().length > 0;
  return (
    <div className="prose">
      {frozen.map((chunk, index) => (
        <Fragment key={index}>
          {index > 0 ? "\n" : null}
          <FrozenMarkdownBlock text={chunk} />
        </Fragment>
      ))}
      {hasTail ? (
        <Fragment>
          {frozen.length > 0 ? "\n" : null}
          <MarkdownContent text={tail} />
        </Fragment>
      ) : null}
    </div>
  );
}

export const ModelText = memo(function ModelText({ text, onForward }: { text: string; onForward?: (text: string) => void }) {
  const [copied, copy] = useCopy();
  const [forwarded, setForwarded] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handOver = () => {
    if (!onForward) return;
    onForward(text);
    setForwarded(true);
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setForwarded(false), 2400);
  };
  return (
    <div className="model-text">
      <div className="prose">
        <MarkdownContent text={text} />
      </div>
      <div className="said-tools">
        {onForward && (
          <button
            type="button"
            className={`said-forward ${forwarded ? "done" : ""}`}
            title={forwarded ? "已转给主分身" : "把这段转给主分身：作为传音发过去，不打断它这一步"}
            onClick={handOver}
          >
            <Icon name="reply" size={14} />
            {forwarded ? "已转给主分身" : "转给主分身"}
          </button>
        )}
        <button type="button" className={`said-copy ${copied ? "done" : ""}`} title={copied ? "已复制" : "复制这段（Markdown）"} onClick={() => copy(text)}>
          <Icon name={copied ? "check-lg" : "copy"} size={14} />
          <span className="sr-only">{copied ? "已复制" : "复制这段"}</span>
        </button>
      </div>
    </div>
  );
});

export function Diff({ hunk }: { hunk: DiffHunk }) {
  return (
    <div className="diff">
      {hunk.lines.map((line, index) => (
        <div key={index} className={`diff-line ${line.kind === "+" ? "added" : line.kind === "-" ? "removed" : ""}`}>
          <span className="sign">{line.kind === "-" ? "−" : line.kind}</span>
          <span className="text">
            {/^\s*/.exec(line.text)?.[0]}
            <span className="words">{line.text.trimStart() || " "}</span>
          </span>
        </div>
      ))}
    </div>
  );
}

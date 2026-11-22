import { type ReactNode, isValidElement, memo, useEffect, useRef, useState } from "react";
import Markdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { api } from "../lib/api";
import type { DiffHunk } from "../lib/types";

const EXTERNAL = /^(https?:|mailto:)/i;

/**
 * A link in the model's text. Following it inside the window would replace the
 * app with the page, so web links open in the system browser and anything
 * else (relative paths, anchors, other schemes) stays plain text.
 */
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

/** Put `text` on the clipboard; `copied` is true for a moment after, for the button to say so. */
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

/** A code block from the model's text, with what it is written in and a button that copies it. */
function CodeBlock({ children }: { children?: ReactNode }) {
  const block = useRef<HTMLPreElement>(null);
  // The fence's own word for the language, which the highlighter leaves on the code as a class.
  const written = isValidElement<{ className?: string }>(children) ? /language-([\w+#.-]+)/.exec(children.props.className ?? "")?.[1] : undefined;
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

/** A table in its own box, so that a wide one scrolls sideways there instead of pushing the page out. */
function Table({ children }: { children?: ReactNode }) {
  return (
    <div className="table-wrap">
      <table>{children}</table>
    </div>
  );
}

export const markdownParts = { a: Link, pre: CodeBlock, table: Table };
// Tables, task lists, and struck-out words, as models write them.
export const markdownSyntax = [remarkGfm];
// Colors code by its language; a block in a language it does not know stays plain.
export const markdownPlugins = [rehypeHighlight];

/**
 * What the 分身 said, once it has said it: the words, and under them a mark that copies them as they were written,
 * in Markdown, so that pasted elsewhere they keep their lists, tables and code. Rendered again only when the words
 * change: parsing every block of a long conversation each time anything moves is what made the end of a turn stall.
 */
export const ModelText = memo(function ModelText({ text }: { text: string }) {
  const [copied, copy] = useCopy();
  return (
    <div className="model-text">
      <div className="prose">
        <Markdown components={markdownParts} remarkPlugins={markdownSyntax} rehypePlugins={markdownPlugins}>
          {text}
        </Markdown>
      </div>
      <div className="said-tools">
        <button type="button" className={`said-copy ${copied ? "done" : ""}`} title={copied ? "已复制" : "复制这段（Markdown）"} onClick={() => copy(text)}>
          {copied ? (
            <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
              <path d="m3 8.5 3.2 3L13 4.5" />
            </svg>
          ) : (
            <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
              <rect x="5.5" y="5.5" width="8" height="8" rx="1.2" />
              <path d="M10.5 5.5V3.7a1.2 1.2 0 0 0-1.2-1.2H3.7a1.2 1.2 0 0 0-1.2 1.2v5.6a1.2 1.2 0 0 0 1.2 1.2h1.8" />
            </svg>
          )}
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

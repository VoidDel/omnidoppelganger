import { useEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import { api } from "../lib/api";
import { documentLabel, fileSize } from "../lib/fold";
import { findInLines, highlightLines } from "../lib/highlight";
import { sameFileView } from "../lib/refresh";
import type { FileView } from "../lib/types";
import { markdownParts, markdownPlugins, markdownSyntax } from "./Markdown";

export interface CodeViewerProps {
  sessionId: string;
  path: string;
  targetLine?: number;
  jumpSeq?: number;
  refreshTick?: number;
  onQuoteLines: (quoteText: string) => void;
  onAttachPath: (relPath: string) => void;
  onError: (message: string) => void;
}

export function CodeViewer({
  sessionId,
  path,
  targetLine,
  jumpSeq = 0,
  refreshTick = 0,
  onQuoteLines,
  onAttachPath,
  onError,
}: CodeViewerProps) {
  const [view, setView] = useState<FileView | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<"code" | "preview">("code");
  const [zoomed, setZoomed] = useState<boolean>(false);
  const [copied, setCopied] = useState<"content" | "path" | "quote" | null>(null);

  // Line selection (1-based inclusive)
  const [selection, setSelection] = useState<{ anchor: number; end: number } | null>(null);

  // In-file search bar
  const [findOpen, setFindOpen] = useState<boolean>(false);
  const [findQuery, setFindQuery] = useState<string>("");
  const [findIndex, setFindIndex] = useState<number>(0);
  const findInputRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setSelection(null);
    setZoomed(false);
    const lower = path.toLowerCase();
    setMode(lower.endsWith(".md") || lower.endsWith(".markdown") ? "preview" : "code");
  }, [path]);

  /** The file the view on screen was read from; a re-read of the same file replaces it quietly, in place. */
  const shownKey = useRef<string | null>(null);
  /** The file wanted now, and the order reads were asked in: a read that lands after a later one is dropped. */
  const wanted = useRef("");
  const asked = useRef(0);
  const landed = useRef(0);
  useEffect(() => {
    const key = `${sessionId}\u0000${path}`;
    wanted.current = key;
    if (shownKey.current !== key) {
      setLoading(true);
      setLoadError(null);
    }
    const seq = ++asked.current;
    // Not dropped when a newer refresh is asked for meanwhile: under a stream of changes some read must land.
    const current = () => wanted.current === key && seq > landed.current;
    api.readWorkspaceFile(sessionId, path).then(
      (data) => {
        if (!current()) return;
        landed.current = seq;
        shownKey.current = key;
        setLoadError(null);
        // Unchanged, the view object stays: nothing is highlighted again, and the page keeps its scroll and selection.
        setView((shown) => (shown && sameFileView(shown, data) ? shown : data));
        setLoading(false);
      },
      (error) => {
        if (!current()) return;
        landed.current = seq;
        setLoadError(String(error));
        setLoading(false);
      },
    );
  }, [sessionId, path, refreshTick]);

  const rawLines = useMemo(() => (view?.kind === "text" ? view.content.split("\n") : []), [view]);
  // A file that got shorter under a selection keeps what is still there of it.
  useEffect(() => {
    if (!selection || rawLines.length === 0) return;
    const last = rawLines.length;
    if (selection.anchor > last && selection.end > last) setSelection(null);
    else if (selection.anchor > last || selection.end > last) setSelection({ anchor: Math.min(selection.anchor, last), end: Math.min(selection.end, last) });
  }, [rawLines.length, selection]);
  const htmlLines = useMemo(
    () => (view?.kind === "text" ? highlightLines(view.content, view.language) : []),
    [view],
  );

  const matches = useMemo(() => findInLines(rawLines, findQuery), [rawLines, findQuery]);
  const matchedLineSet = useMemo(() => new Set(matches.map((m) => m.line)), [matches]);
  const activeMatchLine = matches[findIndex]?.line ?? null;

  useEffect(() => {
    setFindIndex(0);
  }, [findQuery, path]);

  const scrollToLine = (line: number) => {
    const container = scrollRef.current;
    if (!container) return;
    const row = container.querySelector<HTMLElement>(`[data-line="${line}"]`);
    row?.scrollIntoView({ block: "center" });
  };

  useEffect(() => {
    if (!loading && view?.kind === "text" && targetLine && targetLine > 0) {
      setMode("code");
      setSelection({ anchor: targetLine, end: targetLine });
      requestAnimationFrame(() => scrollToLine(targetLine));
    }
  }, [loading, view?.kind, targetLine, jumpSeq]);

  useEffect(() => {
    if (activeMatchLine) {
      scrollToLine(activeMatchLine);
    }
  }, [activeMatchLine, findIndex]);

  const flashCopied = (which: "content" | "path" | "quote") => {
    setCopied(which);
    window.setTimeout(() => setCopied((cur) => (cur === which ? null : cur)), 1500);
  };

  const selectedRange = selection
    ? { start: Math.min(selection.anchor, selection.end), end: Math.max(selection.anchor, selection.end) }
    : null;

  const selectedSnippet = () => {
    if (!selectedRange || view?.kind !== "text") return "";
    const slice = rawLines.slice(selectedRange.start - 1, selectedRange.end).join("\n");
    const lang = view.language ?? "";
    const rangeLabel = selectedRange.start === selectedRange.end ? `第 ${selectedRange.start} 行` : `第 ${selectedRange.start}-${selectedRange.end} 行`;
    return `请看 \`${view.path}\`（${rangeLabel}）：\n\`\`\`${lang}\n${slice}\n\`\`\``;
  };

  const segments = path.split("/");
  const isMarkdown = view?.kind === "text" && /\.(md|mdx|markdown)$/i.test(view.name);
  const isSvg = view?.kind === "text" && !!view.svg_data;
  const canPreview = isMarkdown || isSvg;

  return (
    <div
      ref={rootRef}
      className="code-viewer"
      tabIndex={-1}
      onKeyDown={(e) => {
        if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f" && view?.kind === "text") {
          e.preventDefault();
          setMode("code");
          setFindOpen(true);
          requestAnimationFrame(() => findInputRef.current?.focus());
        } else if (e.key === "Escape" && findOpen) {
          setFindOpen(false);
        }
      }}
    >
      <div className="viewer-bar">
        <div className="viewer-crumbs" title={path}>
          {segments.map((part, idx) => (
            <span key={idx} className="crumb">
              {idx > 0 && <span className="crumb-sep">›</span>}
              <span className={idx === segments.length - 1 ? "crumb-leaf" : "crumb-dir"}>{part}</span>
            </span>
          ))}
        </div>
        <div className="viewer-acts">
          {view && (
            <span className="viewer-meta mono">
              {view.kind === "text" ? (
                <>
                  {view.language ? `${view.language} · ` : ""}
                  {view.lines} 行 · {view.eol} · {fileSize(view.size)}
                </>
              ) : (
                fileSize(view.size)
              )}
            </span>
          )}
          {canPreview && (
            <div className="seg-switch" role="group" aria-label="查看方式">
              <button type="button" className={mode === "preview" ? "on" : ""} onClick={() => setMode("preview")}>
                预览
              </button>
              <button type="button" className={mode === "code" ? "on" : ""} onClick={() => setMode("code")}>
                源码
              </button>
            </div>
          )}
          {view?.kind === "text" && (
            <button
              type="button"
              className={`icon-button small ${findOpen && mode === "code" ? "active" : ""}`}
              aria-pressed={findOpen && mode === "code"}
              title="在文件内查找 (Ctrl/Cmd+F)"
              onClick={() => {
                if (mode !== "code") {
                  setMode("code");
                  setFindOpen(true);
                  requestAnimationFrame(() => findInputRef.current?.focus());
                  return;
                }
                setFindOpen((v) => !v);
                if (!findOpen) requestAnimationFrame(() => findInputRef.current?.focus());
              }}
            >
              <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <circle cx="7" cy="7" r="4.3" />
                <path d="m10.2 10.2 3.3 3.3" />
              </svg>
              <span className="sr-only">查找</span>
            </button>
          )}
          <button
            type="button"
            className="icon-button small"
            title="把此文件附入右侧旁聊"
            onClick={() => onAttachPath(path)}
          >
            <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <path d="M13 7.4 8 12.4a3 3 0 0 1-4.3-4.2l5.4-5.4a2 2 0 0 1 2.9 2.8L6.7 10.9a1 1 0 0 1-1.5-1.4l4.6-4.6" />
            </svg>
            <span className="sr-only">附入对话</span>
          </button>
          <button
            type="button"
            className={`icon-button small ${copied === "path" ? "done" : ""}`}
            title={copied === "path" ? "已复制路径" : "复制相对路径"}
            onClick={() => {
              navigator.clipboard.writeText(path).then(() => flashCopied("path"), () => undefined);
            }}
          >
            {copied === "path" ? (
              <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
                <path d="m3 8.5 3.2 3L13 4.5" />
              </svg>
            ) : (
              <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <path d="M6.5 9.5 9.5 6.5M5.2 7.3 3.7 8.8a2.1 2.1 0 0 0 3 3l1.5-1.5M10.8 8.7l1.5-1.5a2.1 2.1 0 0 0-3-3L7.8 5.7" />
              </svg>
            )}
            <span className="sr-only">{copied === "path" ? "已复制路径" : "复制路径"}</span>
          </button>
          {view?.kind === "text" && (
            <button
              type="button"
              className={`icon-button small ${copied === "content" ? "done" : ""}`}
              title={copied === "content" ? "已复制全文" : "复制全文内容"}
              onClick={() => {
                navigator.clipboard.writeText(view.content).then(() => flashCopied("content"), () => undefined);
              }}
            >
              {copied === "content" ? (
                <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
                  <path d="m3 8.5 3.2 3L13 4.5" />
                </svg>
              ) : (
                <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                  <rect x="5.5" y="5.5" width="8" height="8" rx="1.2" />
                  <path d="M10.5 5.5V3.7a1.2 1.2 0 0 0-1.2-1.2H3.7a1.2 1.2 0 0 0-1.2 1.2v5.6a1.2 1.2 0 0 0 1.2 1.2h1.8" />
                </svg>
              )}
              <span className="sr-only">{copied === "content" ? "已复制全文" : "复制全文"}</span>
            </button>
          )}
          <button
            type="button"
            className="icon-button small"
            title="用系统默认程序打开"
            onClick={() => api.openWorkspaceEntry(sessionId, path, false).catch((err) => onError(String(err)))}
          >
            <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <path d="M9.5 2.5h4v4M13.5 2.5 7.5 8.5M12 9.5v4H2.5V4H6.5" />
            </svg>
            <span className="sr-only">外部打开</span>
          </button>
          <button
            type="button"
            className="icon-button small"
            title="在文件夹中显示"
            onClick={() => api.openWorkspaceEntry(sessionId, path, true).catch((err) => onError(String(err)))}
          >
            <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <path d="M1.8 4.2V12.6h12.4V5.8H7.6L6.2 4.2z" />
            </svg>
            <span className="sr-only">在文件夹中显示</span>
          </button>
        </div>
      </div>

      {findOpen && view?.kind === "text" && mode === "code" && (
        <div className="viewer-find">
          <input
            ref={findInputRef}
            className="input viewer-find-input"
            type="search"
            placeholder="在当前文件内查找…"
            value={findQuery}
            onChange={(e) => setFindQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && matches.length > 0) {
                e.preventDefault();
                setFindIndex((cur) => (e.shiftKey ? (cur - 1 + matches.length) % matches.length : (cur + 1) % matches.length));
              }
              if (e.key === "Escape") {
                e.preventDefault();
                setFindOpen(false);
              }
            }}
          />
          {findQuery && (
            <span className={`viewer-find-count mono ${matches.length === 0 ? "none" : ""}`}>
              {matches.length > 0 ? `${findIndex + 1} / ${matches.length}` : "无匹配"}
            </span>
          )}
          <div className="viewer-find-acts">
            <button
              type="button"
              className="viewer-find-btn"
              disabled={matches.length === 0}
              title="上一处 (Shift+Enter)"
              onClick={() => setFindIndex((cur) => (cur - 1 + matches.length) % matches.length)}
            >
              <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M8 12.5v-9M4.2 7.3 8 3.5l3.8 3.8" />
              </svg>
              <span className="sr-only">上一处</span>
            </button>
            <button
              type="button"
              className="viewer-find-btn"
              disabled={matches.length === 0}
              title="下一处 (Enter)"
              onClick={() => setFindIndex((cur) => (cur + 1) % matches.length)}
            >
              <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M8 3.5v9M4.2 8.7 8 12.5l3.8-3.8" />
              </svg>
              <span className="sr-only">下一处</span>
            </button>
            <button type="button" className="viewer-find-btn" title="关闭查找 (Esc)" onClick={() => setFindOpen(false)}>
              <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" />
              </svg>
              <span className="sr-only">关闭查找</span>
            </button>
          </div>
        </div>
      )}

      {selectedRange && view?.kind === "text" && mode === "code" && (
        <div className="viewer-selection-bar">
          <span>
            已选 <strong>{selectedRange.start === selectedRange.end ? `第 ${selectedRange.start} 行` : `第 ${selectedRange.start}–${selectedRange.end} 行`}</strong>
            <span className="muted">（按住 Shift 点行号可选连续多行）</span>
          </span>
          <span className="viewer-selection-acts">
            <button
              type="button"
              className="button primary small"
              onClick={() => onQuoteLines(selectedSnippet())}
            >
              引用到对话
            </button>
            <button
              type="button"
              className="button outline small"
              onClick={() => {
                const raw = rawLines.slice(selectedRange.start - 1, selectedRange.end).join("\n");
                navigator.clipboard.writeText(raw).then(() => flashCopied("quote"), () => undefined);
              }}
            >
              {copied === "quote" ? "已复制" : "复制代码"}
            </button>
            <button type="button" className="button text small" onClick={() => setSelection(null)}>
              取消
            </button>
          </span>
        </div>
      )}

      <div ref={scrollRef} className="viewer-body">
        {loading ? (
          <div className="viewer-placeholder muted">正在读取文件…</div>
        ) : loadError ? (
          <div className="viewer-placeholder failed">{loadError}</div>
        ) : !view ? null : view.kind === "image" ? (
          <div className={`viewer-image-stage ${zoomed ? "zoomed" : ""}`}>
            <img
              src={`data:${view.media_type};base64,${view.data}`}
              alt={view.name}
              onClick={() => setZoomed((v) => !v)}
              title={zoomed ? "点击适应窗口" : "点击查看原始尺寸"}
            />
          </div>
        ) : view.kind === "document" ? (
          <div className="viewer-doc">
            <div className="viewer-doc-note">
              <span>已提取文档正文（{documentLabel(view.doc_kind, view.parts ?? undefined)}）。排版与插图请用系统默认程序查看。</span>
              <button
                type="button"
                className="button outline small"
                onClick={() => api.openWorkspaceEntry(sessionId, path, false).catch((err) => onError(String(err)))}
              >
                用系统程序打开
              </button>
            </div>
            <pre className="viewer-doc-text">{view.content}</pre>
          </div>
        ) : view.kind === "binary" ? (
          <div className="viewer-placeholder">
            <p>
              {view.reason === "too_large"
                ? `「${view.name}」体积为 ${fileSize(view.size)}，超出内置查看器上限，未直接加载进内存。`
                : `「${view.name}」（${fileSize(view.size)}）是二进制文件，无法作为代码文本显示。`}
            </p>
            <div className="actions">
              <button
                type="button"
                className="button outline"
                onClick={() => api.openWorkspaceEntry(sessionId, path, false).catch((err) => onError(String(err)))}
              >
                用系统默认程序打开
              </button>
              <button
                type="button"
                className="button text"
                onClick={() => api.openWorkspaceEntry(sessionId, path, true).catch((err) => onError(String(err)))}
              >
                在文件夹中显示
              </button>
            </div>
          </div>
        ) : mode === "preview" && isMarkdown ? (
          <div className="viewer-markdown prose">
            <Markdown components={markdownParts} remarkPlugins={markdownSyntax} rehypePlugins={markdownPlugins}>
              {view.content}
            </Markdown>
          </div>
        ) : mode === "preview" && isSvg ? (
          <div className={`viewer-image-stage ${zoomed ? "zoomed" : ""}`}>
            <img
              src={`data:image/svg+xml;base64,${view.svg_data}`}
              alt={view.name}
              onClick={() => setZoomed((v) => !v)}
            />
          </div>
        ) : view.content.length === 0 ? (
          <div className="viewer-placeholder muted">（空文件）</div>
        ) : (
          <div className="code-lines" role="table" aria-label={`${view.name} 源码`}>
            {htmlLines.map((lineHtml, idx) => {
              const lineNo = idx + 1;
              const isSelected = !!selectedRange && lineNo >= selectedRange.start && lineNo <= selectedRange.end;
              const isMatched = matchedLineSet.has(lineNo);
              const isActiveMatch = activeMatchLine === lineNo;
              return (
                <div
                  key={lineNo}
                  data-line={lineNo}
                  role="row"
                  className={[
                    "code-row",
                    isSelected && "selected",
                    isMatched && "matched",
                    isActiveMatch && "active-match",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                >
                  <button
                    type="button"
                    className="line-no"
                    data-line={lineNo}
                    aria-label={`第 ${lineNo} 行`}
                    title="点选此行；按住 Shift 点另一行可选区间"
                    onClick={(e) => {
                      if (e.shiftKey && selection) {
                        setSelection({ anchor: selection.anchor, end: lineNo });
                      } else if (selection && selection.anchor === lineNo && selection.end === lineNo) {
                        setSelection(null);
                      } else {
                        setSelection({ anchor: lineNo, end: lineNo });
                      }
                    }}
                  />
                  <div
                    role="cell"
                    className="line-code"
                    dangerouslySetInnerHTML={{ __html: lineHtml || "<br>" }}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

import { useEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import { api } from "../lib/api";
import { documentLabel, fileSize } from "../lib/fold";
import { findInLines, highlightLines } from "../lib/highlight";
import { sameFileView } from "../lib/refresh";
import type { FileView } from "../lib/types";
import type { ComposerQuoteInput } from "./Composer";
import { Icon } from "./Icon";
import { markdownParts, markdownPlugins, markdownSyntax } from "./Markdown";
import { normalizeMath } from "../lib/math";

export interface CodeViewerProps {
  sessionId: string;
  path: string;
  targetLine?: number;
  jumpSeq?: number;
  refreshTick?: number;
  onQuoteToMain: (quote: ComposerQuoteInput) => void;
  onQuoteToAside?: (quote: ComposerQuoteInput) => void;
  onAttachPath: (relPath: string) => void;
  onError: (message: string) => void;
}

function lineNumberOf(node: Node | null): number | null {
  let cur: Node | null = node;
  while (cur) {
    if (cur instanceof HTMLElement && cur.dataset.line) {
      const n = Number.parseInt(cur.dataset.line, 10);
      if (Number.isFinite(n) && n > 0) return n;
    }
    cur = cur.parentNode;
  }
  return null;
}

export function CodeViewer({
  sessionId,
  path,
  targetLine,
  jumpSeq = 0,
  refreshTick = 0,
  onQuoteToMain,
  onQuoteToAside,
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
  // Drag-selection floating quote popover
  const [dragQuote, setDragQuote] = useState<{
    text: string;
    startLine: number | null;
    endLine: number | null;
    top: number;
    left: number;
    below: boolean;
  } | null>(null);
  const quoteBarRef = useRef<HTMLDivElement>(null);

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

  const formatRangeQuote = (start: number, end: number): ComposerQuoteInput | null => {
    if (view?.kind !== "text") return null;
    const slice = rawLines.slice(start - 1, end).join("\n");
    const lang = view.language ?? "";
    const rangeLabel = start === end ? `第 ${start} 行` : `第 ${start}-${end} 行`;
    const shortRange = start === end ? `L${start}` : `L${start}-${end}`;
    return {
      label: `${view.name}:${shortRange}`,
      meta: `${end - start + 1} 行`,
      content: `请看 \`${view.path}\`（${rangeLabel}）：\n\`\`\`${lang}\n${slice}\n\`\`\``,
    };
  };

  const selectedQuote = (): ComposerQuoteInput | null => {
    if (!selectedRange) return null;
    return formatRangeQuote(selectedRange.start, selectedRange.end);
  };

  const formatDragQuote = (): ComposerQuoteInput | null => {
    if (!dragQuote || !view) return null;
    if (view.kind === "text" && dragQuote.startLine !== null && dragQuote.endLine !== null) {
      return formatRangeQuote(dragQuote.startLine, dragQuote.endLine);
    }
    const trimmed = dragQuote.text.trim();
    const lines = trimmed.split(/\r?\n/);
    const quoted = lines.map((line) => `> ${line}`).join("\n");
    return {
      label: view.name,
      meta: lines.length > 1 ? `${lines.length} 行` : `${trimmed.length} 字`,
      content: `请看 \`${view.path}\`：\n${quoted}`,
    };
  };

  const syncDragQuote = () => {
    const container = scrollRef.current;
    const sel = window.getSelection();
    if (!container || !sel || sel.isCollapsed || sel.rangeCount === 0 || !view || view.kind === "image" || view.kind === "binary") {
      setDragQuote(null);
      return;
    }
    const text = sel.toString().trim();
    if (!text) {
      setDragQuote(null);
      return;
    }
    const range = sel.getRangeAt(0);
    if (!container.contains(range.commonAncestorContainer)) {
      setDragQuote(null);
      return;
    }
    const rect = range.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) {
      setDragQuote(null);
      return;
    }
    let startLine = lineNumberOf(range.startContainer);
    let endLine = lineNumberOf(range.endContainer);
    if (startLine !== null && endLine !== null) {
      if (startLine > endLine) [startLine, endLine] = [endLine, startLine];
      // If selection ends at offset 0 of the next line's cell, don't include that trailing untouched line.
      if (endLine > startLine && range.endOffset === 0) {
        const endRow = range.endContainer instanceof HTMLElement ? range.endContainer.closest("[data-line]") : range.endContainer.parentElement?.closest("[data-line]");
        if (endRow && range.endContainer === endRow.querySelector(".line-code")?.firstChild) {
          endLine -= 1;
        }
      }
    } else if (startLine !== null || endLine !== null) {
      const only = startLine ?? endLine;
      startLine = only;
      endLine = only;
    }
    const below = rect.top < 76;
    const top = below ? rect.bottom + 8 : rect.top - 8;
    const left = Math.min(Math.max(rect.left + rect.width / 2, 160), window.innerWidth - 160);
    setDragQuote({ text, startLine, endLine, top, left, below });
  };

  useEffect(() => {
    if (!dragQuote) return;
    const onSelectionChange = () => {
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed || !sel.toString().trim()) {
        setDragQuote(null);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setDragQuote(null);
    };
    const onPointerDown = (event: PointerEvent) => {
      if (quoteBarRef.current?.contains(event.target as Node)) return;
      setDragQuote(null);
    };
    document.addEventListener("selectionchange", onSelectionChange);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("selectionchange", onSelectionChange);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("pointerdown", onPointerDown);
    };
  }, [dragQuote]);

  useEffect(() => {
    setDragQuote(null);
  }, [path, mode]);

  const sendDragQuote = (target: "main" | "aside") => {
    const quote = formatDragQuote();
    if (!quote) return;
    window.getSelection()?.removeAllRanges();
    setDragQuote(null);
    if (target === "main") onQuoteToMain(quote);
    else onQuoteToAside?.(quote);
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
              <Icon name="search" size={14} />
              <span className="sr-only">查找</span>
            </button>
          )}
          <button
            type="button"
            className="icon-button small"
            title="把此文件附入右侧旁聊"
            onClick={() => onAttachPath(path)}
          >
            <Icon name="paperclip" size={15} />
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
            <Icon name={copied === "path" ? "check-lg" : "link-45deg"} size={15} />
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
              <Icon name={copied === "content" ? "check-lg" : "copy"} size={14} />
              <span className="sr-only">{copied === "content" ? "已复制全文" : "复制全文"}</span>
            </button>
          )}
          <button
            type="button"
            className="icon-button small"
            title="用系统默认程序打开"
            onClick={() => api.openWorkspaceEntry(sessionId, path, false).catch((err) => onError(String(err)))}
          >
            <Icon name="box-arrow-up-right" size={14} />
            <span className="sr-only">外部打开</span>
          </button>
          <button
            type="button"
            className="icon-button small"
            title="在文件夹中显示"
            onClick={() => api.openWorkspaceEntry(sessionId, path, true).catch((err) => onError(String(err)))}
          >
            <Icon name="folder2-open" size={14} />
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
              <Icon name="arrow-up" size={13} />
              <span className="sr-only">上一处</span>
            </button>
            <button
              type="button"
              className="viewer-find-btn"
              disabled={matches.length === 0}
              title="下一处 (Enter)"
              onClick={() => setFindIndex((cur) => (cur + 1) % matches.length)}
            >
              <Icon name="arrow-down" size={13} />
              <span className="sr-only">下一处</span>
            </button>
            <button type="button" className="viewer-find-btn" title="关闭查找 (Esc)" onClick={() => setFindOpen(false)}>
              <Icon name="x-lg" size={12} />
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
              onClick={() => {
                const quote = selectedQuote();
                if (quote) onQuoteToMain(quote);
              }}
            >
              引用到分身
            </button>
            {onQuoteToAside && (
              <button
                type="button"
                className="button outline small"
                onClick={() => {
                  const quote = selectedQuote();
                  if (quote) onQuoteToAside(quote);
                }}
              >
                旁聊提问
              </button>
            )}
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

      <div
        ref={scrollRef}
        className="viewer-body"
        onMouseUp={() => requestAnimationFrame(syncDragQuote)}
        onKeyUp={() => requestAnimationFrame(syncDragQuote)}
        onScroll={() => {
          if (dragQuote) setDragQuote(null);
        }}
      >
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
              {normalizeMath(view.content)}
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
      {dragQuote && view && (
        <div
          ref={quoteBarRef}
          className={`selection-quote-bar ${dragQuote.below ? "below" : "above"}`}
          style={{ top: `${dragQuote.top}px`, left: `${dragQuote.left}px` }}
          role="toolbar"
          aria-label="引用选中代码"
          onMouseDown={(event) => event.preventDefault()}
        >
          <span className="selection-quote-loc mono" title={view.path}>
            {view.name}
            {dragQuote.startLine !== null && dragQuote.endLine !== null
              ? dragQuote.startLine === dragQuote.endLine
                ? `:L${dragQuote.startLine}`
                : `:L${dragQuote.startLine}-${dragQuote.endLine}`
              : ""}
          </span>
          <span className="selection-quote-sep" aria-hidden="true" />
          <button
            type="button"
            className="selection-quote-btn"
            title="将文件路径与选中行填入分身输入框"
            onClick={() => sendDragQuote("main")}
          >
            <Icon name="quote" size={13} />
            <span>引用到分身</span>
          </button>
          {onQuoteToAside && (
            <>
              <span className="selection-quote-sep" aria-hidden="true" />
              <button
                type="button"
                className="selection-quote-btn"
                title="打开侧边栏旁聊并引用选中行提问"
                onClick={() => sendDragQuote("aside")}
              >
                <Icon name="chat-left-quote" size={13} />
                <span>旁聊提问</span>
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}

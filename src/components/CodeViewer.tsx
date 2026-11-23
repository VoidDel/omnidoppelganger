import { useEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import { api } from "../lib/api";
import { documentLabel, fileSize } from "../lib/fold";
import { findInLines, highlightLines } from "../lib/highlight";
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

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setLoadError(null);
    api.readWorkspaceFile(sessionId, path).then(
      (data) => {
        if (!alive) return;
        setView(data);
        setLoading(false);
      },
      (error) => {
        if (!alive) return;
        setLoadError(String(error));
        setLoading(false);
      },
    );
    return () => {
      alive = false;
    };
  }, [sessionId, path, refreshTick]);

  const rawLines = useMemo(() => (view?.kind === "text" ? view.content.split("\n") : []), [view]);
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
          {view?.kind === "text" && mode === "code" && (
            <button
              type="button"
              className={`button text small ${findOpen ? "active" : ""}`}
              title="在文件内查找 (Ctrl/Cmd+F)"
              onClick={() => {
                setFindOpen((v) => !v);
                if (!findOpen) requestAnimationFrame(() => findInputRef.current?.focus());
              }}
            >
              查找
            </button>
          )}
          <button
            type="button"
            className="button text small"
            title="把此文件附入右侧对话"
            onClick={() => onAttachPath(path)}
          >
            附入对话
          </button>
          <button
            type="button"
            className="button text small"
            title="复制相对路径"
            onClick={() => {
              navigator.clipboard.writeText(path).then(() => flashCopied("path"), () => undefined);
            }}
          >
            {copied === "path" ? "已复制路径" : "复制路径"}
          </button>
          {view?.kind === "text" && (
            <button
              type="button"
              className="button text small"
              title="复制全文内容"
              onClick={() => {
                navigator.clipboard.writeText(view.content).then(() => flashCopied("content"), () => undefined);
              }}
            >
              {copied === "content" ? "已复制" : "复制全文"}
            </button>
          )}
          <button
            type="button"
            className="icon-button small"
            title="用系统默认程序打开"
            onClick={() => api.openWorkspaceEntry(sessionId, path, false).catch((err) => onError(String(err)))}
          >
            <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
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
            <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
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
          <span className="viewer-find-count mono">
            {findQuery ? (matches.length > 0 ? `${findIndex + 1} / ${matches.length} 行` : "无匹配") : ""}
          </span>
          <button
            type="button"
            className="icon-button small"
            disabled={matches.length === 0}
            title="上一处 (Shift+Enter)"
            onClick={() => setFindIndex((cur) => (cur - 1 + matches.length) % matches.length)}
          >
            ↑
          </button>
          <button
            type="button"
            className="icon-button small"
            disabled={matches.length === 0}
            title="下一处 (Enter)"
            onClick={() => setFindIndex((cur) => (cur + 1) % matches.length)}
          >
            ↓
          </button>
          <button type="button" className="icon-button small" title="关闭查找" onClick={() => setFindOpen(false)}>
            ×
          </button>
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
                  >
                    {lineNo}
                  </button>
                  <span
                    role="cell"
                    className="line-code"
                    dangerouslySetInnerHTML={{ __html: lineHtml || " " }}
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

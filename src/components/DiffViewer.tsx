import "../styles/workbench.css";
import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { api } from "../lib/api";
import { escapeHtml, highlightLines } from "../lib/highlight";
import { sameDiffView } from "../lib/refresh";
import type { DiffStage, GitDiffView } from "../lib/types";
import type { QuotePayload, StructuredQuote } from "./Composer";
import { Icon } from "./Icon";

export const DIFF_INITIAL_LINES = 800;
export const DIFF_CHUNK_SIZE = 400;
export const DIFF_LOAD_STEP = 1000;

export interface DiffViewerProps {
  sessionId: string;
  path: string;
  stage: DiffStage;
  commit?: string;
  repo?: string | null;
  refreshTick?: number;
  onOpenSource: (relPath: string, line?: number) => void;
  onQuoteToMain: (quote: QuotePayload) => void;
  onQuoteToAside?: (quote: QuotePayload) => void;
}

interface RenderedDiffLine {
  kind: "+" | "-" | " ";
  old_line?: number | null;
  new_line?: number | null;
  text: string;
  html: string;
}

function chunkArray<T>(items: T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < items.length; i += size) {
    out.push(items.slice(i, i + size));
  }
  return out;
}

export function DiffViewer({
  sessionId,
  path,
  stage,
  commit,
  repo,
  refreshTick = 0,
  onOpenSource,
  onQuoteToMain,
  onQuoteToAside,
}: DiffViewerProps) {
  const { t } = useI18n();
  const [diff, setDiff] = useState<GitDiffView | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [visibleLineLimit, setVisibleLineLimit] = useState<number>(DIFF_INITIAL_LINES);

  useEffect(() => {
    setVisibleLineLimit(DIFF_INITIAL_LINES);
  }, [sessionId, path, stage, commit, repo]);

  /** The diff on screen, the one wanted now, and the order they were asked in, as in the code viewer. */
  const shownKey = useRef<string | null>(null);
  const wanted = useRef("");
  const asked = useRef(0);
  const landed = useRef(0);
  useEffect(() => {
    const repoArg = repo || undefined;
    const repoRelPath = repoArg && path.startsWith(`${repoArg}/`) ? path.slice(repoArg.length + 1) : path;
    const key = `${sessionId}\u0000${repoArg ?? ""}\u0000${path}\u0000${stage}\u0000${commit ?? ""}`;
    wanted.current = key;
    const quiet = shownKey.current === key;
    // A commit's diff never changes: shown once, it is not asked for again.
    if (quiet && stage === "commit") return;
    if (!quiet) {
      setLoading(true);
      setError(null);
    }
    const seq = ++asked.current;
    const current = () => wanted.current === key && seq > landed.current;
    api.gitDiff(sessionId, repoRelPath, stage, commit, repoArg).then(
      (data) => {
        if (!current()) return;
        landed.current = seq;
        shownKey.current = key;
        setError(null);
        // Unchanged, the diff object stays: nothing is highlighted again and the page does not move.
        setDiff((shown) => (shown && sameDiffView(shown, data) ? shown : data));
        setLoading(false);
      },
      (err) => {
        if (!current()) return;
        landed.current = seq;
        setError(String(err));
        setLoading(false);
      },
    );
  }, [sessionId, path, stage, commit, repo, refreshTick]);

  const totalLines = useMemo(
    () => (diff ? diff.hunks.reduce((sum, hunk) => sum + hunk.lines.length, 0) : 0),
    [diff],
  );

  // Highlight only visible lines up to `visibleLineLimit` and partition into 400-line `content-visibility: auto` chunks.
  const visibleHunks = useMemo(() => {
    if (!diff) return [];
    let remaining = visibleLineLimit;
    const out: { header: string; chunks: RenderedDiffLine[][] }[] = [];
    for (const hunk of diff.hunks) {
      if (remaining <= 0) break;
      const slice = hunk.lines.slice(0, remaining);
      remaining -= slice.length;
      const joined = slice.map((l) => l.text).join("\n");
      const htmls = diff.language ? highlightLines(joined, diff.language) : slice.map((l) => escapeHtml(l.text));
      const renderedLines: RenderedDiffLine[] = slice.map((line, i) => ({
        ...line,
        html: htmls[i] ?? escapeHtml(line.text),
      }));
      out.push({
        header: hunk.header,
        chunks: chunkArray(renderedLines, DIFF_CHUNK_SIZE),
      });
    }
    return out;
  }, [diff, visibleLineLimit]);

  const stageLabel =
    stage === "staged"
      ? t("quote.diffStaged")
      : stage === "commit"
        ? t("quote.diffCommit", { commit: commit?.slice(0, 7) ?? "" })
        : t("quote.diffUnstaged");
  const segments = path.split("/");
  const firstLine = diff?.hunks[0]?.new_start || 1;

  const formatDiffQuote = (): StructuredQuote | null => {
    if (!diff || diff.hunks.length === 0) return null;
    const body = diff.hunks
      .map((h) => `${h.header}\n${h.lines.map((l) => `${l.kind}${l.text}`).join("\n")}`)
      .join("\n")
      .slice(0, 6000);
    return {
      kind: "diff",
      path: diff.path,
      stage,
      commit,
      added: diff.added,
      removed: diff.removed,
      content: body,
    };
  };

  const shownCount = Math.min(visibleLineLimit, totalLines);

  return (
    <div className="code-viewer diff-viewer">
      <div className="viewer-bar">
        <div className="viewer-crumbs" title={path}>
          {segments.map((part, idx) => (
            <span key={idx} className="crumb">
              {idx > 0 && <span className="crumb-sep">›</span>}
              <span className={idx === segments.length - 1 ? "crumb-leaf" : "crumb-dir"}>{part}</span>
            </span>
          ))}
          <span className="wb-tab-tag">{stageLabel}</span>
        </div>
        <div className="viewer-acts">
          {diff && !diff.binary && (
            <span className="viewer-meta mono">
              <span className="diff-stat-add">+{diff.added}</span>{" "}
              <span className="diff-stat-del">−{diff.removed}</span>
            </span>
          )}
          {diff && diff.hunks.length > 0 && (
            <>
              <button
                type="button"
                className="icon-button small"
                title="引入到对话：将此改动引用到主分身输入框"
                aria-label="引入到对话"
                onClick={() => {
                  const quote = formatDiffQuote();
                  if (quote) onQuoteToMain(quote);
                }}
              >
                <Icon name="quote" size={15} />
              </button>
              {onQuoteToAside && (
                <button
                  type="button"
                  className="icon-button small"
                  title="引入到旁聊对话：将此改动引用到侧边栏旁聊输入框"
                  aria-label="引入到旁聊对话"
                  onClick={() => {
                    const quote = formatDiffQuote();
                    if (quote) onQuoteToAside(quote);
                  }}
                >
                  <Icon name="chat-left-quote" size={14} />
                </button>
              )}
            </>
          )}
          <button
            type="button"
            className="icon-button small"
            title="打开当前文件源码"
            aria-label="打开源文件"
            onClick={() => onOpenSource(path, firstLine)}
          >
            <Icon name="code-slash" size={15} />
          </button>
        </div>
      </div>

      <div className="viewer-body">
        {loading ? (
          <div className="viewer-placeholder muted">正在计算差异…</div>
        ) : error ? (
          <div className="viewer-placeholder failed">{error}</div>
        ) : !diff ? null : diff.binary ? (
          <div className="viewer-placeholder muted">二进制文件发生变动，无法按文本行展示差异。</div>
        ) : visibleHunks.length === 0 ? (
          <div className="viewer-placeholder muted">这一处当前没有文本差异。</div>
        ) : (
          <div className="rich-diff-list">
            {visibleHunks.map((hunk, hIdx) => (
              <div key={hIdx} className="rich-hunk">
                <div className="rich-hunk-header mono">{hunk.header}</div>
                <div className="rich-hunk-lines">
                  {hunk.chunks.map((chunk, cIdx) => (
                    <div key={cIdx} className="rich-hunk-chunk">
                      {chunk.map((line, lIdx) => (
                        <div
                          key={lIdx}
                          className={`rich-diff-row ${line.kind === "+" ? "added" : line.kind === "-" ? "removed" : "context"}`}
                        >
                          <span className="diff-no old" data-line={line.old_line ?? ""} aria-hidden="true" />
                          <button
                            type="button"
                            className="diff-no new"
                            data-line={line.new_line ?? ""}
                            aria-label={line.new_line ? `跳到源文件第 ${line.new_line} 行` : undefined}
                            disabled={!line.new_line}
                            title={line.new_line ? `跳到源文件第 ${line.new_line} 行` : undefined}
                            onClick={() => line.new_line && onOpenSource(path, line.new_line)}
                          />
                          <span className="diff-sign" data-sign={line.kind === "-" ? "−" : line.kind} aria-hidden="true" />
                          <div
                            className="line-code"
                            dangerouslySetInnerHTML={{ __html: line.html || "<br>" }}
                          />
                        </div>
                      ))}
                    </div>
                  ))}
                </div>
              </div>
            ))}
            {totalLines > visibleLineLimit && (
              <div className="diff-more-bar">
                <button
                  type="button"
                  className="button outline small"
                  onClick={() => setVisibleLineLimit((cur) => Math.min(totalLines, cur + DIFF_LOAD_STEP))}
                >
                  {t("workbench.loadMoreLines", { shown: shownCount, total: totalLines })}
                </button>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

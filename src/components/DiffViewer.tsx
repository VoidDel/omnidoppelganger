import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { escapeHtml, highlightLines } from "../lib/highlight";
import type { DiffStage, GitDiffView } from "../lib/types";

export interface DiffViewerProps {
  sessionId: string;
  path: string;
  stage: DiffStage;
  commit?: string;
  refreshTick?: number;
  onOpenSource: (relPath: string, line?: number) => void;
  onQuoteDiff: (quoteText: string) => void;
}

export function DiffViewer({
  sessionId,
  path,
  stage,
  commit,
  refreshTick = 0,
  onOpenSource,
  onQuoteDiff,
}: DiffViewerProps) {
  const [diff, setDiff] = useState<GitDiffView | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    api.gitDiff(sessionId, path, stage, commit).then(
      (data) => {
        if (!alive) return;
        setDiff(data);
        setLoading(false);
      },
      (err) => {
        if (!alive) return;
        setError(String(err));
        setLoading(false);
      },
    );
    return () => {
      alive = false;
    };
  }, [sessionId, path, stage, commit, refreshTick]);

  // Pre-highlight lines per hunk so syntax tokens colorize inside +/- rows cleanly.
  const highlightedHunks = useMemo(() => {
    if (!diff) return [];
    return diff.hunks.map((hunk) => {
      const joined = hunk.lines.map((l) => l.text).join("\n");
      const htmls = diff.language ? highlightLines(joined, diff.language) : hunk.lines.map((l) => escapeHtml(l.text));
      return {
        ...hunk,
        renderedLines: hunk.lines.map((line, i) => ({
          ...line,
          html: htmls[i] ?? escapeHtml(line.text),
        })),
      };
    });
  }, [diff]);

  const stageLabel =
    stage === "staged" ? "已暂存改动" : stage === "commit" ? `提交 ${commit?.slice(0, 7) ?? ""}` : "工作区改动";
  const segments = path.split("/");
  const firstLine = diff?.hunks[0]?.new_start || 1;

  const formatDiffQuote = () => {
    if (!diff || diff.hunks.length === 0) return "";
    const body = diff.hunks
      .map((h) => `${h.header}\n${h.lines.map((l) => `${l.kind}${l.text}`).join("\n")}`)
      .join("\n")
      .slice(0, 6000);
    return `请看 \`${diff.path}\` 的 ${stageLabel}：\n\`\`\`diff\n${body}\n\`\`\``;
  };

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
            <button type="button" className="button text small" onClick={() => onQuoteDiff(formatDiffQuote())}>
              引用改动到对话
            </button>
          )}
          <button
            type="button"
            className="button outline small"
            title="打开当前文件源码"
            onClick={() => onOpenSource(path, firstLine)}
          >
            打开源文件
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
        ) : highlightedHunks.length === 0 ? (
          <div className="viewer-placeholder muted">这一处当前没有文本差异。</div>
        ) : (
          <div className="rich-diff-list">
            {highlightedHunks.map((hunk, hIdx) => (
              <div key={hIdx} className="rich-hunk">
                <div className="rich-hunk-header mono">{hunk.header}</div>
                <div className="rich-hunk-lines">
                  {hunk.renderedLines.map((line, lIdx) => (
                    <div
                      key={lIdx}
                      className={`rich-diff-row ${line.kind === "+" ? "added" : line.kind === "-" ? "removed" : "context"}`}
                    >
                      <span className="diff-no old">{line.old_line ?? ""}</span>
                      <button
                        type="button"
                        className="diff-no new"
                        disabled={!line.new_line}
                        title={line.new_line ? `跳到源文件第 ${line.new_line} 行` : undefined}
                        onClick={() => line.new_line && onOpenSource(path, line.new_line)}
                      >
                        {line.new_line ?? ""}
                      </button>
                      <span className="diff-sign">{line.kind === "-" ? "−" : line.kind}</span>
                      <span
                        className="line-code"
                        dangerouslySetInnerHTML={{ __html: line.html || " " }}
                      />
                    </div>
                  ))}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

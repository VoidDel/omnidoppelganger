import { useEffect, useMemo, useRef, useState } from "react";
import { fileSize } from "../lib/fold";
import type { PasteKind } from "../lib/paste";
import { ModelText } from "./Markdown";
import { Icon } from "./Icon";

export interface PastedPreviewItem {
  name: string;
  bytes: number;
  content: string;
  kind: PasteKind;
  label: string;
  lines: number;
}

interface PasteViewerProps {
  item: PastedPreviewItem;
  onClose: () => void;
  /** Put the pasted text back into the composer textarea and remove the attachment chip. */
  onExpandToInput?: () => void;
}

const SOURCE_PREVIEW_LIMIT = 120_000;

/**
 * Modal preview for a long pasted text block or detected `.svg` / `.html` snippet attached in the composer.
 * SVG renders safely via `<img>` (`data:image/svg+xml`) and HTML via `<iframe sandbox="">` (zero scripts).
 */
export function PasteViewer({ item, onClose, onExpandToInput }: PasteViewerProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const canVisualPreview = item.kind === "svg" || item.kind === "html" || item.kind === "markdown";
  const [tab, setTab] = useState<"preview" | "source">(item.kind === "svg" || item.kind === "markdown" ? "preview" : "source");
  const [showAll, setShowAll] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    dialog.current?.showModal();
    dialog.current?.focus();
  }, []);

  const svgDataUrl = useMemo(() => {
    if (item.kind !== "svg") return null;
    return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(item.content)}`;
  }, [item.kind, item.content]);

  const truncated = !showAll && item.content.length > SOURCE_PREVIEW_LIMIT;
  const shownSource = truncated ? item.content.slice(0, SOURCE_PREVIEW_LIMIT) : item.content;

  const copyContent = () => {
    void navigator.clipboard?.writeText(item.content).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    });
  };

  return (
    <dialog
      ref={dialog}
      className="paste-viewer"
      tabIndex={-1}
      aria-label={`预览 ${item.name}`}
      onClose={onClose}
      onClick={(event) => event.target === event.currentTarget && dialog.current?.close()}
    >
      <div className="paste-viewer-card">
        <div className="paste-viewer-head">
          <div className="paste-viewer-title">
            <span className="paste-kind-badge">{item.label}</span>
            <span className="mono paste-viewer-name">{item.name}</span>
            <span className="paste-viewer-meta">
              {fileSize(item.bytes)} · {item.lines.toLocaleString()} 行
            </span>
          </div>
          <div className="paste-viewer-actions">
            {canVisualPreview && (
              <div className="paste-viewer-tabs" role="tablist">
                <button
                  type="button"
                  role="tab"
                  aria-selected={tab === "preview"}
                  className={`paste-tab ${tab === "preview" ? "active" : ""}`}
                  onClick={() => setTab("preview")}
                >
                  预览
                </button>
                <button
                  type="button"
                  role="tab"
                  aria-selected={tab === "source"}
                  className={`paste-tab ${tab === "source" ? "active" : ""}`}
                  onClick={() => setTab("source")}
                >
                  源码
                </button>
              </div>
            )}
            <button type="button" className="button outline small" onClick={copyContent}>
              {copied ? "已复制" : "复制"}
            </button>
            {onExpandToInput && (
              <button
                type="button"
                className="button outline small"
                title="撤回折叠块，将这段文字直接放回输入框编辑"
                onClick={() => {
                  onExpandToInput();
                  dialog.current?.close();
                }}
              >
                展开到输入框
              </button>
            )}
            <button type="button" className="icon-button" aria-label="关上" title="关上（Esc）" onClick={() => dialog.current?.close()}>
              <Icon name="x-lg" size={14} />
            </button>
          </div>
        </div>
        <div className="paste-viewer-body">
          {tab === "preview" && item.kind === "svg" && svgDataUrl ? (
            <div className="paste-svg-stage">
              <img src={svgDataUrl} alt={item.name} />
            </div>
          ) : tab === "preview" && item.kind === "html" ? (
            <iframe className="paste-html-frame" sandbox="" srcDoc={item.content} title={item.name} />
          ) : tab === "preview" && item.kind === "markdown" ? (
            <div className="paste-markdown-stage">
              <ModelText text={item.content} />
            </div>
          ) : (
            <div className="paste-source-wrap">
              <pre className="paste-source mono">{shownSource}</pre>
              {truncated && (
                <div className="paste-source-more">
                  <span>仅预览前 {SOURCE_PREVIEW_LIMIT.toLocaleString()} 字（共 {item.content.length.toLocaleString()} 字）</span>
                  <button type="button" className="button outline small" onClick={() => setShowAll(true)}>
                    显示全部
                  </button>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </dialog>
  );
}

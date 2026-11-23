import type { ReactNode } from "react";
import { CodeViewer } from "./CodeViewer";

export type WorkbenchTab =
  | {
      id: string;
      kind: "file";
      path: string;
      name: string;
      pinned: boolean;
      line?: number;
      jumpSeq?: number;
    }
  | {
      id: string;
      kind: "diff";
      path: string;
      name: string;
      pinned: boolean;
      stage: "unstaged" | "staged" | "commit";
      commit?: string;
    };

export interface WorkbenchProps {
  sessionId: string;
  tabs: WorkbenchTab[];
  activeTabId: string | null;
  refreshTick?: number;
  onSelectTab: (id: string) => void;
  onPinTab: (id: string) => void;
  onCloseTab: (id: string) => void;
  onCloseAllTabs: () => void;
  onBackToChat: () => void;
  onQuoteLines: (quoteText: string) => void;
  onAttachPath: (relPath: string) => void;
  onError: (message: string) => void;
  renderDiff?: (tab: Extract<WorkbenchTab, { kind: "diff" }>) => ReactNode;
}

export function Workbench({
  sessionId,
  tabs,
  activeTabId,
  refreshTick = 0,
  onSelectTab,
  onPinTab,
  onCloseTab,
  onCloseAllTabs,
  onBackToChat,
  onQuoteLines,
  onAttachPath,
  onError,
  renderDiff,
}: WorkbenchProps) {
  const active = tabs.find((t) => t.id === activeTabId) ?? tabs[0] ?? null;

  return (
    <section className="workbench" aria-label="代码与文件查看器">
      <div className="workbench-tabs-bar">
        <div className="workbench-tabs" role="tablist" aria-label="已打开的文件标签">
          {tabs.map((tab) => {
            const selected = active?.id === tab.id;
            const badge =
              tab.kind === "diff"
                ? tab.stage === "staged"
                  ? "暂存对比"
                  : tab.stage === "commit"
                    ? `${tab.commit?.slice(0, 7) ?? "提交"}`
                    : "改动对比"
                : null;
            return (
              <div
                key={tab.id}
                role="tab"
                aria-selected={selected}
                className={`wb-tab ${selected ? "active" : ""} ${tab.pinned ? "pinned" : "preview"}`}
                onMouseDown={(e) => {
                  if (e.button === 1) {
                    e.preventDefault();
                    onCloseTab(tab.id);
                  }
                }}
              >
                <button
                  type="button"
                  className="wb-tab-label"
                  title={`${tab.path}${tab.pinned ? "" : "\n（预览标签：双击固定）"}`}
                  onClick={() => onSelectTab(tab.id)}
                  onDoubleClick={() => onPinTab(tab.id)}
                >
                  <span className="wb-tab-name">{tab.name}</span>
                  {badge && <span className="wb-tab-tag">{badge}</span>}
                </button>
                <button
                  type="button"
                  className="wb-tab-close"
                  aria-label={`关闭 ${tab.name}`}
                  title="关闭标签"
                  onClick={(e) => {
                    e.stopPropagation();
                    onCloseTab(tab.id);
                  }}
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>
        <div className="workbench-bar-acts">
          {tabs.length > 1 && (
            <button type="button" className="button text small" title="关闭全部文件标签并回到对话" onClick={onCloseAllTabs}>
              全关
            </button>
          )}
          <button type="button" className="button outline small" title="把对话切回中间主区域（保留已打开的文件标签）" onClick={onBackToChat}>
            对话居中
          </button>
        </div>
      </div>

      <div className="workbench-stage">
        {!active ? (
          <div className="viewer-placeholder muted">从左侧选择文件查看。</div>
        ) : active.kind === "file" ? (
          <CodeViewer
            key={active.path}
            sessionId={sessionId}
            path={active.path}
            targetLine={active.line}
            jumpSeq={active.jumpSeq}
            refreshTick={refreshTick}
            onQuoteLines={onQuoteLines}
            onAttachPath={onAttachPath}
            onError={onError}
          />
        ) : renderDiff ? (
          renderDiff(active)
        ) : null}
      </div>
    </section>
  );
}

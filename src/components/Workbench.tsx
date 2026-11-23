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

/**
 * With the conversation back in the middle, the files that were open stay on a strip above it: each one is the
 * way back to the code, at that file, and the strip says how many are still open.
 */
export function WorkbenchDock({
  tabs,
  activeTabId,
  onOpenTab,
  onCloseTab,
  onCloseAll,
}: {
  tabs: WorkbenchTab[];
  activeTabId: string | null;
  onOpenTab: (id: string) => void;
  onCloseTab: (id: string) => void;
  onCloseAll: () => void;
}) {
  const current = tabs.find((tab) => tab.id === activeTabId) ?? tabs[tabs.length - 1];
  return (
    <div className="workbench-dock" role="toolbar" aria-label="已打开的文件">
      <button type="button" className="dock-back" title="回到代码工作台，对话停靠到右侧" onClick={() => current && onOpenTab(current.id)}>
        <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
          <path d="M5.5 5 2.5 8l3 3M10.5 5l3 3-3 3" />
        </svg>
        回到代码
      </button>
      <div className="dock-tabs">
        {tabs.map((tab) => (
          <span key={tab.id} className={`dock-tab ${tab.id === current?.id ? "current" : ""}`}>
            <button type="button" className="dock-tab-label" title={tab.path} onClick={() => onOpenTab(tab.id)}>
              {tab.name}
              {tab.kind === "diff" && <span className="wb-tab-tag">{tab.stage === "commit" ? (tab.commit?.slice(0, 7) ?? "提交") : tab.stage === "staged" ? "暂存" : "改动"}</span>}
            </button>
            <button type="button" className="wb-tab-close" aria-label={`关闭 ${tab.name}`} title="关闭" onClick={() => onCloseTab(tab.id)}>
              ×
            </button>
          </span>
        ))}
      </div>
      {tabs.length > 1 && (
        <button type="button" className="button text small dock-close-all" onClick={onCloseAll}>
          全关
        </button>
      )}
    </div>
  );
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
          <button type="button" className="button outline small" title="把对话切回中间主区域；打开的文件留在对话上方，点一下就回来" onClick={onBackToChat}>
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

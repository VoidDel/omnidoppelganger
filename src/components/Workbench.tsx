import "../styles/workbench.css";
import { Suspense, lazy, type ReactNode, useState } from "react";
import type { Status } from "../lib/types";
import { Icon } from "./Icon";
import type { ComposerQuoteInput } from "./Composer";

const CodeViewer = lazy(() => import("./CodeViewer").then((m) => ({ default: m.CodeViewer })));

export type WorkbenchTab =
  | {
      id: string;
      kind: "file";
      path: string;
      name: string;
      line?: number;
      jumpSeq?: number;
    }
  | {
      id: string;
      kind: "diff";
      path: string;
      name: string;
      stage: "unstaged" | "staged" | "commit";
      commit?: string;
      repo?: string | null;
    };

export interface WorkbenchTabsBarProps {
  tabs: WorkbenchTab[];
  /** Whether a file/diff tab is currently shown; when false, the pinned first `对话` tab is active. */
  inWorkbench: boolean;
  activeTabId: string | null;
  chatStatus: Status;
  onSelectChat: () => void;
  onSelectTab: (id: string) => void;
  onMoveTab: (fromId: string, toId: string) => void;
  onCloseTab: (id: string) => void;
  onCloseAllTabs: () => void;
}

/**
 * Unified top tab bar shown whenever any file or diff is open: `对话` always stands first, fixed in place and
 * non-closable, followed by the open file and diff tabs.
 */
export function WorkbenchTabsBar({
  tabs,
  inWorkbench,
  activeTabId,
  chatStatus,
  onSelectChat,
  onSelectTab,
  onMoveTab,
  onCloseTab,
  onCloseAllTabs,
}: WorkbenchTabsBarProps) {
  const activeFile = inWorkbench ? (tabs.find((t) => t.id === activeTabId) ?? tabs[0] ?? null) : null;
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [overId, setOverId] = useState<string | null>(null);
  return (
    <div className="workbench-tabs-bar">
      <div className="workbench-tabs" role="tablist" aria-label="对话与已打开的文件标签">
        <div role="tab" aria-selected={!inWorkbench} className={`wb-tab wb-tab-chat ${!inWorkbench ? "active" : ""}`}>
          <button type="button" className="wb-tab-label" title="主分身对话（固定在首位，不可移动或关闭）" onClick={onSelectChat}>
            <Icon name="chat-left-text" size={13} />
            <span className="wb-tab-name">对话</span>
            {inWorkbench && chatStatus === "waiting_approval" && <span className="seal small">候</span>}
            {inWorkbench && chatStatus === "running" && <span className="dot busy" title="分身运功中" />}
          </button>
        </div>
        {tabs.map((tab) => {
          const selected = activeFile?.id === tab.id;
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
              draggable={tabs.length > 1}
              className={`wb-tab ${selected ? "active" : ""} ${draggingId === tab.id ? "dragging" : ""} ${overId === tab.id && draggingId !== tab.id ? "drop-target" : ""}`}
              onDragStart={(e) => {
                setDraggingId(tab.id);
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", tab.id);
              }}
              onDragOver={(e) => {
                if (!draggingId || draggingId === tab.id) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (overId !== tab.id) setOverId(tab.id);
              }}
              onDragLeave={() => {
                if (overId === tab.id) setOverId(null);
              }}
              onDrop={(e) => {
                e.preventDefault();
                const from = draggingId ?? e.dataTransfer.getData("text/plain");
                setDraggingId(null);
                setOverId(null);
                if (from && from !== tab.id) onMoveTab(from, tab.id);
              }}
              onDragEnd={() => {
                setDraggingId(null);
                setOverId(null);
              }}
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
                title={tab.path}
                onClick={() => onSelectTab(tab.id)}
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
      {(tabs.length > 1 || (inWorkbench && chatStatus === "waiting_approval")) && (
        <div className="workbench-bar-acts">
          {inWorkbench && chatStatus === "waiting_approval" && (
            <button
              type="button"
              className="wb-await-pill"
              title="主分身有操作或提问等待本尊定夺，点击切回对话批阅"
              onClick={onSelectChat}
            >
              <span className="seal small" aria-hidden="true">
                候
              </span>
              <span>分身候旨 · 去定夺</span>
            </button>
          )}
          {tabs.length > 1 && (
            <button type="button" className="button text small" title="关闭全部文件标签" onClick={onCloseAllTabs}>
              全关
            </button>
          )}
        </div>
      )}
    </div>
  );
}

export interface WorkbenchProps {
  sessionId: string;
  tabs: WorkbenchTab[];
  activeTabId: string | null;
  refreshTick?: number;
  onQuoteToMain: (quote: ComposerQuoteInput) => void;
  onQuoteToAside?: (quote: ComposerQuoteInput) => void;
  onAttachPath: (relPath: string) => void;
  onAttachToAside?: (relPath: string) => void;
  onError: (message: string) => void;
  renderDiff?: (tab: Extract<WorkbenchTab, { kind: "diff" }>) => ReactNode;
}

export function Workbench({
  sessionId,
  tabs,
  activeTabId,
  refreshTick = 0,
  onQuoteToMain,
  onQuoteToAside,
  onAttachPath,
  onAttachToAside,
  onError,
  renderDiff,
}: WorkbenchProps) {
  const active = tabs.find((t) => t.id === activeTabId) ?? tabs[0] ?? null;

  return (
    <section className="workbench" aria-label="代码与文件查看器">
      <div className="workbench-stage">
        {!active ? (
          <div className="viewer-placeholder muted">从左侧选择文件查看。</div>
        ) : active.kind === "file" ? (
          <Suspense fallback={<div className="viewer-placeholder muted">正在加载文件…</div>}>
            <CodeViewer
              key={active.path}
              sessionId={sessionId}
              path={active.path}
              targetLine={active.line}
              jumpSeq={active.jumpSeq}
              refreshTick={refreshTick}
              onQuoteToMain={onQuoteToMain}
              onQuoteToAside={onQuoteToAside}
              onAttachPath={onAttachPath}
              onAttachToAside={onAttachToAside}
              onError={onError}
            />
          </Suspense>
        ) : renderDiff ? (
          renderDiff(active)
        ) : null}
      </div>
    </section>
  );
}

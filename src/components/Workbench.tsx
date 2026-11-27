import "../styles/workbench.css";
import { type KeyboardEvent, type ReactNode, Suspense, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { Status } from "../lib/types";
import type { QuotePayload } from "./Composer";
import { lazyWithRetry } from "./Guard";
import { Icon } from "./Icon";

const CodeViewer = lazyWithRetry(() => import("./CodeViewer").then((m) => ({ default: m.CodeViewer })));

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
 * non-closable, followed by the open file and diff tabs. Uses sibling `<button role="tab">` + close button
 * inside `.wb-tab-item` (eliminating nested interactive controls) with ArrowLeft/Right/Home/End navigation.
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
  const { t } = useI18n();
  const activeFile = inWorkbench ? (tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null) : null;
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [overId, setOverId] = useState<string | null>(null);
  const tablistRef = useRef<HTMLDivElement>(null);

  const handleTablistKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    const buttons = tablistRef.current
      ? [...tablistRef.current.querySelectorAll<HTMLButtonElement>('button[role="tab"]')]
      : [];
    if (buttons.length === 0) return;
    event.preventDefault();
    const focusedIdx = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const activeIdx = !inWorkbench ? 0 : Math.max(1, tabs.findIndex((tb) => tb.id === activeFile?.id) + 1);
    const cur = focusedIdx >= 0 ? focusedIdx : activeIdx;
    let next = cur;
    if (event.key === "Home") next = 0;
    else if (event.key === "End") next = buttons.length - 1;
    else if (event.key === "ArrowRight") next = (cur + 1) % buttons.length;
    else if (event.key === "ArrowLeft") next = (cur - 1 + buttons.length) % buttons.length;

    buttons[next]?.focus();
    if (next === 0) onSelectChat();
    else if (tabs[next - 1]) onSelectTab(tabs[next - 1].id);
  };

  return (
    <div className="workbench-tabs-bar">
      <div
        ref={tablistRef}
        className="workbench-tabs"
        role="tablist"
        aria-label={t("workbench.tablistAria")}
        onKeyDown={handleTablistKeyDown}
      >
        <div className={`wb-tab wb-tab-item wb-tab-chat ${!inWorkbench ? "active" : ""}`}>
          <button
            type="button"
            role="tab"
            aria-selected={!inWorkbench}
            tabIndex={!inWorkbench ? 0 : -1}
            className="wb-tab-label"
            title={t("workbench.chatTabTitle")}
            onClick={onSelectChat}
          >
            <Icon name="chat-left-text" size={13} />
            <span className="wb-tab-name">{t("workbench.chatTab")}</span>
            {inWorkbench && chatStatus === "waiting_approval" && <span className="seal small">{t("nav.sealAwait")}</span>}
            {inWorkbench && chatStatus === "running" && <span className="dot busy" title={t("nav.running")} />}
          </button>
        </div>
        {tabs.map((tab) => {
          const selected = activeFile?.id === tab.id;
          const badge =
            tab.kind === "diff"
              ? tab.stage === "staged"
                ? t("workbench.diffStaged")
                : tab.stage === "commit"
                  ? `${tab.commit?.slice(0, 7) ?? t("workbench.diffCommit")}`
                  : t("workbench.diffUnstaged")
              : null;
          return (
            <div
              key={tab.id}
              draggable={tabs.length > 1}
              className={`wb-tab wb-tab-item ${selected ? "active" : ""} ${draggingId === tab.id ? "dragging" : ""} ${overId === tab.id && draggingId !== tab.id ? "drop-target" : ""}`}
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
                role="tab"
                aria-selected={selected}
                tabIndex={selected ? 0 : -1}
                className="wb-tab-label"
                title={badge ? `${tab.path}（${badge}）` : tab.path}
                onClick={() => onSelectTab(tab.id)}
              >
                <span className="wb-tab-name">{tab.name}</span>
                {badge && <span className="wb-tab-tag">{badge}</span>}
              </button>
              <button
                type="button"
                className="wb-tab-close"
                aria-label={t("workbench.closeTabAria", { name: tab.name })}
                title={t("workbench.closeTab")}
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
              title={t("workbench.awaitPill")}
              onClick={onSelectChat}
            >
              <span className="seal small" aria-hidden="true">
                {t("nav.sealAwait")}
              </span>
              <span>{t("workbench.awaitPill")}</span>
            </button>
          )}
          {tabs.length > 1 && (
            <button type="button" className="button text small" title={t("workbench.closeAllTitle")} onClick={onCloseAllTabs}>
              {t("workbench.closeAll")}
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
  onQuoteToMain: (quote: QuotePayload) => void;
  onQuoteToAside?: (quote: QuotePayload) => void;
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

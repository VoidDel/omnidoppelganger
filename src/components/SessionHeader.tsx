import { useEffect, useState } from "react";
import type { SessionItem } from "../lib/types";
import { ConfirmBubble } from "./ConfirmBubble";

export interface SessionHeaderProps {
  session: SessionItem;
  /** For a 子分身: the 分身 that set it off, and the way back to it. */
  parent?: SessionItem;
  onOpenParent: () => void;
  /** For a fork: the session it was forked from, when that is still there, and the way to it. */
  source?: SessionItem;
  onOpenSource: () => void;
  /** Start a fork of the whole conversation. */
  onFork: () => void;
  /** Whether the session is being replayed, and the way into and out of that. */
  replaying: boolean;
  onReplay: () => void;
  /** For a session in a worktree of its own: bring its work back into the folder. */
  onLand: () => void;
  /** Whether the left sidebar is hidden, and the way to bring it back or put it away. */
  sidebarHidden?: boolean;
  onToggleSidebar?: () => void;
  /** Whether the side panel is put away, and the way to bring it back or put it away. */
  panelHidden: boolean;
  onTogglePanel: () => void;
  /** Whether workbench file/diff tabs exist, whether workbench view is currently active, and toggling between workbench and centered chat. */
  hasWorkbenchTabs?: boolean;
  inWorkbench?: boolean;
  onToggleWorkbench?: () => void;
  onRename: (title: string) => void;
  onDelete: () => void;
}

/** The session's title and folder, with renaming and deleting. Deleting asks once more: it cannot be undone. */
export function SessionHeader({
  session,
  parent,
  onOpenParent,
  source,
  onOpenSource,
  onFork,
  replaying,
  onReplay,
  onLand,
  sidebarHidden,
  onToggleSidebar,
  panelHidden,
  onTogglePanel,
  hasWorkbenchTabs,
  inWorkbench,
  onToggleWorkbench,
  onRename,
  onDelete,
}: SessionHeaderProps) {
  const [draft, setDraft] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  // Another session shown in the same header starts clean.
  useEffect(() => {
    setDraft(null);
    setDeleting(false);
  }, [session.id]);
  const save = () => {
    if (draft !== null && draft.trim() !== session.title) onRename(draft);
    setDraft(null);
  };
  const busy = session.status !== "idle";
  return (
    <header className="main-header">
      {sidebarHidden && onToggleSidebar && (
        <button type="button" className="icon-button" title="展开左侧栏 (Ctrl/Cmd+B)" onClick={onToggleSidebar}>
          <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
            <rect x="2" y="3" width="12" height="10" rx="1.5" />
            <path d="M6 3v10" />
          </svg>
          <span className="sr-only">展开左侧栏</span>
        </button>
      )}
      {/* The title, and under it in small letters the folder the session works in. */}
      <div className="title-block">
        {draft === null ? (
          <h1>{session.title || "新分身"}</h1>
        ) : (
          <input
            className="input title-input"
            aria-label="会话名"
            autoFocus
            value={draft}
            placeholder="留空则用第一句话作名"
            onChange={(event) => setDraft(event.target.value)}
            onBlur={save}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.nativeEvent.isComposing) save();
              if (event.key === "Escape") setDraft(null);
            }}
          />
        )}
        {/* A session in a worktree shows the folder it stands for; where it really works is one hover away. */}
        <div className="where">
          <span className="path" title={session.worktree ? `独立工作树在 ${session.workspace}` : session.workspace}>
            {session.worktree?.origin ?? session.workspace}
          </span>
          {session.branch && (
            <span className="branch" title={session.worktree ? "独立工作树的分支：分身在这上面干活，并回之前不碰洞府里的文件" : "洞府里检出的 Git 分支"}>
              <svg aria-hidden="true" width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
                <circle cx="4.5" cy="3.5" r="1.7" />
                <circle cx="4.5" cy="12.5" r="1.7" />
                <circle cx="11.5" cy="5.5" r="1.7" />
                <path d="M4.5 5.2v5.6M11.5 7.2c0 2.6-7 1.6-7 3.6" />
              </svg>
              {session.branch}
            </span>
          )}
          {session.parent && (
            <button type="button" className="kin-link" title={`这是一个子分身，出自「${parent?.title || "主分身"}」。点这里回到派它出来的分身`} onClick={onOpenParent}>
              <svg aria-hidden="true" width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12.5 13V7.5a2 2 0 0 0-2-2h-7M6.5 2.5l-3 3 3 3" />
              </svg>
              子分身
            </button>
          )}
          {session.fork && (
            <button type="button" className="kin-link" disabled={!source} title={source ? `这是一个分叉，出自「${source.title || "新分身"}」：它的前情抄自那个分身。点这里过去` : "这是一个分叉：它的前情抄自另一个分身，那个分身已经删了"} onClick={onOpenSource}>
              <svg aria-hidden="true" width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M8 14V9.5M8 9.5C8 6.5 4 7 4 3.5M8 9.5c0-3 4-2.5 4-6M2.5 5 4 3.5 5.5 5M10.5 5 12 3.5 13.5 5" />
              </svg>
              分叉
            </button>
          )}
        </div>
      </div>
      <span className="spacer" />
      {draft === null && (
        <>
          {/* Pictures to look at, words for a screen reader and for the pointer that rests on one. */}
          <button type="button" className="icon-button" title="改名" onClick={() => setDraft(session.title)}>
            <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <path d="M10.6 2.9 13.1 5.4 5.6 12.9 2.6 13.4 3.1 10.4Z" />
              <path d="m9.2 4.3 2.5 2.5" />
            </svg>
            <span className="sr-only">改名</span>
          </button>
          <button type="button" className="icon-button replay-toggle" aria-pressed={replaying} title={replaying ? "退出回溯，回到现在" : "回溯：从头一步步看这个分身做过什么"} onClick={onReplay}>
            <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <path d="M3.2 5.2A5.5 5.5 0 1 1 2.5 8M3 2.5v2.9h2.9" />
              <path d="M6.8 5.9 10.3 8 6.8 10.1z" fill="currentColor" />
            </svg>
            <span className="sr-only">回溯</span>
          </button>
          {!session.parent && (
            <button type="button" className="icon-button" title="分叉：另起一个分身，带着这里到现在的全部对话，从这里往别处走。只分对话，洞府里的文件是同一份" onClick={onFork}>
              <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <path d="M8 14V9.5M8 9.5C8 6.5 4 7 4 3.5M8 9.5c0-3 4-2.5 4-6M2.5 5 4 3.5 5.5 5M10.5 5 12 3.5 13.5 5" />
              </svg>
              <span className="sr-only">分叉</span>
            </button>
          )}
          {session.worktree && !session.parent && (
            <button type="button" className="icon-button" disabled={busy} title={busy ? "分身还在运功，先收回它" : "并回：把这个分身在独立工作树里的改动提交，合并进洞府当前的分支"} onClick={onLand}>
              <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <circle cx="4" cy="3.5" r="1.6" />
                <circle cx="4" cy="12.5" r="1.6" />
                <circle cx="12" cy="8" r="1.6" />
                <path d="M4 5.1v5.8M4 6.5c0 1.5 3 1.5 6.4 1.5" />
              </svg>
              <span className="sr-only">并回</span>
            </button>
          )}
          <span className="bubble-anchor">
            <button type="button" className="icon-button danger" disabled={busy} aria-expanded={deleting} title={busy ? "分身还在运功，先收回它" : "删除这个分身"} onClick={() => setDeleting(!deleting)}>
              <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.1 4.3l.6 8.9h6.6l.6-8.9M6.6 6.8v4M9.4 6.8v4" />
              </svg>
              <span className="sr-only">删除</span>
            </button>
            {deleting && (
              <ConfirmBubble label="删除这个分身" confirm="确认删除" onConfirm={onDelete} onCancel={() => setDeleting(false)}>
                <p>这个分身的全部记录和改文件前存的原样都会删掉，找不回来。</p>
                {session.worktree && !session.parent && (
                  <p>
                    它的独立工作树也一并删掉：没提交的改动就没了，提交过但没并回的留在分支 <span className="mono">{session.worktree.branch}</span> 上。
                  </p>
                )}
              </ConfirmBubble>
            )}
          </span>
          {hasWorkbenchTabs && onToggleWorkbench && (
            <button
              type="button"
              className="icon-button"
              aria-pressed={!!inWorkbench}
              title={inWorkbench ? "切回对话居中视图（右侧收起侧边聊天）" : "切到代码工作台视图（中间看文件，右侧侧边聊天）"}
              onClick={onToggleWorkbench}
            >
              <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="1.5" />
                <path d="M5.5 6.2 3.8 8l1.7 1.8M8.5 6.2 10.2 8 8.5 9.8" />
              </svg>
              <span className="sr-only">{inWorkbench ? "切回对话居中" : "切到代码工作台"}</span>
            </button>
          )}
          <button type="button" className="icon-button panel-toggle" aria-pressed={!panelHidden} title={panelHidden ? "展开右边的改动、终端、计划一栏" : "收起右边的改动、终端、计划一栏"} onClick={onTogglePanel}>
            {/* The window with its right-hand column: filled in while the column is showing. */}
            <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <rect x="2" y="3" width="12" height="10" rx="1.5" />
              <path d="M10 3v10" />
              {!panelHidden && <path d="M10 3h2.5A1.5 1.5 0 0 1 14 4.5v7a1.5 1.5 0 0 1-1.5 1.5H10Z" fill="currentColor" stroke="none" opacity="0.3" />}
            </svg>
            <span className="sr-only">{panelHidden ? "展开侧栏" : "收起侧栏"}</span>
          </button>
        </>
      )}
    </header>
  );
}

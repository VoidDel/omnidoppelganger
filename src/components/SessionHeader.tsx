import { useEffect, useState } from "react";
import type { SessionItem } from "../lib/types";
import { ConfirmBubble } from "./ConfirmBubble";
import { Icon } from "./Icon";

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
  /** Export the session log to a Markdown file. */
  onExport?: () => void;
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
  onExport,
  replaying,
  onReplay,
  onLand,
  sidebarHidden,
  onToggleSidebar,
  panelHidden,
  onTogglePanel,
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
    <header className="main-header" data-tauri-drag-region>
      {sidebarHidden && onToggleSidebar && (
        <button type="button" className="icon-button" title="展开左侧栏 (Ctrl/Cmd+B)" onClick={onToggleSidebar}>
          <Icon name="layout-sidebar" size={16} />
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
              <Icon name="diagram-2" size={12} />
              {session.branch}
            </span>
          )}
          {session.parent && (
            <button type="button" className="kin-link" title={`这是一个子分身，出自「${parent?.title || "主分身"}」。点这里回到派它出来的分身`} onClick={onOpenParent}>
              <Icon name="arrow-90deg-left" size={11} />
              子分身
            </button>
          )}
          {session.fork && (
            <button type="button" className="kin-link" disabled={!source} title={source ? `这是一个分叉，出自「${source.title || "新分身"}」：它的前情抄自那个分身。点这里过去` : "这是一个分叉：它的前情抄自另一个分身，那个分身已经删了"} onClick={onOpenSource}>
              <Icon name="signpost-split" size={11} />
              分叉
            </button>
          )}
        </div>
      </div>
      <span className="spacer" data-tauri-drag-region />
      {draft === null && (
        <>
          {/* Pictures to look at, words for a screen reader and for the pointer that rests on one. */}
          <button type="button" className="icon-button" title="改名" onClick={() => setDraft(session.title)}>
            <Icon name="pencil" size={15} />
            <span className="sr-only">改名</span>
          </button>
          <button type="button" className="icon-button replay-toggle" aria-pressed={replaying} title={replaying ? "退出回溯，回到现在" : "回溯：从头一步步看这个分身做过什么"} onClick={onReplay}>
            <Icon name="clock-history" size={16} />
            <span className="sr-only">回溯</span>
          </button>
          {!session.parent && (
            <button type="button" className="icon-button" title="分叉：另起一个分身，带着这里到现在的全部对话，从这里往别处走。只分对话，洞府里的文件是同一份" onClick={onFork}>
              <Icon name="signpost-split" size={16} />
              <span className="sr-only">分叉</span>
            </button>
          )}
          {onExport && (
            <button type="button" className="icon-button" title="导出为 Markdown" onClick={onExport}>
              <Icon name="download" size={15} />
              <span className="sr-only">导出为 Markdown</span>
            </button>
          )}
          {session.worktree && !session.parent && (
            <button type="button" className="icon-button" disabled={busy} title={busy ? "分身还在运功，先收回它" : "并回：把这个分身在独立工作树里的改动提交，合并进洞府当前的分支"} onClick={onLand}>
              <Icon name="git" size={16} />
              <span className="sr-only">并回</span>
            </button>
          )}
          <span className="bubble-anchor">
            <button type="button" className="icon-button danger" disabled={busy} aria-expanded={deleting} title={busy ? "分身还在运功，先收回它" : "删除这个分身"} onClick={() => setDeleting(!deleting)}>
              <Icon name="trash3" size={15} />
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
          <button type="button" className="icon-button panel-toggle" aria-pressed={!panelHidden} title={panelHidden ? "展开右边的旁聊、改动、终端一栏" : "收起右边的旁聊、改动、终端一栏"} onClick={onTogglePanel}>
            <Icon name={panelHidden ? "layout-sidebar-reverse" : "layout-sidebar-inset-reverse"} size={16} />
            <span className="sr-only">{panelHidden ? "展开侧栏" : "收起侧栏"}</span>
          </button>
        </>
      )}
    </header>
  );
}

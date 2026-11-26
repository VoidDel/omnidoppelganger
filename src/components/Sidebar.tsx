import { type ReactNode, useEffect, useRef, useState } from "react";
import pkg from "../../package.json";
import type { SessionItem } from "../lib/types";
import { ago } from "../lib/when";
import { Logo } from "./Backdrop";
import { Icon } from "./Icon";
import { Light } from "./Light";

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;

export type SidebarTab = "sessions" | "files" | "git";

export const SIDEBAR_WIDTH = { initial: 236, least: 210, most: 440 };
export const fitSidebar = (width: number) => Math.round(Math.min(Math.max(width, SIDEBAR_WIDTH.least), SIDEBAR_WIDTH.most));

export interface SidebarProps {
  sessions: SessionItem[];
  activeId: string | null;
  /** What the main area shows: a conversation, or one of the pages reached from the foot of this list. */
  page: "chat" | "settings" | "schedules" | "mcp" | "skills";
  /** Optional mode switcher when a project 洞府 is active (`分身 | 文件 | Git`). */
  tab?: SidebarTab;
  onTab?: (tab: SidebarTab) => void;
  filesPane?: ReactNode;
  gitPane?: ReactNode;
  gitCount?: number;
  width?: number;
  onResize?: (width: number) => void;
  /** Back to the home screen: no 分身 open. */
  onHome: () => void;
  onMcp: () => void;
  onSkills: () => void;
  onSchedules: () => void;
  /** Start a conversation: a 分身 in the folder kept for them. */
  onChat: () => void;
  onOpen: (id: string) => void;
  onNew: () => void;
  /** Start another session in a workspace that already has some, without asking for the folder again; `isolated` gives it a worktree of its own. */
  onNewIn: (workspace: string, isolated: boolean) => void;
  onSettings: () => void;
  /** Ids of the sessions that hold the text, in their title, folder, or conversation. */
  onSearch: (query: string) => Promise<string[]>;
  onRenameSession?: (id: string, title: string) => void;
  onForkSession?: (id: string) => void;
  onExportSession?: (id: string, title: string) => void;
  onDeleteSession?: (id: string) => void;
  onCleanScheduled?: (ids: string[]) => void;
  onRevealWorkspace?: (id: string) => void;
}

/** The time, looked at again every so often, so that what is said of how long ago stays true. */
function useNow(every: number): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), every);
    return () => clearInterval(timer);
  }, [every]);
  return now;
}

const FOLDED_KEY = "odg.folded";

/** The 洞府 whose 分身 are put away in the list, as the list was last left. */
function useFolded() {
  const [folded, setFolded] = useState<string[]>(() => {
    try {
      const kept = JSON.parse(localStorage.getItem(FOLDED_KEY) ?? "[]");
      return Array.isArray(kept) ? kept.filter((item) => typeof item === "string") : [];
    } catch {
      return [];
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(FOLDED_KEY, JSON.stringify(folded));
    } catch {
      // Storage may be unavailable; every 洞府 then starts out open.
    }
  }, [folded]);
  const toggle = (workspace: string) => setFolded((now) => (now.includes(workspace) ? now.filter((item) => item !== workspace) : [...now, workspace]));
  return [folded, toggle] as const;
}

/** A word said, in a bubble: a conversation. */
export const CHAT_ICON = <Icon name="chat-left-text" size={15} />;

export function Sidebar({
  sessions,
  activeId,
  page,
  tab = "sessions",
  onTab,
  filesPane,
  gitPane,
  gitCount = 0,
  width = SIDEBAR_WIDTH.initial,
  onResize,
  onHome,
  onMcp,
  onSkills,
  onSchedules,
  onChat,
  onOpen,
  onNew,
  onNewIn,
  onSettings,
  onSearch,
  onRenameSession,
  onForkSession,
  onExportSession,
  onDeleteSession,
  onCleanScheduled,
  onRevealWorkspace,
}: SidebarProps) {
  const now = useNow(30_000);
  const [folded, toggleFolded] = useFolded();
  const [query, setQuery] = useState("");
  const [dragging, setDragging] = useState(false);
  const held = useRef(false);
  const [menu, setMenu] = useState<{ sessionId: string; x: number; y: number; confirm?: "delete" | "clean" } | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; draft: string } | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menu) return;
    const closeOnPointer = (event: PointerEvent) => {
      if (menuRef.current && event.target instanceof Node && menuRef.current.contains(event.target)) return;
      setMenu(null);
    };
    const closeOnKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setMenu(null);
      }
    };
    const close = () => setMenu(null);
    window.addEventListener("pointerdown", closeOnPointer, true);
    window.addEventListener("keydown", closeOnKey, true);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("pointerdown", closeOnPointer, true);
      window.removeEventListener("keydown", closeOnKey, true);
      window.removeEventListener("blur", close);
    };
  }, [menu]);

  const openMenu = (sessionId: string, clientX: number, clientY: number) => {
    const x = Math.max(8, Math.min(clientX, window.innerWidth - 228));
    const y = Math.max(8, Math.min(clientY, window.innerHeight - 236));
    setMenu({ sessionId, x, y });
  };

  const idleScheduledIds = sessions.filter((s) => !!s.schedule && s.status === "idle").map((s) => s.id);
  const menuSession = menu ? sessions.find((s) => s.id === menu.sessionId) ?? null : null;
  /** The sessions the search found; null while nothing is searched for. */
  const [found, setFound] = useState<Set<string> | null>(null);
  useEffect(() => {
    if (!query.trim()) {
      setFound(null);
      return;
    }
    // Wait for a pause in typing, and drop an answer that a later search has overtaken.
    let current = true;
    const timer = setTimeout(() => onSearch(query).then((ids) => current && setFound(new Set(ids)), () => current && setFound(new Set())), 200);
    return () => {
      current = false;
      clearTimeout(timer);
    };
    // A session that appears or is renamed while searching is searched too.
  }, [query, sessions.length, onSearch]);
  const shown = found ? sessions.filter((session) => found.has(session.id)) : sessions;
  // A 子分身 is listed under the 分身 that set it off: while either is on screen or it is at work,
  // and always when it is what a search found. One whose 分身 is not listed stands by itself.
  const listed = new Set(shown.map((session) => session.id));
  const childOf = (session: SessionItem) => (session.parent && listed.has(session.parent) ? session.parent : null);
  const family = (id: string) => shown.filter((session) => childOf(session) === id);
  const open = (parent: string) => !!found || parent === activeId || family(parent).some((child) => child.id === activeId || child.status !== "idle");
  const groups = new Map<string, SessionItem[]>();
  // A session in a worktree of its own is listed with the folder it stands for.
  const homeOf = (session: SessionItem) => session.worktree?.origin ?? session.workspace;
  for (const session of shown.filter((session) => !childOf(session))) {
    const rows = [session, ...(open(session.id) ? family(session.id) : [])];
    groups.set(homeOf(session), [...(groups.get(homeOf(session)) ?? []), ...rows]);
  }
  const titleOf = (session: SessionItem) => session.title || (session.parent ? "子分身" : "新分身");
  // Those waiting for 本尊 stand at the head of the list wherever they are in it, but for the one already on screen.
  const waiting = sessions.filter((session) => session.status === "waiting_approval" && !(page === "chat" && session.id === activeId));
  const showSwitcher = page === "chat" && !!filesPane && !!onTab;
  const activeTab: SidebarTab = showSwitcher ? (tab === "git" && !gitPane ? "sessions" : tab) : "sessions";

  return (
    <nav className="sidebar" aria-label="侧边导航">
      <div className="sidebar-drag" data-tauri-drag-region />
      {onResize && (
        <div
          className={`sidebar-edge ${dragging ? "dragging" : ""}`}
          role="separator"
          aria-orientation="vertical"
          aria-label="拖动调整左侧栏宽度，双击恢复"
          aria-valuenow={width}
          tabIndex={0}
          onPointerDown={(e) => {
            e.currentTarget.setPointerCapture(e.pointerId);
            held.current = true;
            setDragging(true);
          }}
          onPointerMove={(e) => held.current && onResize(fitSidebar(e.clientX))}
          onPointerUp={() => {
            held.current = false;
            setDragging(false);
          }}
          onPointerCancel={() => {
            held.current = false;
            setDragging(false);
          }}
          onDoubleClick={() => onResize(SIDEBAR_WIDTH.initial)}
          onKeyDown={(e) => {
            if (e.key === "ArrowRight") onResize(fitSidebar(width + 24));
            if (e.key === "ArrowLeft") onResize(fitSidebar(width - 24));
          }}
        />
      )}
      {/* The name over the door is the way back to the home screen. */}
      <button type="button" className="brand" title={`回到首页 · OmniDoppelganger v${pkg.version}`} aria-current={page === "chat" && !activeId ? "page" : undefined} onClick={onHome}>
        <Logo />
        <span className="brand-name">OmniDoppelganger</span>
        <span className="brand-version">v{pkg.version}</span>
      </button>
      {showSwitcher && (
        <div className="sidebar-modes" role="tablist" aria-label="左栏视图">
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === "sessions"}
            className={`sidebar-mode ${activeTab === "sessions" ? "active" : ""}`}
            onClick={() => onTab("sessions")}
          >
            分身
            {waiting.length > 0 && <span className="seal small">候</span>}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === "files"}
            className={`sidebar-mode ${activeTab === "files" ? "active" : ""}`}
            onClick={() => onTab("files")}
          >
            文件
          </button>
          {gitPane && (
            <button
              type="button"
              role="tab"
              aria-selected={activeTab === "git"}
              className={`sidebar-mode ${activeTab === "git" ? "active" : ""}`}
              onClick={() => onTab("git")}
            >
              Git
              {gitCount > 0 && <span className="mode-badge">{gitCount}</span>}
            </button>
          )}
        </div>
      )}
      {activeTab === "files" && filesPane ? (
        <div className="sidebar-pane">{filesPane}</div>
      ) : activeTab === "git" && gitPane ? (
        <div className="sidebar-pane">{gitPane}</div>
      ) : (
        <>
          {/* Work in a folder, or just talk: a 分身 for a project, or one in the folder kept for conversations. */}
          <div className="sidebar-new">
        <button type="button" className="button primary" onClick={onNew}>
          <Icon name="plus-lg" size={14} />
          开辟洞府
        </button>
        <button type="button" className="button outline" title="闲谈：不选文件夹，直接和分身说话" onClick={onChat}>
          {CHAT_ICON}
          闲谈
        </button>
      </div>
      {sessions.length > 0 && (
        <>
          <label htmlFor="session-search" className="sr-only">
            搜索会话
          </label>
          <input id="session-search" className="input" type="search" placeholder="搜标题、洞府、说过的话" value={query} onChange={(event) => setQuery(event.target.value)} />
        </>
      )}
      {waiting.length > 0 && (
        <div className="awaiting" role="group" aria-label="候本尊定夺的分身">
          {waiting.map((session) => (
            // A slip of the talisman's own paper: the request it stands for is on that 分身's page.
            <button type="button" key={session.id} className="await-slip" title={`「${titleOf(session)}」在等本尊定夺，点开去看`} onClick={() => onOpen(session.id)}>
              <span className="seal small" aria-hidden="true">
                候
              </span>
              <span className="await-title">{titleOf(session)}</span>
              <span className="await-where">{baseName(homeOf(session))}</span>
            </button>
          ))}
        </div>
      )}
      <div className="sidebar-scroll">
        {found && shown.length === 0 && <p className="muted">没有哪个分身提过这个。</p>}
        {[...groups].map(([workspace, items]) => {
          // A search opens every 洞府 it found something in.
          const closed = !found && folded.includes(workspace);
          const asking = items.filter((session) => session.status === "waiting_approval").length;
          const working = items.filter((session) => session.status === "running").length;
          return (
            <div className={`group ${closed ? "closed" : ""}`} key={workspace}>
              <div className="group-head">
                <button type="button" className="group-name" title={`${workspace}\n点一下${closed ? "展开" : "收起"}这处洞府的分身`} aria-expanded={!closed} disabled={!!found} onClick={() => toggleFolded(workspace)}>
                  <Icon name="chevron-right" size={12} className="group-caret" />
                  <span className="group-label">{baseName(workspace)}</span>
                  {/* Put away, the 洞府 still says how many are in it, and whether any is at work or waiting. */}
                  {closed && (
                    <span className="group-count">
                      {asking > 0 && <span className="dot await" title={`${asking} 个候旨`} />}
                      {working > 0 && <span className="dot busy" title={`${working} 个运功中`} />}
                      {items.length}
                    </span>
                  )}
                </button>
                {/* Offered where Git is: a 分身 with a worktree of its own, to work beside the others without touching their files. */}
                {items.some((session) => session.branch) && (
                  <button
                    type="button"
                    className="group-add apart"
                    title={`在 ${baseName(workspace)} 再化一个分身，给它一份独立的工作树（Git worktree）：它改它的，不碰这里的文件，干完再并回`}
                    aria-label={`在 ${baseName(workspace)} 化一个用独立工作树的分身`}
                    onClick={() => (closed && toggleFolded(workspace), onNewIn(workspace, true))}
                  >
                    <Icon name="diagram-2" size={14} />
                  </button>
                )}
                <button type="button" className="group-add" title={`在 ${baseName(workspace)} 这处洞府里再化一个分身`} aria-label={`在 ${baseName(workspace)} 这处洞府里再化一个分身`} onClick={() => (closed && toggleFolded(workspace), onNewIn(workspace, false))}>
                  <Icon name="plus-lg" size={13} />
                </button>
              </div>
              {!closed &&
                items.map((session) => {
                  const child = !!childOf(session);
                  const state = session.status === "waiting_approval" ? "await" : session.status === "running" ? "busy" : "";
                  if (renaming?.id === session.id) {
                    const commitRename = () => {
                      if (renaming.draft.trim() !== session.title) onRenameSession?.(session.id, renaming.draft);
                      setRenaming(null);
                    };
                    return (
                      <div className={`session-rename ${child ? "child" : ""}`} key={session.id}>
                        <input
                          className="input session-rename-input"
                          aria-label="会话名"
                          autoFocus
                          value={renaming.draft}
                          placeholder="留空则用第一句话作名"
                          onChange={(e) => setRenaming({ id: session.id, draft: e.target.value })}
                          onBlur={commitRename}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" && !e.nativeEvent.isComposing) commitRename();
                            if (e.key === "Escape") setRenaming(null);
                          }}
                        />
                      </div>
                    );
                  }
                  const hasMenu = !!(onRenameSession || onForkSession || onDeleteSession || onRevealWorkspace);
                  return (
                    <div className={`session-slot ${child ? "child" : ""} ${menu?.sessionId === session.id ? "menu-open" : ""}`} key={session.id}>
                      <button
                        type="button"
                        className={`row-button session ${child ? "child" : ""} ${state}`}
                        aria-current={page === "chat" && session.id === activeId ? "true" : undefined}
                        onClick={() => onOpen(session.id)}
                        onContextMenu={
                          hasMenu
                            ? (e) => {
                                e.preventDefault();
                                openMenu(session.id, e.clientX, e.clientY);
                              }
                            : undefined
                        }
                      >
                        {/* How it stands, as a seal: 候 in cinnabar while it waits for 本尊, 运 in jade while it works, a plain ring at rest. */}
                        <span className={`row-seal ${state}`} title={state === "await" ? "候旨：在等本尊定夺" : state === "busy" ? "运功中" : undefined} aria-hidden={state ? undefined : "true"}>
                          {state === "busy" && <Light />}
                          {state === "await" ? "候" : state === "busy" ? "运" : ""}
                        </span>
                        <span className="row-text">
                          <span className="row-title">{titleOf(session)}</span>
                          {/* Under the title, small: when it was last at work, and what kind of 分身 it is. A 子分身 keeps to one line. */}
                          {!child && (
                            <span className="row-meta">
                              <span className="row-when" title={new Date(session.updated_at).toLocaleString()}>
                                {state === "await" ? "候旨" : state === "busy" ? "运功中" : ago(session.updated_at, now)}
                              </span>
                              {session.fork && <span className="chip kin" title="分叉：它的前情抄自另一个分身">分叉</span>}
                              {session.schedule && <span className="chip kin" title={`功课「${session.schedule}」到时辰化出的分身`}>功课</span>}
                              {session.worktree && <span className="chip kin" title={`用独立的工作树，分支 ${session.worktree.branch}`}>独立</span>}
                              {!open(session.id) && family(session.id).length > 0 && <span className="chip kin" title="它派出过的子分身，点开这个分身就能看到">{family(session.id).length} 子</span>}
                            </span>
                          )}
                        </span>
                      </button>
                      {hasMenu && !child && (
                        <button
                          type="button"
                          className="session-more"
                          aria-label={`管理「${titleOf(session)}」`}
                          aria-haspopup="menu"
                          aria-expanded={menu?.sessionId === session.id}
                          title="管理分身（右键也可唤出）"
                          onClick={(e) => {
                            e.stopPropagation();
                            if (menu?.sessionId === session.id) {
                              setMenu(null);
                              return;
                            }
                            const rect = e.currentTarget.getBoundingClientRect();
                            openMenu(session.id, rect.right, rect.bottom + 4);
                          }}
                        >
                          <Icon name="three-dots" size={14} />
                        </button>
                      )}
                    </div>
                  );
                })}
            </div>
          );
        })}
      </div>
        </>
      )}
      {/* The pages at the foot, close together: one group, apart from the 分身 above. */}
      <div className="sidebar-foot">
        <button type="button" className="row-button" aria-current={page === "skills" ? "page" : undefined} title="秘籍：分身遇到对得上的事才研读的做事说明" onClick={onSkills}>
          <Icon name="book" size={15} />
          秘籍
        </button>
        <button type="button" className="row-button" aria-current={page === "mcp" ? "page" : undefined} title="法器：给分身添上本事的 MCP 服务" onClick={onMcp}>
          <Icon name="bell" size={15} />
          法器
        </button>
        <button type="button" className="row-button" aria-current={page === "schedules" ? "page" : undefined} title="功课：到了时辰，自行化出分身去办" onClick={onSchedules}>
          <Icon name="clock" size={15} />
          功课
        </button>
        <button type="button" className="row-button" aria-current={page === "settings" ? "page" : undefined} onClick={onSettings}>
          <Icon name="gear" size={15} />
          设置
        </button>
      </div>
      {menu && menuSession && (
        <div
          ref={menuRef}
          className="session-menu"
          role="menu"
          aria-label={`「${titleOf(menuSession)}」操作菜单`}
          style={{ left: `${menu.x}px`, top: `${menu.y}px` }}
        >
          {menu.confirm === "delete" ? (
            <div className="session-menu-confirm">
              <p>删除「{titleOf(menuSession)}」及其快照，不可恢复。</p>
              <div className="session-menu-confirm-acts">
                <button type="button" className="button text small" onClick={() => setMenu({ ...menu, confirm: undefined })}>
                  取消
                </button>
                <button
                  type="button"
                  className="button solid small"
                  onClick={() => {
                    const id = menuSession.id;
                    setMenu(null);
                    onDeleteSession?.(id);
                  }}
                >
                  确认删掉
                </button>
              </div>
            </div>
          ) : menu.confirm === "clean" ? (
            <div className="session-menu-confirm">
              <p>一并清理 {idleScheduledIds.length} 个已收功的功课分身？</p>
              <div className="session-menu-confirm-acts">
                <button type="button" className="button text small" onClick={() => setMenu({ ...menu, confirm: undefined })}>
                  取消
                </button>
                <button
                  type="button"
                  className="button solid small"
                  onClick={() => {
                    const ids = [...idleScheduledIds];
                    setMenu(null);
                    onCleanScheduled?.(ids);
                  }}
                >
                  确认清扫
                </button>
              </div>
            </div>
          ) : (
            <>
              {onRenameSession && (
                <button
                  type="button"
                  role="menuitem"
                  className="session-menu-item"
                  onClick={() => {
                    setRenaming({ id: menuSession.id, draft: menuSession.title });
                    setMenu(null);
                  }}
                >
                  重命名分身
                </button>
              )}
              {!menuSession.parent && onForkSession && (
                <button
                  type="button"
                  role="menuitem"
                  className="session-menu-item"
                  onClick={() => {
                    const id = menuSession.id;
                    setMenu(null);
                    onForkSession(id);
                  }}
                >
                  从此处分叉会话
                </button>
              )}
              {onExportSession && (
                <button
                  type="button"
                  role="menuitem"
                  className="session-menu-item"
                  onClick={() => {
                    const { id, title } = menuSession;
                    setMenu(null);
                    onExportSession(id, title);
                  }}
                >
                  导出为 Markdown…
                </button>
              )}
              {onRevealWorkspace && (
                <button
                  type="button"
                  role="menuitem"
                  className="session-menu-item"
                  onClick={() => {
                    const id = menuSession.id;
                    setMenu(null);
                    onRevealWorkspace(id);
                  }}
                >
                  在文件夹中显示洞府
                </button>
              )}
              {onCleanScheduled && menuSession.schedule && idleScheduledIds.length > 1 && (
                <button
                  type="button"
                  role="menuitem"
                  className="session-menu-item"
                  onClick={() => setMenu({ ...menu, confirm: "clean" })}
                >
                  清扫已办功课分身（{idleScheduledIds.length}）
                </button>
              )}
              {onDeleteSession && (
                <>
                  <div className="session-menu-sep" role="separator" />
                  <button
                    type="button"
                    role="menuitem"
                    className="session-menu-item danger"
                    disabled={menuSession.status !== "idle"}
                    title={menuSession.status !== "idle" ? "分身正在运功或候旨，请先收回" : undefined}
                    onClick={() => setMenu({ ...menu, confirm: "delete" })}
                  >
                    删除这个分身…
                  </button>
                </>
              )}
            </>
          )}
        </div>
      )}
    </nav>
  );
}

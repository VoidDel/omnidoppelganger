import { type ReactNode, useEffect, useRef, useState } from "react";
import type { SessionItem } from "../lib/types";
import { ago } from "../lib/when";
import { Logo } from "./Backdrop";
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
export const CHAT_ICON = (
  <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round">
    <path d="M2.5 3.5h11v7.5H7.5l-3 2.5V11h-2z" />
  </svg>
);

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
}: SidebarProps) {
  const now = useNow(30_000);
  const [folded, toggleFolded] = useFolded();
  const [query, setQuery] = useState("");
  const [dragging, setDragging] = useState(false);
  const held = useRef(false);
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
      <button type="button" className="brand" title="回到首页" aria-current={page === "chat" && !activeId ? "page" : undefined} onClick={onHome}>
        <Logo />
        <span>OmniDoppelganger</span>
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
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
            <path d="M8 3v10M3 8h10" />
          </svg>
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
                  <svg className="group-caret" aria-hidden="true" width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M3 2l3.5 3L3 8" />
                  </svg>
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
                    <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
                      <circle cx="4.5" cy="3.5" r="1.7" />
                      <circle cx="4.5" cy="12.5" r="1.7" />
                      <circle cx="11.5" cy="5.5" r="1.7" />
                      <path d="M4.5 5.2v5.6M11.5 7.2c0 2.6-7 1.6-7 3.6" />
                    </svg>
                  </button>
                )}
                <button type="button" className="group-add" title={`在 ${baseName(workspace)} 这处洞府里再化一个分身`} aria-label={`在 ${baseName(workspace)} 这处洞府里再化一个分身`} onClick={() => (closed && toggleFolded(workspace), onNewIn(workspace, false))}>
                  <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
                    <path d="M8 3v10M3 8h10" />
                  </svg>
                </button>
              </div>
              {!closed &&
                items.map((session) => {
                  const child = !!childOf(session);
                  const state = session.status === "waiting_approval" ? "await" : session.status === "running" ? "busy" : "";
                  return (
                    <button
                      type="button"
                      key={session.id}
                      className={`row-button session ${child ? "child" : ""} ${state}`}
                      aria-current={page === "chat" && session.id === activeId ? "true" : undefined}
                      onClick={() => onOpen(session.id)}
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
          {/* A bound book, open at its spine: a 秘籍. */}
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <path d="M8 4c-1.6-1.2-3.6-1.5-5.5-1.2v9.8c1.9-.3 3.9 0 5.5 1.2 1.6-1.2 3.6-1.5 5.5-1.2V2.8C11.6 2.5 9.6 2.8 8 4zM8 4v9.6" />
          </svg>
          秘籍
        </button>
        <button type="button" className="row-button" aria-current={page === "mcp" ? "page" : undefined} title="法器：给分身添上本事的 MCP 服务" onClick={onMcp}>
          {/* A bell of bronze hung from its loop: a 法器. */}
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <path d="M8 1.8v1.4M4.6 11.5V7.4a3.4 3.4 0 0 1 6.8 0v4.1M3 11.5h10M6.6 13.6a1.5 1.5 0 0 0 2.8 0" />
          </svg>
          法器
        </button>
        <button type="button" className="row-button" aria-current={page === "schedules" ? "page" : undefined} title="功课：到了时辰，自行化出分身去办" onClick={onSchedules}>
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="8" cy="8" r="6" />
            <path d="M8 4.5V8l2.5 1.5" />
          </svg>
          功课
        </button>
        <button type="button" className="row-button" aria-current={page === "settings" ? "page" : undefined} onClick={onSettings}>
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <circle cx="8" cy="8" r="2.2" />
            <path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4" />
          </svg>
          设置
        </button>
      </div>
    </nav>
  );
}

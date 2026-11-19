import { type ReactNode, useEffect, useRef, useState } from "react";
import { launchFrom } from "../lib/flight";
import { type ChangedFile, type ToolItem, type Usage, shortCount } from "../lib/fold";
import { MODES, modeOf, nextMode } from "../lib/modes";
import type { Image, Mode, Provider, SessionItem, Status, Todo } from "../lib/types";

/** Far mountains in three washes with the golden core behind them: the backdrop of the home screen. */
export function Mountains() {
  return (
    <svg className="mountains" aria-hidden="true" viewBox="0 0 1200 420" preserveAspectRatio="xMidYMax slice">
      <circle className="core" cx="860" cy="150" r="46" />
      <path className="far" d="M0 300 L120 210 L210 262 L330 150 L430 236 L520 190 L640 270 L760 170 L880 250 L980 205 L1090 262 L1200 215 L1200 420 L0 420 Z" />
      <path className="mid" d="M0 350 L90 292 L190 330 L300 250 L390 312 L500 268 L610 330 L720 262 L830 318 L950 276 L1060 326 L1200 286 L1200 420 L0 420 Z" />
      <path className="near" d="M0 392 L140 340 L260 378 L380 322 L500 372 L640 336 L770 384 L900 344 L1030 388 L1200 352 L1200 420 L0 420 Z" />
    </svg>
  );
}

// One circle split along an S into two halves, drawn around the origin with radius 10.
// SELF is 本尊's half, DOUBLE is the 分身's: together they close into the whole circle.
const SELF = "M0-10A10 10 0 0 1 0 10A5 5 0 0 0 0 0A5 5 0 0 1 0-10Z";
const DOUBLE = "M0-10A10 10 0 0 0 0 10A5 5 0 0 0 0 0A5 5 0 0 1 0-10Z";

/**
 * The 分身 mark as a status sign: the halves rest together when idle, the 分身's half
 * drifts out and back while it works, and it turns cinnabar and holds apart while it
 * waits for a decision.
 */
export function Mark({ state = "idle", size = 26 }: { state?: "idle" | "working" | "waiting"; size?: number }) {
  return (
    <svg className={`mark ${state}`} aria-hidden="true" width={size} height={size} viewBox="0 0 26 26">
      <g transform="translate(13 13) rotate(30)">
        <path className="self" d={SELF} />
        <path className="double" d={DOUBLE} />
      </g>
    </svg>
  );
}

/** The logo: the split circle stamped on a cinnabar seal. Used where the product names itself. */
export function Logo({ size = 26 }: { size?: number }) {
  return (
    <svg className="logo" aria-hidden="true" width={size} height={size} viewBox="0 0 26 26">
      <rect className="plate" x="1" y="1" width="24" height="24" rx="5.5" />
      <g transform="translate(13 13) rotate(30) scale(0.74)">
        <path className="self" d={SELF} transform="translate(0.7 0)" />
        <path className="double" d={DOUBLE} transform="translate(-0.7 0)" />
      </g>
    </svg>
  );
}

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;

interface SidebarProps {
  sessions: SessionItem[];
  activeId: string | null;
  /** What the main area shows: a conversation, or one of the two pages reached from the foot of this list. */
  page: "chat" | "settings" | "schedules";
  onSchedules: () => void;
  onOpen: (id: string) => void;
  onNew: () => void;
  /** Start another session in a workspace that already has some, without asking for the folder again; `isolated` gives it a worktree of its own. */
  onNewIn: (workspace: string, isolated: boolean) => void;
  onSettings: () => void;
  /** Ids of the sessions that hold the text, in their title, folder, or conversation. */
  onSearch: (query: string) => Promise<string[]>;
}

export function Sidebar({ sessions, activeId, page, onSchedules, onOpen, onNew, onNewIn, onSettings, onSearch }: SidebarProps) {
  const [query, setQuery] = useState("");
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
  return (
    <nav className="sidebar" aria-label="会话">
      <div className="brand">
        <Logo />
        <span>OmniDoppelganger</span>
      </div>
      <button type="button" className="button primary" onClick={onNew}>
        <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
          <path d="M8 3v10M3 8h10" />
        </svg>
        开辟洞府
      </button>
      {sessions.length > 0 && (
        <>
          <label htmlFor="session-search" className="sr-only">
            搜索会话
          </label>
          <input id="session-search" className="input" type="search" placeholder="搜标题、洞府、说过的话" value={query} onChange={(event) => setQuery(event.target.value)} />
        </>
      )}
      <div className="sidebar-scroll">
        {found && shown.length === 0 && <p className="muted">没有哪个分身提过这个。</p>}
        {[...groups].map(([workspace, items]) => (
          <div className="group" key={workspace}>
            <div className="group-head">
              <div className="group-name" title={workspace}>
                {baseName(workspace)}
              </div>
              {/* Offered where Git is: a 分身 with a worktree of its own, to work beside the others without touching their files. */}
              {items.some((session) => session.branch) && (
                <button
                  type="button"
                  className="group-add apart"
                  title={`在 ${baseName(workspace)} 再化一个分身，给它一份独立的工作树（Git worktree）：它改它的，不碰这里的文件，干完再并回`}
                  aria-label={`在 ${baseName(workspace)} 化一个用独立工作树的分身`}
                  onClick={() => onNewIn(workspace, true)}
                >
                  <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
                    <circle cx="4.5" cy="3.5" r="1.7" />
                    <circle cx="4.5" cy="12.5" r="1.7" />
                    <circle cx="11.5" cy="5.5" r="1.7" />
                    <path d="M4.5 5.2v5.6M11.5 7.2c0 2.6-7 1.6-7 3.6" />
                  </svg>
                </button>
              )}
              <button type="button" className="group-add" title={`在 ${baseName(workspace)} 这处洞府里再化一个分身`} aria-label={`在 ${baseName(workspace)} 这处洞府里再化一个分身`} onClick={() => onNewIn(workspace, false)}>
                <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
                  <path d="M8 3v10M3 8h10" />
                </svg>
              </button>
            </div>
            {items.map((session) => (
              <button
                type="button"
                key={session.id}
                className={`row-button ${childOf(session) ? "child" : ""}`}
                aria-current={page === "chat" && session.id === activeId ? "true" : undefined}
                onClick={() => onOpen(session.id)}
              >
                <span className="row-title">{session.title || (childOf(session) ? "子分身" : "新分身")}</span>
                {session.schedule && <span className="chip kin" title={`功课「${session.schedule}」到时辰化出的分身`}>功课</span>}
                {session.worktree && !childOf(session) && <span className="chip kin" title={`用独立的工作树，分支 ${session.worktree.branch}`}>独立</span>}
                {!childOf(session) && !open(session.id) && family(session.id).length > 0 && <span className="chip kin" title="它派出过的子分身，点开这个分身就能看到">{family(session.id).length} 子</span>}
                {session.status === "waiting_approval" && <span className="chip await">候旨</span>}
                {session.status === "running" && <span className="chip busy">运功中</span>}
              </button>
            ))}
          </div>
        ))}
      </div>
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
    </nav>
  );
}

const MODE_ICONS: Record<Mode, string> = {
  manual: "M5.5 8.5V4a1 1 0 0 1 2 0v3.5M7.5 7V2.8a1 1 0 0 1 2 0V7M9.5 7V3.8a1 1 0 0 1 2 0V8.5M11.5 7.5a1 1 0 0 1 2 0V10a4.5 4.5 0 0 1-4.5 4.5h-.6a4 4 0 0 1-3.2-1.6L3.3 10.3a1 1 0 0 1 1.5-1.3l.7.8",
  edit: "M6 4 2 8l4 4M10 4l4 4-4 4",
  plan: "M4 2.5h8v11H4zM6 5.5h4M6 8h4M6 10.5h2.5",
  auto: "M9 1.5 3.5 9H8l-1 5.5L12.5 7H8z",
};

function ModeIcon({ mode }: { mode: Mode }) {
  return (
    <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
      <path d={MODE_ICONS[mode]} />
    </svg>
  );
}

interface PopupProps {
  /** What the button shows. */
  label: ReactNode;
  title?: string;
  disabled?: boolean;
  /** What the menu is, for a screen reader. */
  name: string;
  /** The menu's contents, given a function that closes it. */
  children: (close: () => void) => ReactNode;
}

/** A button in the composer row whose menu opens above it, and closes on Escape or a click elsewhere. */
function Popup({ label, title, disabled, name, children }: PopupProps) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => !root.current?.contains(event.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);
  return (
    <div className="mode-picker" ref={root} onKeyDown={(event) => event.key === "Escape" && setOpen(false)}>
      <button type="button" className="select-button" aria-haspopup="menu" aria-expanded={open} title={title} disabled={disabled} onClick={() => setOpen(!open)}>
        {label}
      </button>
      {open && (
        <div className="mode-menu" role="menu" aria-label={name}>
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

const CHECK = (
  <svg className="check" aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
    <path d="m3 8.5 3.2 3L13 4.5" />
  </svg>
);

interface ModelListProps {
  provider: Provider | undefined;
  providerId: string;
  model: string;
  onPick: (model: string) => void;
  listModels: (provider: string) => Promise<[string, string][]>;
}

/** The models to choose from: what the provider says it serves, or the built-in suggestions while that is unknown. */
function ModelList({ provider, providerId, model, onPick, listModels }: ModelListProps) {
  const [asked, setAsked] = useState<{ state: "asking" | "told" | "failed"; models: [string, string][]; why?: string }>({ state: "asking", models: [] });
  const [filter, setFilter] = useState("");
  // Asked each time the menu opens: a provider's list changes, and so does the key it is asked with.
  useEffect(() => {
    let current = true;
    listModels(providerId).then(
      (models) => current && setAsked({ state: "told", models }),
      (error) => current && setAsked({ state: "failed", models: [], why: String(error) }),
    );
    return () => {
      current = false;
    };
  }, [providerId, listModels]);
  const known = asked.state === "told" ? asked.models : (provider?.models ?? []);
  // The model in use is always there to return to, even when the provider's list leaves it out.
  const models = known.some(([id]) => id === model) ? known : [[model, model] as [string, string], ...known];
  const shown = models.filter(([id, name]) => `${id} ${name}`.toLowerCase().includes(filter.trim().toLowerCase()));
  const name = provider?.name ?? providerId;
  return (
    <>
      <div className="mode-menu-head">
        <span>{name} 的功法</span>
        <span>{asked.state === "asking" ? "正在问它有哪些…" : asked.state === "told" ? `它报了 ${asked.models.length} 门` : "没问到，先列内置的"}</span>
      </div>
      {asked.state === "failed" && <p className="menu-note">{asked.why}</p>}
      {models.length > 8 && <input className="input menu-filter" type="search" placeholder="筛一筛" aria-label="筛选功法" autoFocus value={filter} onChange={(event) => setFilter(event.target.value)} />}
      <div className="menu-scroll">
        {shown.map(([id, display]) => (
          <button type="button" role="menuitemradio" aria-checked={id === model} key={id} className="mode-option plain" onClick={() => onPick(id)}>
            <span className="mode-text">
              <span className="name mono">{id}</span>
              {display !== id && <span className="hint">{display}</span>}
            </span>
            {id === model ? CHECK : <span />}
          </button>
        ))}
        {shown.length === 0 && <p className="menu-note">没有对得上的。</p>}
      </div>
      <p className="menu-note">换了之后，这个分身接下来的话由新功法来答，前情照旧带着。只能在同一出处里换。</p>
    </>
  );
}

/** How the context limit reads in the menu: 50000 as 5 万. */
const LIMITS = [50_000, 100_000, 200_000, 500_000];

/** The 禁制 button and its menu: what the 分身 may do without asking. */
function ModePicker({ mode, onMode }: { mode: Mode; onMode: (mode: Mode) => void }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => !root.current?.contains(event.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);
  const current = modeOf(mode);
  return (
    <div className="mode-picker" ref={root} onKeyDown={(event) => event.key === "Escape" && setOpen(false)}>
      <button type="button" className="select-button" aria-haspopup="menu" aria-expanded={open} title={`禁制：${current.hint}。Shift+Tab 切换`} onClick={() => setOpen(!open)}>
        <ModeIcon mode={mode} />
        <span className="label sr-only">禁制</span>
        <span className="value">{current.name}</span>
      </button>
      {open && (
        <div className="mode-menu" role="menu" aria-label="禁制">
          <div className="mode-menu-head">
            <span>禁制</span>
            <span>
              <kbd>Shift</kbd> + <kbd>Tab</kbd> 切换
            </span>
          </div>
          {MODES.map((option) => (
            <button
              type="button"
              role="menuitemradio"
              aria-checked={option.value === mode}
              key={option.value}
              className="mode-option"
              autoFocus={option.value === mode}
              onClick={() => {
                onMode(option.value);
                setOpen(false);
              }}
            >
              <ModeIcon mode={option.value} />
              <span className="mode-text">
                <span className="name">{option.name}</span>
                <span className="hint">{option.hint}</span>
              </span>
              <svg className="check" aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                {option.value === mode && <path d="m3 8.5 3.2 3L13 4.5" />}
              </svg>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

interface SessionHeaderProps {
  session: SessionItem;
  /** For a 子分身: the 分身 that set it off, and the way back to it. */
  parent?: SessionItem;
  onOpenParent: () => void;
  /** For a session in a worktree of its own: bring its work back into the folder. */
  onLand: () => void;
  /** Whether the side panel is put away, and the way to bring it back or put it away. */
  panelHidden: boolean;
  onTogglePanel: () => void;
  onRename: (title: string) => void;
  onDelete: () => void;
}

/** The session's title and folder, with renaming and deleting. Deleting asks once more: it cannot be undone. */
export function SessionHeader({ session, parent, onOpenParent, onLand, panelHidden, onTogglePanel, onRename, onDelete }: SessionHeaderProps) {
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
      {session.parent && (
        <button type="button" className="kin-link" title="这是一个子分身，回到派它出来的分身" onClick={onOpenParent}>
          <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <path d="M12.5 13V7.5a2 2 0 0 0-2-2h-7M6.5 2.5l-3 3 3 3" />
          </svg>
          子分身，出自「{parent?.title || "主分身"}」
        </button>
      )}
      {/* A session in a worktree shows the folder it stands for; where it really works is one hover away. */}
      <span className="path" title={session.worktree ? `独立工作树在 ${session.workspace}` : session.workspace}>
        {session.worktree?.origin ?? session.workspace}
      </span>
      {session.branch && (
        <span className="branch" title={session.worktree ? "独立工作树的分支：分身在这上面干活，并回之前不碰洞府里的文件" : "洞府里检出的 Git 分支"}>
          <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <circle cx="4.5" cy="3.5" r="1.7" />
            <circle cx="4.5" cy="12.5" r="1.7" />
            <circle cx="11.5" cy="5.5" r="1.7" />
            <path d="M4.5 5.2v5.6M11.5 7.2c0 2.6-7 1.6-7 3.6" />
          </svg>
          {session.branch}
        </span>
      )}
      <span className="spacer" />
      {deleting ? (
        <span className="undo-confirm">
          这个分身的全部记录和改文件前存的原样都会删掉，找不回来。
          {session.worktree && !session.parent && `它的独立工作树也一并删掉：没提交的改动就没了，提交过但没并回的留在分支 ${session.worktree.branch} 上。`}
          <button type="button" className="button outline red small" onClick={onDelete}>
            确认删除
          </button>
          <button type="button" className="button text small" onClick={() => setDeleting(false)}>
            算了
          </button>
        </span>
      ) : (
        draft === null && (
          <>
            {/* Pictures to look at, words for a screen reader and for the pointer that rests on one. */}
            <button type="button" className="icon-button" title="改名" onClick={() => setDraft(session.title)}>
              <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <path d="M10.6 2.9 13.1 5.4 5.6 12.9 2.6 13.4 3.1 10.4Z" />
                <path d="m9.2 4.3 2.5 2.5" />
              </svg>
              <span className="sr-only">改名</span>
            </button>
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
            <button type="button" className="icon-button danger" disabled={busy} title={busy ? "分身还在运功，先收回它" : "删除这个分身"} onClick={() => setDeleting(true)}>
              <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                <path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.1 4.3l.6 8.9h6.6l.6-8.9M6.6 6.8v4M9.4 6.8v4" />
              </svg>
              <span className="sr-only">删除</span>
            </button>
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
        )
      )}
    </header>
  );
}

// What a message may carry, as the core checks it: these formats, this many, this large.
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const IMAGES_PER_MESSAGE = 8;
// 5 MB of base64 is 3.75 MB of file.
const IMAGE_BYTES = 3_700_000;

/** A file's bytes in base64, without the `data:` prefix a reader puts in front. */
const readBase64 = (file: File) =>
  new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).replace(/^data:[^,]*,/, ""));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });

interface ComposerProps {
  status: Status;
  /** The session's provider as the core describes it, and its id for when this build does not know it. */
  provider: Provider | undefined;
  providerId: string;
  /** The id of the model the session is on. */
  model: string;
  /** Whether the model takes pictures; null when that is not known, in which case they are sent. */
  vision: boolean | null;
  mode: Mode;
  usage: Usage;
  /** The number of tokens at which the conversation is compacted, the most it can be for this model, and the limit as set. */
  contextLimit: number;
  contextCeiling: number;
  contextSetting: number;
  /** Set the limit, for every session. */
  onLimit: (limit: number) => void;
  onModel: (model: string) => void;
  listModels: (provider: string) => Promise<[string, string][]>;
  /** The last turn in plan mode ended with the 分身's answer: offer to carry it out. */
  planReady: boolean;
  onMode: (mode: Mode) => void;
  /** Switch to `mode`, then tell the 分身 to carry out its plan. */
  onCarryOut: (mode: Mode) => void;
  onSend: (text: string, images: Image[]) => void;
  onStop: () => void;
}

export function Composer(props: ComposerProps) {
  const { status, provider, providerId, model, vision, mode, usage, contextLimit, contextCeiling, contextSetting, planReady } = props;
  const { onLimit, onModel, listModels, onMode, onCarryOut, onSend, onStop } = props;
  const providerName = provider?.name ?? providerId;
  const limits = [...LIMITS.filter((limit) => limit < contextCeiling), contextCeiling];
  const [text, setText] = useState("");
  const [images, setImages] = useState<Image[]>([]);
  /** Why a picture was not taken, until the next attempt. */
  const [refused, setRefused] = useState<string | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const busy = status !== "idle";
  const empty = !text.trim() && images.length === 0;
  const sender = useRef<HTMLButtonElement>(null);
  const submit = () => {
    if (empty) return;
    // The token on the button flies to the message's place in the conversation.
    launchFrom(sender.current);
    onSend(text, images);
    setText("");
    setImages([]);
    setRefused(null);
  };
  /** Attach the pictures among `files`, saying why when one cannot be sent. */
  const attach = async (files: File[]) => {
    setRefused(null);
    if (vision === false) return setRefused(`${model} 这门功法只认字，看不了图。点左下的功法，换一门能看图的。`);
    const taken: Image[] = [];
    for (const file of files) {
      if (!IMAGE_TYPES.includes(file.type)) setRefused("只能附 PNG、JPEG、GIF、WebP 格式的图。");
      else if (file.size > IMAGE_BYTES) setRefused("这张图超过 3.7 MB，太大了，缩小或截小一点再附。");
      else if (images.length + taken.length >= IMAGES_PER_MESSAGE) setRefused(`一次最多附 ${IMAGES_PER_MESSAGE} 张图。`);
      else taken.push({ media_type: file.type, data: await readBase64(file) });
    }
    if (taken.length) setImages((current) => [...current, ...taken].slice(0, IMAGES_PER_MESSAGE));
  };
  return (
    <div className="composer">
      <div className="composer-status" role="status">
        {status === "running" && (
          <>
            <Mark state="working" size={18} />
            <span>分身正在运功。此刻传音，它做完这一步就会听到，不会打断它。</span>
          </>
        )}
        {status === "waiting_approval" && (
          <>
            <Mark state="waiting" size={18} />
            <span>分身收功候旨，本尊定夺后继续。</span>
          </>
        )}
        {status === "idle" && planReady && (
          <>
            <span>计划已呈上。本尊点头，就换个禁制让它动手：</span>
            {MODES.filter((option) => option.value === "edit" || option.value === "auto").map((option) => (
              <button type="button" key={option.value} className="button outline small" onClick={() => onCarryOut(option.value)}>
                {option.name}，照计划动手
              </button>
            ))}
          </>
        )}
      </div>
      <div className="composer-box">
        {(images.length > 0 || refused) && (
          <div className="attachments">
            {images.map((image, index) => (
              <span className="attachment" key={index}>
                <img src={`data:${image.media_type};base64,${image.data}`} alt={`要附的第 ${index + 1} 张图`} />
                <button type="button" aria-label={`去掉第 ${index + 1} 张图`} onClick={() => setImages(images.filter((_, at) => at !== index))}>
                  ×
                </button>
              </span>
            ))}
            {refused && (
              <span className="refused" role="alert">
                {refused}
              </span>
            )}
          </div>
        )}
        <label htmlFor="composer-input" className="sr-only">
          给分身的法旨
        </label>
        <textarea
          id="composer-input"
          rows={2}
          value={text}
          placeholder={busy ? "给分身传一句音" : "吩咐一件事，回车传令"}
          onChange={(event) => setText(event.target.value)}
          onPaste={(event) => {
            // A pasted screenshot is attached; pasted text goes into the box as usual.
            const files = [...event.clipboardData.items].filter((item) => item.kind === "file" && item.type.startsWith("image/")).flatMap((item) => item.getAsFile() ?? []);
            if (files.length === 0) return;
            event.preventDefault();
            void attach(files);
          }}
          onKeyDown={(event) => {
            // Enter sends; Shift+Enter breaks the line. Enter that confirms an input-method candidate does neither.
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              submit();
            }
            // Shift+Tab cycles the 禁制 instead of moving focus back.
            if (event.key === "Tab" && event.shiftKey) {
              event.preventDefault();
              onMode(nextMode(mode));
            }
          }}
        />
        <div className="composer-row">
          <Popup
            name="功法"
            disabled={busy}
            title={busy ? `功法：${providerName} · ${model}。分身运功时不能换，等它收功` : `功法：${providerName} · ${model}。点开换一门`}
            label={
              <>
                <span className="label sr-only">功法</span>
                <span className="value">{model}</span>
              </>
            }
          >
            {(close) => (
              <ModelList
                provider={provider}
                providerId={providerId}
                model={model}
                listModels={listModels}
                onPick={(picked) => {
                  if (picked !== model) onModel(picked);
                  close();
                }}
              />
            )}
          </Popup>
          <ModePicker mode={mode} onMode={onMode} />
          <Popup
            name="灵石"
            title="灵石：前情占了多少 / 到多少就凝成摘要。点开看累计用量、改上限"
            label={
              <>
                <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M5 2.5h6l3 4-6 7-6-7zM2 6.5h12M5 2.5l3 4 3-4" />
                </svg>
                <span className="label sr-only">灵石</span>
                <span className="value">
                  {shortCount(usage.context)} / {shortCount(contextLimit)}
                </span>
              </>
            }
          >
            {(close) => (
              <>
                <div className="mode-menu-head">
                  <span>灵石</span>
                  <span>这个分身累计</span>
                </div>
                <p className="menu-note">
                  输入 {shortCount(usage.spent.input)}（其中 {shortCount(usage.spent.cached)} 读自缓存，便宜得多），输出 {shortCount(usage.spent.output)}。眼下的前情占 {shortCount(usage.context)}。
                </p>
                <div className="mode-menu-head">
                  <span>前情上限</span>
                  <span>到了就凝成摘要</span>
                </div>
                {limits.map((limit) => {
                  // A setting above what the model can hold lands on the model's own ceiling.
                  const chosen = limit === Math.min(contextSetting, contextCeiling);
                  return (
                    <button
                      type="button"
                      role="menuitemradio"
                      aria-checked={chosen}
                      key={limit}
                      className="mode-option plain"
                      onClick={() => {
                        if (!chosen) onLimit(limit);
                        close();
                      }}
                    >
                      <span className="mode-text">
                        <span className="name">{shortCount(limit)}</span>
                        {limit === contextCeiling && <span className="hint">这门功法装得下的最多</span>}
                      </span>
                      {chosen ? CHECK : <span />}
                    </button>
                  );
                })}
                <p className="menu-note">每次请求都要把整段前情发给功法，上限越高，长对话越费灵石。对所有分身生效；别的数可以到设置里填。</p>
              </>
            )}
          </Popup>
          <input
            ref={picker}
            type="file"
            accept={IMAGE_TYPES.join(",")}
            multiple
            hidden
            onChange={(event) => {
              void attach([...(event.target.files ?? [])]);
              // The same file can be picked again after being removed.
              event.target.value = "";
            }}
          />
          <button
            type="button"
            className="select-button"
            disabled={vision === false}
            title={vision === false ? `${model} 只认字，看不了图` : "附图给分身看，也可以直接把截图粘贴进输入框"}
            onClick={() => picker.current?.click()}
          >
            <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <rect x="2" y="3" width="12" height="10" rx="1.5" />
              <circle cx="5.7" cy="6.5" r="1.1" />
              <path d="m2.5 11.5 3.2-3 2.3 2 2.5-2.7 3 3.2" />
            </svg>
            <span className="sr-only">附图</span>
          </button>
          <span className="spacer" />
          {/* A 令牌, the tablet an order is sent with: 令 on jade for an order, 音 on gold for a word to a 分身 at work.
              While the 分身 works the token is away with the order, and shows only when there is a word to send after it. */}
          {(!busy || !empty) && (
            <button type="button" key={busy ? "word" : "order"} ref={sender} className="token-button" disabled={empty} title={busy ? "传音：分身做完这一步就会听到" : "传令（回车）"} onClick={submit}>
            <svg className={`token ${busy ? "word" : ""}`} aria-hidden="true" width="32" height="40" viewBox="0 0 32 40">
                <path className="plate" d="M7 1.5h18l5.5 5.5v21L16 38.5 1.5 28V7Z" />
                <path className="rim" d="M8.2 4.5h15.6l3.7 3.7v18.3L16 34.8 4.5 26.5V8.2Z" />
                <circle className="hole" cx="16" cy="8.6" r="1.7" />
                <text x="16" y="26" textAnchor="middle">
                  {busy ? "音" : "令"}
                </text>
              </svg>
              <span className="sr-only">{busy ? "传音" : "传令"}</span>
            </button>
          )}
          {/* In the token's place while the 分身 works: a token of cinnabar, the order that calls it back. */}
          {busy && (
            <button type="button" className="token-button stop" title="收回分身：停下这一轮" onClick={onStop}>
              <svg className="token halt" aria-hidden="true" width="32" height="40" viewBox="0 0 32 40">
                <path className="plate" d="M7 1.5h18l5.5 5.5v21L16 38.5 1.5 28V7Z" />
                <path className="rim" d="M8.2 4.5h15.6l3.7 3.7v18.3L16 34.8 4.5 26.5V8.2Z" />
                <circle className="hole" cx="16" cy="8.6" r="1.7" />
                <text x="16" y="26" textAnchor="middle">
                  止
                </text>
              </svg>
              <span className="sr-only">收回分身</span>
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

const TODO_MARKS: Record<Todo["status"], string> = { pending: "○", in_progress: "▶", done: "✓" };

/** The side panel's width in pixels: as the user left it, within what leaves the conversation room. */
export const PANEL_WIDTH = { initial: 360, least: 260 };
export const fitPanel = (width: number) => Math.round(Math.min(Math.max(width, PANEL_WIDTH.least), Math.max(PANEL_WIDTH.least, window.innerWidth - 248 - 420)));

interface SidePanelProps {
  files: ChangedFile[];
  runs: ToolItem[];
  todos: Todo[];
  width: number;
  onResize: (width: number) => void;
}

export function SidePanel({ files, runs, todos, width, onResize }: SidePanelProps) {
  const [tab, setTab] = useState<"files" | "terminal" | "plan">("files");
  const [dragging, setDragging] = useState(false);
  const done = todos.filter((todo) => todo.status === "done").length;
  return (
    <aside className="panel" aria-label="改动与终端">
      {/* The edge to drag: the panel is as wide as the pointer is far from the window's right side. */}
      <div
        className={`panel-edge ${dragging ? "dragging" : ""}`}
        role="separator"
        aria-orientation="vertical"
        aria-label="拖动调整侧栏宽度，双击恢复"
        aria-valuenow={width}
        tabIndex={0}
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId);
          setDragging(true);
        }}
        onPointerMove={(event) => dragging && onResize(fitPanel(window.innerWidth - event.clientX))}
        onPointerUp={() => setDragging(false)}
        onPointerCancel={() => setDragging(false)}
        onDoubleClick={() => onResize(fitPanel(PANEL_WIDTH.initial))}
        onKeyDown={(event) => {
          // Left widens the panel, right narrows it: the edge moves the way the arrow points.
          if (event.key === "ArrowLeft") onResize(fitPanel(width + 24));
          if (event.key === "ArrowRight") onResize(fitPanel(width - 24));
        }}
      />
      <div className="tabs">
        <button type="button" className="tab" aria-pressed={tab === "files"} onClick={() => setTab("files")}>
          改动
        </button>
        <button type="button" className="tab" aria-pressed={tab === "terminal"} onClick={() => setTab("terminal")}>
          终端
        </button>
        <button type="button" className="tab" aria-pressed={tab === "plan"} onClick={() => setTab("plan")}>
          计划{todos.length > 0 ? ` ${done}/${todos.length}` : ""}
        </button>
      </div>
      {tab === "plan" ? (
        <div className="panel-body">
          <h2>分身的计划</h2>
          {todos.length === 0 && <p className="muted">分身还没有列计划。事情分好几步时，它会在这里记下每一步，做完一步勾一步。</p>}
          <ol className="todos">
            {todos.map((todo, index) => (
              <li key={index} className={`todo ${todo.status}`}>
                <span className="mark" aria-hidden="true">
                  {TODO_MARKS[todo.status]}
                </span>
                <span className="sr-only">{todo.status === "done" ? "已办：" : todo.status === "in_progress" ? "正在办：" : "待办："}</span>
                <span className="text">{todo.text}</span>
              </li>
            ))}
          </ol>
        </div>
      ) : tab === "files" ? (
        <div className="panel-body">
          <h2>分身动过的文件</h2>
          {files.length === 0 && <p className="muted">分身还没有改过文件。</p>}
          {files.map((file) => (
            <div className="file-row" key={file.path}>
              <span className="target" title={file.path}>
                {file.path}
              </span>
              <span className="outcome">
                加 {file.added} 删 {file.removed}
              </span>
            </div>
          ))}
        </div>
      ) : (
        <div className="panel-body">
          {runs.length === 0 && <p className="muted">分身还没有运行过命令。</p>}
          {runs.map((run) => {
            const meta = run.result!.meta!;
            return (
              <pre className="terminal" key={run.id}>
                <span className="prompt">$</span> {meta.command}
                {run.name === "run_command" && meta.background ? <span className="muted">{`\n[后台运行，编号 ${meta.job}]`}</span> : null}
                {run.name !== "run_command" ? <span className="muted">{`  （${meta.job}${run.name === "stop_command" ? "，停止" : ""}）`}</span> : null}
                {"\n"}
                {meta.output}
                {meta.stopped ? <span className="failed">{`\n[${meta.stopped === "timed out" ? "超时停止" : "已停止"}]`}</span> : null}
                {!meta.stopped && !meta.background && !meta.running && meta.exit_code !== 0 ? <span className="failed">{`\n[退出码 ${meta.exit_code ?? "未知"}]`}</span> : null}
              </pre>
            );
          })}
        </div>
      )}
    </aside>
  );
}

import { type CSSProperties, type ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";
import { launchFrom } from "../lib/flight";
import { type ChangedFile, type ToolItem, type Usage, shortCount } from "../lib/fold";
import { BYPASS, MODES, modeOf, nextMode } from "../lib/modes";
import type { Image, Mode, Provider, SessionItem, Status, Todo } from "../lib/types";
import { ago } from "../lib/when";
import { Diff } from "./Transcript";

/** Far mountains in three washes with the golden core behind them: the backdrop of the home screen. */
export function Mountains() {
  return (
    <svg className="mountains" aria-hidden="true" viewBox="0 0 1200 420" preserveAspectRatio="xMidYMax slice">
      <defs>
        <radialGradient id="core-halo">
          <stop offset="0" stopColor="var(--gold)" stopOpacity="0.5" />
          <stop offset="1" stopColor="var(--gold)" stopOpacity="0" />
        </radialGradient>
        <linearGradient id="mist" x1="0" x2="1">
          <stop offset="0" stopColor="var(--bg)" stopOpacity="0" />
          <stop offset="0.5" stopColor="var(--bg)" stopOpacity="0.85" />
          <stop offset="1" stopColor="var(--bg)" stopOpacity="0" />
        </linearGradient>
      </defs>
      <circle className="halo" cx="860" cy="150" r="120" fill="url(#core-halo)" />
      <circle className="core" cx="860" cy="150" r="46" />
      <path className="far" d="M0 300 L120 210 L210 262 L330 150 L430 236 L520 190 L640 270 L760 170 L880 250 L980 205 L1090 262 L1200 215 L1200 420 L0 420 Z" />
      {/* Two bands of mist, between the far range and the near ones, drifting slowly the same way at different paces. */}
      <rect className="mist slow" x="-600" y="236" width="900" height="54" rx="27" fill="url(#mist)" />
      <rect className="mist fast" x="-500" y="292" width="700" height="40" rx="20" fill="url(#mist)" />
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

export function Sidebar({ sessions, activeId, page, onSchedules, onOpen, onNew, onNewIn, onSettings, onSearch }: SidebarProps) {
  const now = useNow(30_000);
  const [folded, toggleFolded] = useFolded();
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
  const titleOf = (session: SessionItem) => session.title || (session.parent ? "子分身" : "新分身");
  // Those waiting for 本尊 stand at the head of the list wherever they are in it, but for the one already on screen.
  const waiting = sessions.filter((session) => session.status === "waiting_approval" && !(page === "chat" && session.id === activeId));
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
              <button type="button" className="group-name" title={`${workspace}\n点一下${closed ? "展开" : "收起"}这处洞府的分身`} aria-expanded={!closed} onClick={() => toggleFolded(workspace)}>
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
  // An open lock: nothing is kept shut.
  bypass: "M4 7.5h8v6H4zM6 7.5V5a2 2 0 0 1 3.9-.6M8 10v1.2",
};

/** Said before 全权托付 is switched on, and only switched on when 本尊 stamps it. */
function BypassDialog({ onConfirm, onClose }: { onConfirm: () => void; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  // The keyboard starts on 算了: Enter alone must not hand everything over.
  useEffect(() => {
    dialog.current?.showModal();
    cancel.current?.focus();
  }, []);
  return (
    <dialog ref={dialog} className="dialog danger" aria-labelledby="bypass-title" onClose={onClose}>
      <form
        method="dialog"
        onSubmit={(event) => {
          event.preventDefault();
          onConfirm();
        }}
      >
        <h2 id="bypass-title">全权托付这个分身？</h2>
        <p>托付之后，分身做什么都不再请示：跑任何命令、改洞府之外的文件、改 .git、上网、动用身外法器，一概自行放行。</p>
        <p>
          <strong>Windows 上没有结界保护。</strong>它能删除、改写这台电脑上本尊能动的任何文件，也能把文件内容发到网上。它在独立工作树里改的东西，并回前不会进洞府；但洞府之外的一切不受保护。
        </p>
        <p className="muted">只对这个分身生效，不会成为新分身的默认，功课也用不了。随时可以在禁制菜单里换回别的。</p>
        <div className="actions">
          <button type="submit" className="button solid">
            确认托付
          </button>
          <button type="button" className="button text" ref={cancel} onClick={onClose}>
            算了
          </button>
        </div>
      </form>
    </dialog>
  );
}

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
      <p className="menu-note">换了之后，这个分身接下来的话由新功法来答，前情照旧带着。只能在同一宗门里换。</p>
    </>
  );
}

/** The limits the menu offers, read there as 50K, 100K, 200K, 500K. */
const LIMITS = [50_000, 100_000, 200_000, 500_000];

/** The 禁制 button and its menu: what the 分身 may do without asking. */
function ModePicker({ mode, onMode, trustable }: { mode: Mode; onMode: (mode: Mode) => void; trustable: boolean }) {
  const [open, setOpen] = useState(false);
  /** 全权托付 was picked: the dialog that says what it means is open. */
  const [confirming, setConfirming] = useState(false);
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
      <button
        type="button"
        className={`select-button ${mode === "bypass" ? "danger" : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        title={`禁制：${current.hint}。Shift+Tab 切换`}
        onClick={() => setOpen(!open)}
      >
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
          {/* Apart from the four, in cinnabar, and only where a worktree keeps its work off the folder's own files. */}
          <button
            type="button"
            role="menuitemradio"
            aria-checked={mode === "bypass"}
            className="mode-option danger"
            disabled={!trustable}
            title={trustable ? undefined : "只有独立工作树的分身才能全权托付：在洞府名旁点分支图标化一个"}
            onClick={() => {
              setOpen(false);
              if (mode !== "bypass") setConfirming(true);
            }}
          >
            <ModeIcon mode="bypass" />
            <span className="mode-text">
              <span className="name">{BYPASS.name}</span>
              <span className="hint">{trustable ? BYPASS.hint : "只有独立工作树的分身才能托付"}</span>
            </span>
            <svg className="check" aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              {mode === "bypass" && <path d="m3 8.5 3.2 3L13 4.5" />}
            </svg>
          </button>
        </div>
      )}
      {confirming && (
        <BypassDialog
          onConfirm={() => {
            setConfirming(false);
            onMode("bypass");
          }}
          onClose={() => setConfirming(false)}
        />
      )}
    </div>
  );
}

interface SessionHeaderProps {
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
  /** Whether the side panel is put away, and the way to bring it back or put it away. */
  panelHidden: boolean;
  onTogglePanel: () => void;
  onRename: (title: string) => void;
  onDelete: () => void;
}

/** The session's title and folder, with renaming and deleting. Deleting asks once more: it cannot be undone. */
export function SessionHeader({ session, parent, onOpenParent, source, onOpenSource, onFork, replaying, onReplay, onLand, panelHidden, onTogglePanel, onRename, onDelete }: SessionHeaderProps) {
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
  /** Whether the limit follows each model's own window. */
  contextAuto: boolean;
  /** Set the limit, for every session. */
  onLimit: (limit: number | "auto") => void;
  /** Have the 分身 summarize the conversation now. */
  onCompact: () => void;
  onModel: (model: string) => void;
  listModels: (provider: string) => Promise<[string, string][]>;
  /** The last turn in plan mode ended with the 分身's answer: offer to carry it out. */
  planReady: boolean;
  onMode: (mode: Mode) => void;
  /** Switch to `mode`, then tell the 分身 to carry out its plan. */
  onCarryOut: (mode: Mode) => void;
  onSend: (text: string, images: Image[]) => void;
  onStop: () => void;
  /** Whether the session works in a worktree of its own, the only place 全权托付 may be switched on. */
  trustable: boolean;
  /** Words to put in the box, as when a fork brings back the message it starts before; a new `id` puts them in. */
  seed: { id: number; text: string } | null;
}

/**
 * How full the conversation is, as five 灵石 lit one after another: each stands for a fifth of the room there is
 * before it is summarized. They are jade, turn gold past seven tenths, and cinnabar past nine, where the last one beats.
 */
function Stones({ part }: { part: number }) {
  const full = Math.min(Math.max(part, 0), 1);
  return (
    <span className={`stones ${full >= 0.9 ? "hot" : full >= 0.7 ? "warm" : ""}`} aria-hidden="true">
      {[0, 1, 2, 3, 4].map((at) => (
        <span key={at} style={{ "--lit": Math.min(Math.max(full * 5 - at, 0), 1) } as CSSProperties} />
      ))}
    </span>
  );
}

/** How tall the box to type in stands with nothing in it, in pixels: as it starts, the least, and where it is kept. */
const BOX = { initial: 54, least: 28, key: "odg.composer" };
/** The most it may be made, or grow to with what is typed: it leaves the conversation most of the window. */
const tallest = () => Math.max(BOX.initial, Math.round(window.innerHeight * 0.55));
const fitBox = (height: number) => Math.round(Math.min(Math.max(height, BOX.least), tallest()));

/** The height the box was last left at. */
function useBoxHeight() {
  const [height, setHeight] = useState(() => {
    try {
      const kept = Number(localStorage.getItem(BOX.key));
      return kept > 0 ? fitBox(kept) : BOX.initial;
    } catch {
      return BOX.initial;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(BOX.key, String(height));
    } catch {
      // Storage may be unavailable; the box then starts out as it does the first time.
    }
  }, [height]);
  return [height, setHeight] as const;
}

export function Composer(props: ComposerProps) {
  const { status, provider, providerId, model, vision, mode, usage, contextLimit, contextCeiling, contextSetting, contextAuto, planReady } = props;
  const { onLimit, onCompact, onModel, listModels, onMode, onCarryOut, onSend, onStop, seed, trustable } = props;
  const providerName = provider?.name ?? providerId;
  const limits = [...LIMITS.filter((limit) => limit < contextCeiling), contextCeiling];
  const [text, setText] = useState("");
  const input = useRef<HTMLTextAreaElement>(null);
  const [height, setHeight] = useBoxHeight();
  const [dragging, setDragging] = useState(false);
  /** Where a drag of the box's top edge began: the pointer, and the height then. Read by the pointer's own events. */
  const held = useRef<{ y: number; height: number } | null>(null);
  // The box is as tall as it was left, and taller while what is typed needs more, up to what leaves the conversation room.
  useLayoutEffect(() => {
    const box = input.current;
    if (!box) return;
    box.style.height = `${height}px`;
    box.style.height = `${Math.min(Math.max(box.scrollHeight, height), tallest())}px`;
  }, [text, height]);
  useEffect(() => {
    if (!seed) return;
    setText(seed.text);
    input.current?.focus();
  }, [seed?.id]);
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
        {mode === "bypass" && status === "idle" && !planReady && (
          <span className="trusted-note">全权托付中：分身什么都不请示。</span>
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
      {/* The frame carries the light round the box: jade and gold going round while the 分身 works, cinnabar held while it waits. */}
      <div className={`composer-frame ${status === "running" ? "working" : status === "waiting_approval" ? "waiting" : ""}`}>
      <div className={`composer-box ${mode === "bypass" ? "trusted" : ""}`}>
        {/* The box's top edge, to drag: up makes more room to write in, down gives it back to the conversation. */}
        <div
          className={`composer-grip ${dragging ? "dragging" : ""}`}
          role="separator"
          aria-orientation="horizontal"
          aria-label="拖动调整输入框高度，双击恢复"
          aria-valuenow={height}
          tabIndex={0}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
            held.current = { y: event.clientY, height };
            setDragging(true);
          }}
          onPointerMove={(event) => held.current && setHeight(fitBox(held.current.height + held.current.y - event.clientY))}
          onPointerUp={() => ((held.current = null), setDragging(false))}
          onPointerCancel={() => ((held.current = null), setDragging(false))}
          onDoubleClick={() => setHeight(BOX.initial)}
          onKeyDown={(event) => {
            if (event.key === "ArrowUp") setHeight(fitBox(height + 24));
            if (event.key === "ArrowDown") setHeight(fitBox(height - 24));
          }}
        />
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
          ref={input}
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
                <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M3.5 2.5h8a1 1 0 0 1 1 1v10h-8a1 1 0 0 1-1-1zM3.5 11.5a1 1 0 0 1 1-1h8M6 5.5h4" />
                </svg>
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
          <ModePicker mode={mode} onMode={onMode} trustable={trustable} />
          <Popup
            name="灵石"
            title={`灵石：前情占了 ${shortCount(usage.context)}，到 ${shortCount(contextLimit)} 就凝成摘要。点开看累计用量、改上限`}
            label={
              <>
                <Stones part={contextLimit > 0 ? usage.context / contextLimit : 0} />
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
                <button
                  type="button"
                  role="menuitem"
                  className="mode-option plain"
                  disabled={busy || usage.context === 0}
                  title={busy ? "分身运功时不能凝练，等它收功" : usage.context === 0 ? "还没有前情可凝" : undefined}
                  onClick={() => {
                    onCompact();
                    close();
                  }}
                >
                  <span className="mode-text">
                    <span className="name">现在就凝成摘要</span>
                    <span className="hint">不等到上限：让分身把前情凝成一段摘要，之后的请求省灵石。摘要会丢掉细节</span>
                  </span>
                  <span />
                </button>
                <div className="mode-menu-head">
                  <span>前情上限</span>
                  <span>到了就凝成摘要</span>
                </div>
                <button
                  type="button"
                  role="menuitemradio"
                  aria-checked={contextAuto}
                  className="mode-option plain"
                  onClick={() => {
                    if (!contextAuto) onLimit("auto");
                    close();
                  }}
                >
                  <span className="mode-text">
                    <span className="name">自动</span>
                    <span className="hint">按功法自己的窗口，用到八成：这门是 {shortCount(contextCeiling)}</span>
                  </span>
                  {contextAuto ? CHECK : <span />}
                </button>
                {limits.map((limit) => {
                  // A setting above what the model can hold lands on the model's own ceiling.
                  const chosen = !contextAuto && limit === Math.min(contextSetting, contextCeiling);
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
            <svg className={`token ${busy ? "word" : ""}`} aria-hidden="true" width="26" height="32" viewBox="0 0 32 40">
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
              <svg className="token halt" aria-hidden="true" width="26" height="32" viewBox="0 0 32 40">
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
    </div>
  );
}

/** The seal on a step of the plan: nothing yet for one to come, 办 for the one in hand, 毕 for one done. */
const TODO_SEALS: Record<Todo["status"], string> = { pending: "", in_progress: "办", done: "毕" };
const TODO_STATES: Record<Todo["status"], string> = { pending: "busy", in_progress: "busy", done: "done" };

/** The panel's width while there is nothing in it: a strip that holds its three tabs. */
export const PANEL_STRIP = 56;

type PanelTab = "files" | "terminal" | "plan";

const TABS: { id: PanelTab; name: string; title: string; icon: ReactNode }[] = [
  {
    id: "files",
    name: "改动",
    title: "改动：分身动过的文件",
    icon: (
      <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
        <path d="M4 1.8h5.2L12 4.6v9.6H4z" />
        <path d="M9.2 1.8v2.8H12M6.2 8h3.6M8 6.2v3.6M6.2 11.8h3.6" />
      </svg>
    ),
  },
  {
    id: "terminal",
    name: "终端",
    title: "终端：分身跑过的命令和输出",
    icon: (
      <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
        <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="1.5" />
        <path d="M4.6 6.2 6.8 8l-2.2 1.8M8.4 10h3" />
      </svg>
    ),
  },
  {
    id: "plan",
    name: "计划",
    title: "计划：分身列的步骤，做完一步勾一步",
    icon: (
      <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
        <path d="M2.2 4.2l1.3 1.3 2.2-2.4M2.2 9.2l1.3 1.3 2.2-2.4M8.2 4.4h5.6M8.2 9.4h5.6M2.6 13.4h2.6M8.2 13.4h5.6" />
      </svg>
    ),
  },
];

/** A file the 分身 changed: where it is, how much it changed, and, opened, the changes themselves. */
function FileRow({ file }: { file: ChangedFile }) {
  const [open, setOpen] = useState(false);
  const cut = Math.max(file.path.lastIndexOf("/"), file.path.lastIndexOf("\\")) + 1;
  const total = file.added + file.removed;
  return (
    <div className={`file-row ${open ? "open" : ""}`}>
      <button type="button" className="file-head" aria-expanded={file.hunks.length ? open : undefined} disabled={!file.hunks.length} title={file.path} onClick={() => setOpen(!open)}>
        <span className="file-where">
          <span className="file-name">{file.path.slice(cut)}</span>
          {cut > 0 && <span className="file-dir">{file.path.slice(0, cut - 1)}</span>}
        </span>
        {file.created && <span className="chip kin">新</span>}
        {/* How much went in and how much came out, as a bar of fixed length, then in figures. */}
        <span className="file-bar" aria-hidden="true">
          <span className="in" style={{ flexGrow: total ? file.added : 0 }} />
          <span className="out" style={{ flexGrow: total ? file.removed : 0 }} />
        </span>
        <span className="file-count">
          <span className="in">+{file.added}</span> <span className="out">−{file.removed}</span>
        </span>
      </button>
      {open && (
        <div className="file-diff">
          {file.hunks.map((hunk, at) => (
            <Diff key={at} hunk={hunk} />
          ))}
        </div>
      )}
    </div>
  );
}

/** The side panel's width in pixels: as the user left it, within what leaves the conversation room. */
export const PANEL_WIDTH = { initial: 360, least: 260 };
export const fitPanel = (width: number) => Math.round(Math.min(Math.max(width, PANEL_WIDTH.least), Math.max(PANEL_WIDTH.least, window.innerWidth - 248 - 420)));

interface SidePanelProps {
  files: ChangedFile[];
  runs: ToolItem[];
  todos: Todo[];
  width: number;
  onResize: (width: number) => void;
  /** Nothing in it to show, so it stands as a strip; a tab pressed there asks for it to open anyway. */
  narrow: boolean;
  onWiden: () => void;
}

export function SidePanel({ files, runs, todos, width, onResize, narrow, onWiden }: SidePanelProps) {
  const [tab, setTab] = useState<PanelTab>("files");
  const counts: Record<PanelTab, number> = { files: files.length, terminal: runs.length, plan: todos.length };
  // When the first thing comes into a panel that held nothing, it opens on that.
  const had = useRef(counts);
  useEffect(() => {
    const before = had.current;
    had.current = counts;
    if (before.files + before.terminal + before.plan > 0) return;
    const first = TABS.find((each) => before[each.id] === 0 && counts[each.id] > 0);
    if (first) setTab(first.id);
  }, [files.length, runs.length, todos.length]);
  // The steps done while the page was watched, for a moment: their ink is drawn through them as it happens.
  const [struck, setStruck] = useState<Set<string>>(new Set());
  const steps = useRef(new Map(todos.map((todo) => [todo.text, todo.status])));
  useEffect(() => {
    const now = todos.filter((todo) => todo.status === "done" && steps.current.has(todo.text) && steps.current.get(todo.text) !== "done").map((todo) => todo.text);
    steps.current = new Map(todos.map((todo) => [todo.text, todo.status]));
    if (now.length === 0) return;
    setStruck((was) => new Set([...was, ...now]));
    const timer = setTimeout(() => setStruck((was) => new Set([...was].filter((text) => !now.includes(text)))), 1600);
    return () => clearTimeout(timer);
  }, [todos]);
  const [dragging, setDragging] = useState(false);
  // Read by the pointer's own events, which can come before the page has drawn again: the state above is for how the edge looks.
  const held = useRef(false);
  const hold = (down: boolean) => {
    held.current = down;
    setDragging(down);
  };
  const done = todos.filter((todo) => todo.status === "done").length;
  // After every hook: the strip and the panel must call the same ones, in the same order.
  if (narrow) {
    return (
      <aside className="panel strip" aria-label="改动与终端">
        <div className="strip-tabs">
          {TABS.map((each) => (
            <button
              type="button"
              key={each.id}
              className="tab"
              title={`${each.title}。还是空的，点开看`}
              onClick={() => {
                setTab(each.id);
                onWiden();
              }}
            >
              {each.icon}
              <span className="sr-only">{each.name}</span>
            </button>
          ))}
        </div>
        {/* Written down the strip, as on a scroll: why there is nothing here yet. */}
        <p className="strip-note">分身尚未动手</p>
      </aside>
    );
  }
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
          hold(true);
        }}
        onPointerMove={(event) => held.current && onResize(fitPanel(window.innerWidth - event.clientX))}
        onPointerUp={() => hold(false)}
        onPointerCancel={() => hold(false)}
        onDoubleClick={() => onResize(fitPanel(PANEL_WIDTH.initial))}
        onKeyDown={(event) => {
          // Left widens the panel, right narrows it: the edge moves the way the arrow points.
          if (event.key === "ArrowLeft") onResize(fitPanel(width + 24));
          if (event.key === "ArrowRight") onResize(fitPanel(width - 24));
        }}
      />
      <div className="tabs">
        {/* Pictures to look at; each one's name is in its title and for a screen reader. */}
        {TABS.map((each) => (
          <button type="button" key={each.id} className="tab" aria-pressed={tab === each.id} title={each.title} onClick={() => setTab(each.id)}>
            {each.icon}
            <span className="sr-only">{each.id === "plan" ? `${each.name} ` : each.name}</span>
            {each.id === "plan" && todos.length > 0 && <span className="count">{`${done}/${todos.length}`}</span>}
          </button>
        ))}
      </div>
      {tab === "plan" ? (
        <div className="panel-body">
          <div className="panel-head">
            <h2>分身的计划</h2>
            {todos.length > 0 && (
              <span className="panel-meta">
                {done} / {todos.length} 已毕
              </span>
            )}
          </div>
          {todos.length === 0 && <p className="muted">分身还没有列计划。事情分好几步时，它会在这里记下每一步，做完一步勾一步。</p>}
          {todos.length > 0 && (
            <div className="plan-progress" aria-hidden="true">
              <span style={{ width: `${(done / todos.length) * 100}%` }} />
            </div>
          )}
          <ol className="todos">
            {todos.map((todo, index) => (
              <li key={index} className={["todo", todo.status, struck.has(todo.text) && "fresh"].filter(Boolean).join(" ")}>
                <span className={`row-seal ${todo.status === "pending" ? "" : TODO_STATES[todo.status]}`} aria-hidden="true">
                  {TODO_SEALS[todo.status]}
                </span>
                <span className="sr-only">{todo.status === "done" ? "已办：" : todo.status === "in_progress" ? "正在办：" : "待办："}</span>
                {/* The words in a line of their own inside the cell, so that a stroke through them is as long as they are. */}
                <span className="text">
                  <span className="words">{todo.text}</span>
                </span>
              </li>
            ))}
          </ol>
        </div>
      ) : tab === "files" ? (
        <div className="panel-body">
          <div className="panel-head">
            <h2>分身动过的文件</h2>
            {files.length > 0 && (
              <span className="panel-meta">
                {files.length} 个，<span className="in">+{files.reduce((sum, file) => sum + file.added, 0)}</span> <span className="out">−{files.reduce((sum, file) => sum + file.removed, 0)}</span>
              </span>
            )}
          </div>
          {files.length === 0 && <p className="muted">分身还没有改过文件。</p>}
          <div className="file-list">
            {files.map((file) => (
              <FileRow key={file.path} file={file} />
            ))}
          </div>
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

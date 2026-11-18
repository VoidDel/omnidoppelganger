import { useEffect, useRef, useState } from "react";
import { type ChangedFile, type ToolItem, type Usage, shortCount } from "../lib/fold";
import { MODES, modeOf, nextMode } from "../lib/modes";
import type { Mode, SessionItem, Status, Todo } from "../lib/types";

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
  inSettings: boolean;
  onOpen: (id: string) => void;
  onNew: () => void;
  onSettings: () => void;
  /** Ids of the sessions that hold the text, in their title, folder, or conversation. */
  onSearch: (query: string) => Promise<string[]>;
}

export function Sidebar({ sessions, activeId, inSettings, onOpen, onNew, onSettings, onSearch }: SidebarProps) {
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
  const groups = new Map<string, SessionItem[]>();
  for (const session of shown) groups.set(session.workspace, [...(groups.get(session.workspace) ?? []), session]);
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
        化出分身
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
            <div className="group-name" title={workspace}>
              {baseName(workspace)}
            </div>
            {items.map((session) => (
              <button
                type="button"
                key={session.id}
                className="row-button"
                aria-current={!inSettings && session.id === activeId ? "true" : undefined}
                onClick={() => onOpen(session.id)}
              >
                <span className="row-title">{session.title || "新分身"}</span>
                {session.status === "waiting_approval" && <span className="chip await">候旨</span>}
                {session.status === "running" && <span className="chip busy">运功中</span>}
              </button>
            ))}
          </div>
        ))}
      </div>
      <button type="button" className="row-button" aria-current={inSettings ? "page" : undefined} onClick={onSettings}>
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
      <button type="button" className="select-button" aria-haspopup="menu" aria-expanded={open} title="Shift+Tab 切换" onClick={() => setOpen(!open)}>
        <ModeIcon mode={mode} />
        <span className="label">禁制</span>
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
  onRename: (title: string) => void;
  onDelete: () => void;
}

/** The session's title and folder, with renaming and deleting. Deleting asks once more: it cannot be undone. */
export function SessionHeader({ session, onRename, onDelete }: SessionHeaderProps) {
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
      <span className="path" title={session.workspace}>
        {session.workspace}
      </span>
      <span className="spacer" />
      {deleting ? (
        <span className="undo-confirm">
          这个分身的全部记录和改文件前存的原样都会删掉，找不回来。
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
            <button type="button" className="button text small" onClick={() => setDraft(session.title)}>
              改名
            </button>
            <button type="button" className="button text small" disabled={busy} title={busy ? "分身还在运功，先收回它" : undefined} onClick={() => setDeleting(true)}>
              删除
            </button>
          </>
        )
      )}
    </header>
  );
}

interface ComposerProps {
  status: Status;
  model: string;
  mode: Mode;
  usage: Usage;
  /** The number of tokens at which the conversation is compacted. */
  contextLimit: number;
  /** The last turn in plan mode ended with the 分身's answer: offer to carry it out. */
  planReady: boolean;
  onMode: (mode: Mode) => void;
  /** Switch to `mode`, then tell the 分身 to carry out its plan. */
  onCarryOut: (mode: Mode) => void;
  onSend: (text: string) => void;
  onStop: () => void;
}

export function Composer({ status, model, mode, usage, contextLimit, planReady, onMode, onCarryOut, onSend, onStop }: ComposerProps) {
  const [text, setText] = useState("");
  const busy = status !== "idle";
  const submit = () => {
    if (!text.trim()) return;
    onSend(text);
    setText("");
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
        <label htmlFor="composer-input" className="sr-only">
          给分身的法旨
        </label>
        <textarea
          id="composer-input"
          rows={2}
          value={text}
          placeholder={busy ? "给分身传一句音" : "吩咐一件事，回车传令"}
          onChange={(event) => setText(event.target.value)}
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
          <span className="select-button">
            <span className="label">功法</span>
            <span className="value">{model}</span>
          </span>
          <ModePicker mode={mode} onMode={onMode} />
          <span
            className="select-button"
            title={`这个会话累计用掉：输入 ${usage.spent.input}（其中 ${usage.spent.cached} 读自缓存），输出 ${usage.spent.output}。眼下的对话到 ${contextLimit} token 时，分身会把前情凝成摘要再继续。`}
          >
            <span className="label">灵石</span>
            <span className="value">
              {shortCount(usage.context)} / {shortCount(contextLimit)}
            </span>
          </span>
          <span className="spacer" />
          {busy && (
            <button type="button" className="button outline" onClick={onStop}>
              收回分身
            </button>
          )}
          <button type="button" className="button primary" disabled={!text.trim()} onClick={submit}>
            {busy ? "传音" : "传令"}
          </button>
        </div>
      </div>
    </div>
  );
}

const TODO_MARKS: Record<Todo["status"], string> = { pending: "○", in_progress: "▶", done: "✓" };

export function SidePanel({ files, runs, todos }: { files: ChangedFile[]; runs: ToolItem[]; todos: Todo[] }) {
  const [tab, setTab] = useState<"files" | "terminal" | "plan">("files");
  const done = todos.filter((todo) => todo.status === "done").length;
  return (
    <aside className="panel" aria-label="改动与终端">
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

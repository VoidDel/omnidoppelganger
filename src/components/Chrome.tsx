import { useState } from "react";
import type { ChangedFile, ToolItem } from "../lib/fold";
import type { SessionItem, Status } from "../lib/types";

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

/** Two sheets: aligned when idle, sliding apart while the 分身 works, cinnabar underneath while it waits for a decision. */
export function Mark({ state = "idle", size = 26 }: { state?: "idle" | "working" | "waiting"; size?: number }) {
  const under = state === "waiting" ? { x: 8, y: 8, fill: "var(--cinnabar)" } : { x: 6, y: 6, fill: "var(--talisman-under)" };
  return (
    <svg className={`mark ${state}`} aria-hidden="true" width={size} height={size} viewBox="0 0 26 26">
      <rect className="under" x={under.x} y={under.y} width="16" height="16" rx="2" fill={under.fill} />
      <rect x="2.75" y="2.75" width="16" height="16" rx="2" fill="var(--mark-front)" stroke="var(--ink)" strokeWidth="1.5" />
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
}

export function Sidebar({ sessions, activeId, inSettings, onOpen, onNew, onSettings }: SidebarProps) {
  const groups = new Map<string, SessionItem[]>();
  for (const session of sessions) groups.set(session.workspace, [...(groups.get(session.workspace) ?? []), session]);
  return (
    <nav className="sidebar" aria-label="会话">
      <div className="brand">
        <Mark />
        <span>OmniDoppelganger</span>
      </div>
      <button type="button" className="button primary" onClick={onNew}>
        <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
          <path d="M8 3v10M3 8h10" />
        </svg>
        召出分身
      </button>
      <div className="sidebar-scroll">
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

interface ComposerProps {
  status: Status;
  model: string;
  onSend: (text: string) => void;
  onStop: () => void;
}

export function Composer({ status, model, onSend, onStop }: ComposerProps) {
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
          }}
        />
        <div className="composer-row">
          <span className="select-button">
            <span className="label">功法</span>
            <span className="value">{model}</span>
          </span>
          <span className="select-button">
            <span className="label">禁制</span>
            <span className="value">只动洞府之内</span>
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

export function SidePanel({ files, runs }: { files: ChangedFile[]; runs: ToolItem[] }) {
  const [tab, setTab] = useState<"files" | "terminal">("files");
  return (
    <aside className="panel" aria-label="改动与终端">
      <div className="tabs">
        <button type="button" className="tab" aria-pressed={tab === "files"} onClick={() => setTab("files")}>
          改动
        </button>
        <button type="button" className="tab" aria-pressed={tab === "terminal"} onClick={() => setTab("terminal")}>
          终端
        </button>
      </div>
      {tab === "files" ? (
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
                {"\n"}
                {meta.output}
                {meta.stopped ? <span className="failed">{`\n[${meta.stopped === "timed out" ? "超时停止" : "已停止"}]`}</span> : null}
                {!meta.stopped && meta.exit_code !== 0 ? <span className="failed">{`\n[退出码 ${meta.exit_code ?? "未知"}]`}</span> : null}
              </pre>
            );
          })}
        </div>
      )}
    </aside>
  );
}

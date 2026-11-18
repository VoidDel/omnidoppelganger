import { useState } from "react";
import type { ChangedFile, ToolItem } from "../lib/fold";
import type { SessionItem, Status } from "../lib/types";

/** Two sheets of paper: aligned when idle, sliding apart while the agent works, pink underneath while it waits. */
export function Mark({ state = "idle", size = 26 }: { state?: "idle" | "working" | "waiting"; size?: number }) {
  const under = state === "waiting" ? { x: 8, y: 8, fill: "var(--pink-under)" } : { x: 6, y: 6, fill: "var(--canary-under)" };
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
        新建会话
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
                <span className="row-title">{session.title || "新会话"}</span>
                {session.status === "waiting_approval" && <span className="chip pink">待签</span>}
                {session.status === "running" && <span className="chip canary">进行中</span>}
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
            <span>分身在干活。现在发消息会插进当前步骤，不会打断它。</span>
          </>
        )}
        {status === "waiting_approval" && (
          <>
            <Mark state="waiting" size={18} />
            <span>分身停在这里，等你签字后继续。</span>
          </>
        )}
      </div>
      <div className="composer-box">
        <label htmlFor="composer-input" className="sr-only">
          给分身的指令
        </label>
        <textarea
          id="composer-input"
          rows={2}
          value={text}
          placeholder={busy ? "插一句话给分身" : "交代一件事，回车发送"}
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
            <span className="label">模型</span>
            <span className="value">{model}</span>
          </span>
          <span className="select-button">
            <span className="label">权限</span>
            <span className="value">只能改工作区</span>
          </span>
          <span className="spacer" />
          {busy && (
            <button type="button" className="button outline" onClick={onStop}>
              停止分身
            </button>
          )}
          <button type="button" className="button primary" disabled={!text.trim()} onClick={submit}>
            {busy ? "插话" : "发送"}
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
          <h2>这次改过的文件</h2>
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

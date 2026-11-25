import "@xterm/xterm/css/xterm.css";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { type ITheme, Terminal } from "@xterm/xterm";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import type { ToolItem } from "../lib/fold";
import type { PtyInfo } from "../lib/types";

export interface TerminalPaneProps {
  sessionId: string;
  runs: ToolItem[];
  onSendToChat: (quote: string) => void;
  onError: (message: string) => void;
}

/**
 * The light palette is chosen against its own paper (#fbfcfa): every one of the sixteen colours reads at a WCAG
 * contrast of at least 5.9:1. "White" and "bright white" are greys of ink here, since on paper a program's white
 * text would otherwise vanish; the bright colours are deeper rather than paler, for emphasis that still reads.
 */
const LIGHT_THEME: ITheme = {
  background: "#fbfcfa",
  foreground: "#1b2a2b",
  cursor: "#1f6f64",
  cursorAccent: "#fbfcfa",
  selectionBackground: "rgba(31, 111, 100, 0.22)",
  black: "#1b2a2b",
  red: "#b3261e",
  green: "#1d6b3a",
  yellow: "#865600",
  blue: "#1f5a96",
  magenta: "#863c8a",
  cyan: "#0b6672",
  white: "#54656a",
  brightBlack: "#5f6f72",
  brightRed: "#a8301f",
  brightGreen: "#226f41",
  brightYellow: "#7a5200",
  brightBlue: "#245e9c",
  brightMagenta: "#8a3f8e",
  brightCyan: "#0f6874",
  brightWhite: "#3f4f53",
};

/** On paper, colours a program names outright (256 colours, true colour) are darkened by xterm until they read. */
const LIGHT_MIN_CONTRAST = 4.5;

const DARK_THEME: ITheme = {
  background: "#0f1a1c",
  foreground: "#e6eeea",
  cursor: "#6fd0be",
  cursorAccent: "#0f1a1c",
  selectionBackground: "rgba(111, 208, 190, 0.26)",
  black: "#162426",
  red: "#cf4a40",
  green: "#6fd0be",
  yellow: "#e0b85a",
  blue: "#76b2e8",
  magenta: "#cf8fd4",
  cyan: "#6dcad4",
  white: "#cad8d3",
  brightBlack: "#6d8480",
  brightRed: "#eb6c63",
  brightGreen: "#8de0d0",
  brightYellow: "#f0dc94",
  brightBlue: "#95c7f5",
  brightMagenta: "#e2ace6",
  brightCyan: "#90dfe8",
  brightWhite: "#ffffff",
};

function isDark(): boolean {
  return document.documentElement.dataset.theme === "dark";
}

function currentXtermTheme(): ITheme {
  return isDark() ? DARK_THEME : LIGHT_THEME;
}

/** 1 leaves colours as programs give them, which the dark palette is drawn for. */
function currentMinContrast(): number {
  return isDark() ? 1 : LIGHT_MIN_CONTRAST;
}

interface MountedTerm {
  term: Terminal;
  fit: FitAddon;
  host: HTMLDivElement;
}

export default function TerminalPane({ sessionId, runs, onSendToChat, onError }: TerminalPaneProps) {
  const [viewMode, setViewMode] = useState<"pty" | "agent">("pty");
  const [ptys, setPtys] = useState<PtyInfo[]>([]);
  const [activePtyId, setActivePtyId] = useState<string | null>(null);
  const [selectedText, setSelectedText] = useState<string>("");
  const [spawning, setSpawning] = useState<boolean>(false);

  const stageRef = useRef<HTMLDivElement>(null);
  const mountedRef = useRef<Map<string, MountedTerm>>(new Map());
  const activePtyRef = useRef<string | null>(null);
  activePtyRef.current = activePtyId;

  /** What a shell printed before its terminal was on screen: it starts printing as soon as it is spawned. */
  const pendingRef = useRef<Map<string, string>>(new Map());

  /** A terminal in a host of its own on the stage, not yet joined to any shell. */
  const openTerminal = useCallback((visible: boolean): MountedTerm | null => {
    const stage = stageRef.current;
    if (!stage) return null;
    const host = document.createElement("div");
    host.className = "xterm-instance";
    host.style.display = visible ? "block" : "none";
    stage.appendChild(host);

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      lineHeight: 1.35,
      fontFamily: '"JetBrains Mono", "Noto Sans SC", "PingFang SC", "Microsoft YaHei UI", ui-monospace, monospace',
      scrollback: 5000,
      allowProposedApi: false,
      theme: currentXtermTheme(),
      minimumContrastRatio: currentMinContrast(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(
      new WebLinksAddon((_event, uri) => {
        api.openExternal(uri).catch(() => undefined);
      }),
    );
    term.open(host);
    return { term, fit, host };
  }, []);

  /** Join a terminal to its shell: keys go to the shell, and what it printed so far is shown. */
  const attachTerminal = useCallback((info: PtyInfo, mounted: MountedTerm, earlier: string) => {
    const { term } = mounted;
    if (earlier) term.write(earlier);
    if (info.exited !== null) {
      term.write(`\r\n\x1b[90m[进程已结束，退出码 ${info.exited}]\x1b[0m\r\n`);
    }
    term.onData((data) => {
      api.ptyWrite(info.id, data).catch(() => undefined);
    });
    term.onSelectionChange(() => {
      if (activePtyRef.current === info.id) {
        setSelectedText(term.getSelection().trim());
      }
    });
    mountedRef.current.set(info.id, mounted);
  }, []);

  /** A terminal for a shell that was already running: its backlog says what it printed, so nothing waiting is added. */
  const ensureTerminalInstance = useCallback(
    (info: PtyInfo) => {
      const existing = mountedRef.current.get(info.id);
      if (existing) return existing;
      const mounted = openTerminal(activePtyRef.current === info.id);
      if (!mounted) return null;
      pendingRef.current.delete(info.id);
      attachTerminal(info, mounted, info.backlog);
      return mounted;
    },
    [openTerminal, attachTerminal],
  );

  /**
   * A new shell, started at the size its terminal has on screen. A shell started at a guessed size draws for that
   * width: zsh's mark for an unfinished line (a lone `%` and a row of spaces) then wrapped onto a line of its own
   * once the terminal was fitted, and stayed above the prompt.
   */
  const createPty = useCallback(async () => {
    if (spawning) return;
    setSpawning(true);
    // The new terminal is measured alone on the stage: the others step aside while it is.
    for (const [, inst] of mountedRef.current) inst.host.style.display = "none";
    const mounted = openTerminal(true);
    let cols = 80;
    let rows = 24;
    if (mounted && mounted.host.clientWidth > 0 && mounted.host.clientHeight > 0) {
      try {
        mounted.fit.fit();
        cols = mounted.term.cols;
        rows = mounted.term.rows;
      } catch {
        // Not laid out yet: the shell starts at the usual size, and is resized when the terminal is fitted.
      }
    }
    try {
      const info = await api.ptySpawn(sessionId, cols, rows);
      const earlier = pendingRef.current.get(info.id) ?? "";
      pendingRef.current.delete(info.id);
      if (mounted) attachTerminal(info, mounted, earlier);
      setPtys((prev) => [...prev, info]);
      setActivePtyId(info.id);
      setViewMode("pty");
      if (mounted) mounted.term.focus();
      else requestAnimationFrame(() => ensureTerminalInstance(info)?.term.focus());
    } catch (err) {
      mounted?.term.dispose();
      mounted?.host.remove();
      // The terminal that was on screen comes back.
      const back = activePtyRef.current ? mountedRef.current.get(activePtyRef.current) : undefined;
      if (back) back.host.style.display = "block";
      onError(String(err));
    } finally {
      setSpawning(false);
    }
  }, [sessionId, spawning, openTerminal, attachTerminal, ensureTerminalInstance, onError]);

  // Load existing PTYs for this session (or auto-spawn the first one) on mount / session switch.
  useEffect(() => {
    // Dispose DOM terminals from previous sessionId
    for (const [, inst] of mountedRef.current) {
      inst.term.dispose();
      inst.host.remove();
    }
    mountedRef.current.clear();
    pendingRef.current.clear();
    setSelectedText("");

    let alive = true;
    api.ptyList(sessionId).then(
      (list) => {
        if (!alive) return;
        if (list.length > 0) {
          setPtys(list);
          const firstId = list[list.length - 1].id;
          setActivePtyId(firstId);
          requestAnimationFrame(() => {
            if (!alive) return;
            for (const item of list) {
              ensureTerminalInstance(item);
            }
            const cur = mountedRef.current.get(firstId);
            if (cur) {
              cur.host.style.display = "block";
              try {
                cur.fit.fit();
                api.ptyResize(firstId, cur.term.cols, cur.term.rows).catch(() => undefined);
              } catch {
                // Ignore fit error
              }
            }
          });
        } else {
          setPtys([]);
          setActivePtyId(null);
          void createPty();
        }
      },
      (err) => alive && onError(String(err)),
    );

    return () => {
      alive = false;
      for (const [, inst] of mountedRef.current) {
        inst.term.dispose();
        inst.host.remove();
      }
      mountedRef.current.clear();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  // Subscribe to PTY output and exit events.
  useEffect(() => {
    const stopOut = api.onPtyOutput((payload) => {
      if (payload.session_id !== sessionId) return;
      const inst = mountedRef.current.get(payload.id);
      if (inst) {
        inst.term.write(payload.data);
        return;
      }
      // A shell whose terminal is not joined yet: kept until it is, at most as much as the backlog holds.
      const kept = (pendingRef.current.get(payload.id) ?? "") + payload.data;
      pendingRef.current.set(payload.id, kept.length > 131072 ? kept.slice(-131072) : kept);
    });
    const stopExit = api.onPtyExit((payload) => {
      if (payload.session_id !== sessionId) return;
      const code = payload.exit_code ?? 0;
      setPtys((prev) => prev.map((p) => (p.id === payload.id ? { ...p, exited: code } : p)));
      const inst = mountedRef.current.get(payload.id);
      inst?.term.write(`\r\n\x1b[90m[进程已结束，退出码 ${code}]\x1b[0m\r\n`);
    });
    return () => {
      stopOut.then((fn) => fn()).catch(() => undefined);
      stopExit.then((fn) => fn()).catch(() => undefined);
    };
  }, [sessionId]);

  // Sync visibility and fit active terminal whenever activePtyId or viewMode changes.
  useEffect(() => {
    for (const [id, inst] of mountedRef.current) {
      const visible = viewMode === "pty" && id === activePtyId;
      inst.host.style.display = visible ? "block" : "none";
      if (visible) {
        setSelectedText(inst.term.getSelection().trim());
        requestAnimationFrame(() => {
          if (inst.host.clientWidth > 0 && inst.host.clientHeight > 0) {
            try {
              inst.fit.fit();
              api.ptyResize(id, inst.term.cols, inst.term.rows).catch(() => undefined);
              inst.term.focus();
            } catch {
              // Ignore
            }
          }
        });
      }
    }
  }, [activePtyId, viewMode, ptys.length]);

  // ResizeObserver on stage container so dragging SidePanel width updates PTY cols/rows.
  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const curId = activePtyRef.current;
        if (!curId) return;
        const inst = mountedRef.current.get(curId);
        if (!inst || inst.host.style.display === "none") return;
        if (stage.clientWidth > 0 && stage.clientHeight > 0) {
          try {
            inst.fit.fit();
            api.ptyResize(curId, inst.term.cols, inst.term.rows).catch(() => undefined);
          } catch {
            // Ignore
          }
        }
      });
    });
    observer.observe(stage);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, []);

  // Live light/dark theme sync with `<html data-theme="...">`.
  useEffect(() => {
    const observer = new MutationObserver(() => {
      const theme = currentXtermTheme();
      const contrast = currentMinContrast();
      for (const [, inst] of mountedRef.current) {
        inst.term.options.theme = theme;
        inst.term.options.minimumContrastRatio = contrast;
      }
    });
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);

  const closePty = (id: string) => {
    api.ptyClose(id).catch(() => undefined);
    const inst = mountedRef.current.get(id);
    if (inst) {
      inst.term.dispose();
      inst.host.remove();
      mountedRef.current.delete(id);
    }
    setPtys((prev) => {
      const idx = prev.findIndex((p) => p.id === id);
      const next = prev.filter((p) => p.id !== id);
      if (activePtyId === id) {
        setActivePtyId(next[Math.min(idx, next.length - 1)]?.id ?? null);
      }
      return next;
    });
  };

  const clearActive = () => {
    if (!activePtyId) return;
    const inst = mountedRef.current.get(activePtyId);
    inst?.term.clear();
    api.ptyClear(activePtyId).catch(() => undefined);
  };

  return (
    <div className="term-pane">
      <div className="term-bar">
        <div className="term-tabs" role="tablist" aria-label="终端实例">
          {ptys.map((p) => {
            const active = viewMode === "pty" && p.id === activePtyId;
            return (
              <div key={p.id} className={`term-tab ${active ? "active" : ""} ${p.exited !== null ? "exited" : ""}`}>
                <button
                  type="button"
                  role="tab"
                  aria-selected={active}
                  className="term-tab-btn"
                  title={p.cwd}
                  onClick={() => {
                    setViewMode("pty");
                    setActivePtyId(p.id);
                  }}
                >
                  <span>{p.title}</span>
                </button>
                <button
                  type="button"
                  className="term-tab-close"
                  title="关闭此终端"
                  aria-label={`关闭 ${p.title}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    closePty(p.id);
                  }}
                >
                  ×
                </button>
              </div>
            );
          })}
          <button
            type="button"
            className="icon-button small"
            disabled={spawning}
            title="新建交互式终端"
            onClick={() => void createPty()}
          >
            +
          </button>
        </div>

        <div className="term-acts">
          {viewMode === "pty" && selectedText && (
            <button
              type="button"
              className="button primary small"
              title="将终端选中的内容发到对话输入框"
              onClick={() => onSendToChat(`终端输出如下，请帮我看看：\n\`\`\`text\n${selectedText.slice(0, 6000)}\n\`\`\``)}
            >
              发给分身
            </button>
          )}
          {viewMode === "pty" && activePtyId && (
            <button type="button" className="button text small" title="清屏" onClick={clearActive}>
              清屏
            </button>
          )}
          <button
            type="button"
            className={`term-log-switch ${viewMode === "agent" ? "active" : ""}`}
            aria-pressed={viewMode === "agent"}
            title="查看分身执行过的命令记录"
            onClick={() => setViewMode((m) => (m === "agent" ? "pty" : "agent"))}
          >
            分身命令{runs.length > 0 ? ` (${runs.length})` : ""}
          </button>
        </div>
      </div>

      <div
        ref={stageRef}
        className="term-stage"
        style={{ display: viewMode === "pty" ? "flex" : "none" }}
      >
        {ptys.length === 0 && !spawning && (
          <div className="viewer-placeholder term-empty">
            <p className="muted">当前没有打开的终端。</p>
            <button type="button" className="button outline small" onClick={() => void createPty()}>
              开启终端
            </button>
          </div>
        )}
      </div>

      {viewMode === "agent" && (
        <div className="panel-body term-runs">
          {runs.length === 0 && <p className="muted">分身还没有运行过命令。</p>}
          {runs.map((run) => {
            const meta = run.result!.meta!;
            const cmdLine = `${meta.cwd ? `[${meta.cwd}] ` : ""}${meta.command ?? ""}`.replace(/\s+/g, " ").trim();
            const failed = Boolean(meta.stopped) || (!meta.background && !meta.running && meta.exit_code !== 0);
            const outputLines = meta.output ? meta.output.replace(/\n$/, "").split("\n").length : 0;
            const badge = meta.stopped
              ? meta.stopped === "timed out"
                ? "超时"
                : "已停止"
              : run.name === "run_command" && meta.background
                ? `后台 #${meta.job}`
                : meta.running
                  ? "运行中"
                  : meta.exit_code !== 0
                    ? `退出 ${meta.exit_code ?? "?"}`
                    : outputLines > 0
                      ? `${outputLines} 行`
                      : "成功";
            return (
              <details className="term-run" key={run.id}>
                <summary className="term-run-head" title={cmdLine}>
                  <span className="term-run-chevron" aria-hidden="true">
                    ▸
                  </span>
                  <span className="prompt">$</span>
                  <span className="term-run-cmd">{cmdLine}</span>
                  <span className={`term-run-status ${failed ? "failed" : "muted"}`}>{badge}</span>
                </summary>
                <pre className="terminal term-run-body">
                  <span className="prompt">$</span> {meta.cwd ? `[${meta.cwd}] ` : ""}
                  {meta.command}
                  {run.name === "run_command" && meta.background ? <span className="muted">{`\n[后台运行，编号 ${meta.job}]`}</span> : null}
                  {run.name !== "run_command" ? <span className="muted">{`  （${meta.job}${run.name === "stop_command" ? "，停止" : ""}）`}</span> : null}
                  {meta.output ? `\n${meta.output}` : ""}
                  {meta.stopped ? <span className="failed">{`\n[${meta.stopped === "timed out" ? "超时停止" : "已停止"}]`}</span> : null}
                  {!meta.stopped && !meta.background && !meta.running && meta.exit_code !== 0 ? (
                    <span className="failed">{`\n[退出码 ${meta.exit_code ?? "未知"}]`}</span>
                  ) : null}
                </pre>
              </details>
            );
          })}
        </div>
      )}
    </div>
  );
}

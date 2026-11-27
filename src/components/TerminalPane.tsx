import "../styles/workbench.css";
import "@xterm/xterm/css/xterm.css";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { type ITheme, Terminal } from "@xterm/xterm";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import type { PtyInfo, PtyOutputPayload } from "../lib/types";
import type { QuotePayload } from "./Composer";

export interface TerminalPaneProps {
  sessionId: string;
  onSendToChat: (quote: QuotePayload) => void;
  onError: (message: string) => void;
}

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

function currentMinContrast(): number {
  return isDark() ? 1 : LIGHT_MIN_CONTRAST;
}

const utf8Encoder = new TextEncoder();
const utf8Decoder = new TextDecoder();

/**
 * Filter and slice buffered `PtyOutputPayload` chunks that arrived while `ptyList`/`ptySpawn` was in flight,
 * deduplicating any bytes already covered by `info.backlog_end_offset`.
 */
export function filterIncrementalPtyOutput(
  info: Pick<PtyInfo, "backlog" | "backlog_end_offset">,
  chunks: PtyOutputPayload[],
  isFreshSpawn = false,
): string {
  if (chunks.length === 0) return "";
  const backlogEnd = info.backlog_end_offset ?? 0;
  let cursor = backlogEnd;
  let out = "";

  for (const chunk of chunks) {
    if (chunk.end_offset !== undefined) {
      if (chunk.end_offset <= cursor) continue;
      const chunkBytes = utf8Encoder.encode(chunk.data);
      const start = chunk.start_offset ?? chunk.end_offset - chunkBytes.byteLength;
      if (start < cursor) {
        const skip = cursor - start;
        if (skip < chunkBytes.byteLength) {
          out += utf8Decoder.decode(chunkBytes.subarray(skip));
        }
      } else {
        out += chunk.data;
      }
      cursor = chunk.end_offset;
    } else if (isFreshSpawn && !info.backlog) {
      out += chunk.data;
    }
  }
  return out;
}

interface MountedTerm {
  term: Terminal;
  fit: FitAddon;
  host: HTMLDivElement;
}

const MAX_PENDING_CHUNKS = 512;

export default function TerminalPane({ sessionId, onSendToChat, onError }: TerminalPaneProps) {
  const [ptys, setPtys] = useState<PtyInfo[]>([]);
  const [activePtyId, setActivePtyId] = useState<string | null>(null);
  const [selectedText, setSelectedText] = useState<string>("");
  const [spawning, setSpawning] = useState<boolean>(false);

  const stageRef = useRef<HTMLDivElement>(null);
  const mountedRef = useRef<Map<string, MountedTerm>>(new Map());
  const activePtyRef = useRef<string | null>(null);
  activePtyRef.current = activePtyId;

  const mountedSessionRef = useRef<string>(sessionId);
  mountedSessionRef.current = sessionId;
  const disposedRef = useRef<boolean>(false);

  const pendingRef = useRef<Map<string, PtyOutputPayload[]>>(new Map());
  const spawningRef = useRef<boolean>(false);

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
      fontFamily: '"JetBrains Mono", "PingFang SC", "Microsoft YaHei UI", "Noto Sans CJK SC", ui-monospace, monospace',
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

  const attachTerminal = useCallback((info: PtyInfo, mounted: MountedTerm, initialText: string) => {
    const { term } = mounted;
    if (initialText) term.write(initialText);
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

  const ensureTerminalInstance = useCallback(
    (info: PtyInfo) => {
      const existing = mountedRef.current.get(info.id);
      if (existing) return existing;
      const mounted = openTerminal(activePtyRef.current === info.id);
      if (!mounted) return null;
      const buffered = pendingRef.current.get(info.id) ?? [];
      pendingRef.current.delete(info.id);
      const delta = filterIncrementalPtyOutput(info, buffered, false);
      attachTerminal(info, mounted, info.backlog + delta);
      return mounted;
    },
    [openTerminal, attachTerminal],
  );

  const createPty = useCallback(
    async (targetSessionId = sessionId, autoInit = false) => {
      if (!autoInit && spawningRef.current) return;
      spawningRef.current = true;
      setSpawning(true);
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
          // Use default size before layout
        }
      }
      try {
        const info = await api.ptySpawn(targetSessionId, cols, rows);
        if (disposedRef.current || mountedSessionRef.current !== targetSessionId) {
          mounted?.term.dispose();
          mounted?.host.remove();
          pendingRef.current.delete(info.id);
          if (autoInit) {
            api.ptyClose(info.id).catch(() => undefined);
          }
          return;
        }
        const buffered = pendingRef.current.get(info.id) ?? [];
        pendingRef.current.delete(info.id);
        const delta = filterIncrementalPtyOutput(info, buffered, true);
        if (mounted) attachTerminal(info, mounted, info.backlog + delta);
        setPtys((prev) => (prev.some((p) => p.id === info.id) ? prev : [...prev, info]));
        setActivePtyId(info.id);
        if (mounted) mounted.term.focus();
        else requestAnimationFrame(() => ensureTerminalInstance(info)?.term.focus());
      } catch (err) {
        mounted?.term.dispose();
        mounted?.host.remove();
        if (!disposedRef.current && mountedSessionRef.current === targetSessionId) {
          const back = activePtyRef.current ? mountedRef.current.get(activePtyRef.current) : undefined;
          if (back) back.host.style.display = "block";
          onError(String(err));
        }
      } finally {
        spawningRef.current = false;
        if (!disposedRef.current && mountedSessionRef.current === targetSessionId) {
          setSpawning(false);
        }
      }
    },
    [sessionId, openTerminal, attachTerminal, ensureTerminalInstance, onError],
  );

  // Subscribe to PTY output and exit events BEFORE listing/spawning so T0..T2 chunks are captured in `pendingRef`.
  useEffect(() => {
    const stopOut = api.onPtyOutput((payload) => {
      if (payload.session_id !== mountedSessionRef.current) return;
      const inst = mountedRef.current.get(payload.id);
      if (inst) {
        inst.term.write(payload.data);
        return;
      }
      const list = pendingRef.current.get(payload.id) ?? [];
      list.push(payload);
      if (list.length > MAX_PENDING_CHUNKS) list.splice(0, list.length - MAX_PENDING_CHUNKS);
      pendingRef.current.set(payload.id, list);
    });
    const stopExit = api.onPtyExit((payload) => {
      if (payload.session_id !== mountedSessionRef.current) return;
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

  // Load existing PTYs for this session (or auto-spawn the first one) on mount / session switch.
  useEffect(() => {
    disposedRef.current = false;
    mountedSessionRef.current = sessionId;
    spawningRef.current = false;
    setSpawning(false);

    for (const [, inst] of mountedRef.current) {
      inst.term.dispose();
      inst.host.remove();
    }
    mountedRef.current.clear();
    pendingRef.current.clear();
    setSelectedText("");
    setPtys([]);
    setActivePtyId(null);

    const targetSession = sessionId;
    let alive = true;
    api.ptyList(targetSession).then(
      (list) => {
        if (!alive || disposedRef.current || mountedSessionRef.current !== targetSession) return;
        if (list.length > 0) {
          setPtys(list);
          const firstId = list[list.length - 1].id;
          activePtyRef.current = firstId;
          setActivePtyId(firstId);
          for (const item of list) {
            ensureTerminalInstance(item);
          }
          requestAnimationFrame(() => {
            if (!alive || disposedRef.current || mountedSessionRef.current !== targetSession) return;
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
          void createPty(targetSession, true);
        }
      },
      (err) => {
        if (alive && !disposedRef.current && mountedSessionRef.current === targetSession) {
          onError(String(err));
        }
      },
    );

    return () => {
      alive = false;
      disposedRef.current = true;
      for (const [, inst] of mountedRef.current) {
        inst.term.dispose();
        inst.host.remove();
      }
      mountedRef.current.clear();
    };
  }, [sessionId, createPty, ensureTerminalInstance, onError]);

  useEffect(() => {
    for (const [id, inst] of mountedRef.current) {
      const visible = id === activePtyId;
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
  }, [activePtyId, ptys.length]);

  useEffect(() => {
    const stage = stageRef.current;
    if (!stage || typeof ResizeObserver === "undefined") return;
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

  useEffect(() => {
    const observer = new MutationObserver(() => {
      const theme = currentXtermTheme();
      const contrast = currentMinContrast();
      for (const [, inst] of mountedRef.current) {
        inst.term.options.theme = theme;
        inst.term.options.minimumContrastRatio = contrast;
      }
    });
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "data-palette"] });
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
            const active = p.id === activePtyId;
            return (
              <div key={p.id} className={`term-tab ${active ? "active" : ""} ${p.exited !== null ? "exited" : ""}`}>
                <button
                  type="button"
                  role="tab"
                  aria-selected={active}
                  className="term-tab-btn"
                  title={p.cwd}
                  onClick={() => setActivePtyId(p.id)}
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
          {selectedText && (
            <button
              type="button"
              className="button primary small"
              title="将终端选中的内容发到对话输入框"
              onClick={() => onSendToChat({ kind: "terminal", content: selectedText.slice(0, 6000) })}
            >
              发给分身
            </button>
          )}
          {activePtyId && (
            <button type="button" className="button text small" title="清屏" onClick={clearActive}>
              清屏
            </button>
          )}
        </div>
      </div>

      <div ref={stageRef} className="term-stage" style={{ display: "flex" }}>
        {ptys.length === 0 && !spawning && (
          <div className="viewer-placeholder term-empty">
            <p className="muted">当前没有打开的终端。</p>
            <button type="button" className="button outline small" onClick={() => void createPty()}>
              开启终端
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

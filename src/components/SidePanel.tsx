import "../styles/panel.css";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { type ChangedFile, type Product, type ProductVersion, type ToolItem, fileSize } from "../lib/fold";
import { useMenuFocus } from "../lib/menuFocus";
import type { RestoredArtifact, Todo } from "../lib/types";
import { Light } from "./Light";
import { Icon } from "./Icon";
import { Diff } from "./Markdown";

/** The seal on a step of the plan: nothing yet for one to come, 办 for the one in hand, 毕 for one done. */
const TODO_SEALS: Record<Todo["status"], string> = { pending: "", in_progress: "办", done: "毕" };

/** The panel's width while there is nothing in it: a strip that holds its tabs. */
export const PANEL_STRIP = 56;

export type PanelTab = "products" | "jobs" | "terminal" | "files" | "plan" | "chat";

interface TabDef {
  id: PanelTab;
  name: string;
  desc: string;
  title: string;
  icon: ReactNode;
}

const TAB_DEFS: Record<PanelTab, TabDef> = {
  products: {
    id: "products",
    name: "交付物",
    desc: "分身做出来的文档、图片等文件",
    title: "交付物：分身做出来的文档、图片等文件，交付的和这一轮留在洞府里的",
    icon: <Icon name="box-seam" size={15} />,
  },
  jobs: {
    id: "jobs",
    name: "分身任务",
    desc: "后台进程实时日志、停止与命令记录",
    title: "分身任务与后台：实时查看与管理后台进程，回看分身跑过的命令",
    icon: <Icon name="activity" size={15} />,
  },
  terminal: {
    id: "terminal",
    name: "用户终端",
    desc: "本机交互式终端 (Ctrl/Cmd+J)",
    title: "用户终端：打开本机交互式终端 (Ctrl/Cmd+J)",
    icon: <Icon name="terminal" size={15} />,
  },
  files: {
    id: "files",
    name: "改动",
    desc: "本会话累计修改过的文件",
    title: "改动：当前分身在本会话中累计修改过的文件（仓库未提交变更见左栏 Git）",
    icon: <Icon name="file-earmark-diff" size={15} />,
  },
  plan: {
    id: "plan",
    name: "计划",
    desc: "分身列的步骤与完成进度",
    title: "计划：分身列的步骤，做完一步勾一步",
    icon: <Icon name="list-check" size={15} />,
  },
  chat: {
    id: "chat",
    name: "旁聊",
    desc: "不打断主分身的侧栏只读对话",
    title: "旁聊：不打断主分身，在侧栏另开只读对话问进展、查代码",
    icon: <Icon name="chat-left-text" size={15} />,
  },
};

const ADDABLE_ORDER: PanelTab[] = ["jobs", "terminal", "files", "plan", "chat"];

const VERSION_HOW: Record<ProductVersion["how"], string> = {
  painted: "作画",
  delivered: "交付",
  written: "写入",
  found: "留下",
  backup: "恢复前备份",
};

const shortTime = (ts?: string) => (ts ? new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "");

/** A file the 分身 made for 本尊, with expandable version history when revised across steps or turns. */
function ProductRow({
  product,
  workspace,
  working,
  frozen,
  onOpenFile,
  onRestoreArtifact,
  onSaveArtifactCopy,
}: {
  product: Product;
  workspace?: string;
  working: boolean;
  frozen?: boolean;
  onOpenFile?: (path: string, reveal: boolean, snapshot?: string, version?: number) => Promise<string>;
  onRestoreArtifact?: (path: string, snapshot: string, version: number) => Promise<RestoredArtifact>;
  onSaveArtifactCopy?: (path: string, snapshot: string, version: number) => Promise<string>;
}) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ text: string; failed: boolean } | null>(null);
  // Where it is, from the workspace when it is in it: the folders that tell one file from another, not the drive's.
  const root = workspace?.replace(/[\\/]+$/, "");
  const inside = !!root && product.path.length > root.length + 1 && product.path.slice(0, root.length).toLowerCase() === root.toLowerCase() && /[\\/]/.test(product.path[root.length]);
  const shown = inside ? product.path.slice(root!.length + 1) : product.path;
  const cut = Math.max(shown.lastIndexOf("/"), shown.lastIndexOf("\\")) + 1;
  const name = shown.slice(cut);
  const hasHistory = product.versions.length > 1 || product.versions.some((version) => !!version.snapshot);
  const expandable = product.versions.length > 1;
  const openIt = (reveal: boolean, snapshot?: string, version?: number) => {
    if (!onOpenFile) return;
    setNote(null);
    onOpenFile(product.path, reveal, snapshot, version).then(
      (done) => done === "revealed" && !reveal && setNote({ text: "这是可执行文件，不从这里直接运行，已在文件夹中显示。", failed: false }),
      (error) => setNote({ text: String(error), failed: true }),
    );
  };
  const saveCopy = (version: ProductVersion) => {
    if (!version.snapshot || !onSaveArtifactCopy || busy) return;
    setBusy(true);
    setNote(null);
    onSaveArtifactCopy(product.path, version.snapshot, version.version)
      .then(
        (saved) => setNote({ text: `已另存副本：${saved}`, failed: false }),
        (error) => setNote({ text: String(error), failed: true }),
      )
      .finally(() => setBusy(false));
  };
  const restoreIt = (version: ProductVersion) => {
    if (!version.snapshot || !onRestoreArtifact || busy || working || frozen) return;
    setBusy(true);
    setNote(null);
    onRestoreArtifact(product.path, version.snapshot, version.version)
      .then(
        (done) => setNote({ text: `已恢复到 v${done.version}${done.backed_up ? "（原文件改动已留存备份）" : ""}`, failed: false }),
        (error) => setNote({ text: String(error), failed: true }),
      )
      .finally(() => setBusy(false));
  };
  const latest = product.versions[product.versions.length - 1];
  const badge =
    product.how === "painted" ? (
      <span className="chip done" title="分身请画师画的">
        作画
      </span>
    ) : product.how === "delivered" ? (
      <span className="chip done" title="分身做好之后交付的">
        交付
      </span>
    ) : product.how === "written" ? (
      <span className="chip kin" title="分身写出的文档或图表">
        文档
      </span>
    ) : (
      <span className="chip kin" title={`第 ${product.turn} 轮结束时在洞府里找到的：分身用脚本或命令做出来，没有说交付`}>
        留下
      </span>
    );
  return (
    <div className={`file-row product-row ${open ? "open" : ""}`}>
      <div className="file-line">
        <button
          type="button"
          className="file-head"
          aria-expanded={expandable ? open : undefined}
          disabled={!expandable && !onOpenFile}
          title={expandable ? `${product.path}\n共 ${product.versions.length} 个版本，点开查看或恢复历史版本` : `${product.path}\n用系统默认的程序打开`}
          onClick={() => (expandable ? setOpen(!open) : openIt(false))}
        >
          <span className="file-where">
            <span className="file-name">{name}</span>
            {cut > 0 && <span className="file-dir">{shown.slice(0, cut - 1)}</span>}
          </span>
          {product.versions.length > 1 && (
            <span className="chip kin" title={`已留存 ${product.versions.length} 个版本`}>
              {product.versions.length} 版
            </span>
          )}
          {badge}
        </button>
        {onOpenFile && (
          <span className="file-acts">
            <button type="button" className="icon-button" title="用系统默认的程序打开（最新版）" onClick={() => openIt(false)}>
              <Icon name="box-arrow-up-right" size={14} />
              <span className="sr-only">打开 {name}</span>
            </button>
            <button type="button" className="icon-button" title="在文件夹中显示" onClick={() => openIt(true)}>
              <Icon name="folder2-open" size={14} />
              <span className="sr-only">在文件夹中显示 {name}</span>
            </button>
          </span>
        )}
      </div>
      {note && (
        <p className={`file-note ${note.failed ? "failed" : ""}`} role="status">
          {note.text}
          {note.failed && hasHistory && !expandable && latest?.snapshot && !frozen && !working && onRestoreArtifact && (
            <>
              {" "}
              <button type="button" className="button text small" disabled={busy} onClick={() => restoreIt(latest)}>
                从快照恢复
              </button>
            </>
          )}
        </p>
      )}
      {open && expandable && (
        <ul className="product-versions">
          {[...product.versions].reverse().map((version) => {
            const isCurrent = !!version.snapshot && version.snapshot === product.currentSnapshot;
            const when = shortTime(version.ts);
            return (
              <li key={version.version} className={`version-row ${isCurrent ? "current" : ""}`}>
                <span className="version-info">
                  <span className="version-tag mono">v{version.version}</span>
                  {isCurrent && <span className="chip done">当前</span>}
                  <span className="version-meta">
                    {version.turn > 0 ? `第 ${version.turn} 轮` : "手动"} · {VERSION_HOW[version.how]}
                    {version.bytes !== undefined ? ` · ${fileSize(version.bytes)}` : ""}
                    {when ? ` · ${when}` : ""}
                  </span>
                </span>
                {version.snapshot && (
                  <span className="version-acts">
                    {onOpenFile && (
                      <button type="button" className="button text small" title={`打开 v${version.version} 快照（不改动洞府文件）`} onClick={() => openIt(false, version.snapshot, version.version)}>
                        打开
                      </button>
                    )}
                    {!frozen && onSaveArtifactCopy && (
                      <button type="button" className="button text small" disabled={busy} title={`另存为同目录副本（如 v${version.version}）`} onClick={() => saveCopy(version)}>
                        存副本
                      </button>
                    )}
                    {!isCurrent && !frozen && !working && onRestoreArtifact && (
                      <button type="button" className="button text small" disabled={busy} title={`将洞府中的 ${name} 恢复到此版本`} onClick={() => restoreIt(version)}>
                        恢复
                      </button>
                    )}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** A file the 分身 changed: where it is, how much it changed, and, opened, the changes themselves. */
function FileRow({
  file,
  onOpenFile,
  onInspectFile,
}: {
  file: ChangedFile;
  onOpenFile?: (path: string, reveal: boolean) => Promise<string>;
  onInspectFile?: (relPath: string) => void;
}) {
  const [open, setOpen] = useState(false);
  /** What came of opening it: why it could not be, or that a program was shown in its folder instead. */
  const [note, setNote] = useState<{ text: string; failed: boolean } | null>(null);
  const cut = Math.max(file.path.lastIndexOf("/"), file.path.lastIndexOf("\\")) + 1;
  const total = file.added + file.removed;
  const where = file.file;
  const openIt = (reveal: boolean) => {
    if (!where || !onOpenFile) return;
    setNote(null);
    onOpenFile(where, reveal).then(
      (done) => done === "revealed" && !reveal && setNote({ text: "这是可执行文件，不从这里直接运行，已在文件夹中显示。", failed: false }),
      (error) => setNote({ text: String(error), failed: true }),
    );
  };
  return (
    <div className={`file-row ${open ? "open" : ""}`}>
      <div className="file-line">
        <button type="button" className="file-head" aria-expanded={file.hunks.length ? open : undefined} disabled={!file.hunks.length} title={file.path} onClick={() => setOpen(!open)}>
          <span className="file-where">
            <span className="file-name">{file.path.slice(cut)}</span>
            {cut > 0 && <span className="file-dir">{file.path.slice(0, cut - 1)}</span>}
          </span>
          {file.created && !file.gone && <span className="chip kin">新</span>}
          {file.gone && <span className="chip">已删</span>}
          {file.movedFrom && !file.gone && (
            <span className="chip" title={`从 ${file.movedFrom} 挪来`}>
              挪来
            </span>
          )}
          {/* How much went in and how much came out, as a bar of fixed length, then in figures. */}
          <span className="file-bar" aria-hidden="true">
            <span className="in" style={{ flexGrow: total ? file.added : 0 }} />
            <span className="out" style={{ flexGrow: total ? file.removed : 0 }} />
          </span>
          <span className="file-count">
            <span className="in">+{file.added}</span> <span className="out">−{file.removed}</span>
          </span>
        </button>
        {/* At the line's end: inspect inside workbench, open with system's program, or reveal in folder. */}
        {(where || onInspectFile) && (
          <span className="file-acts">
            {!file.gone && onInspectFile && !/^(?:\/|[A-Za-z]:[\\/])/.test(file.path) && (
              <button type="button" className="icon-button" title="在中间工作台查看源码" onClick={() => onInspectFile(file.path.replace(/\\/g, "/"))}>
                <Icon name="code-slash" size={14} />
                <span className="sr-only">在工作台查看 {file.path.slice(cut)}</span>
              </button>
            )}
            {where && onOpenFile && (
              <>
                <button type="button" className="icon-button" title="用系统默认的程序打开" onClick={() => openIt(false)}>
                  <Icon name="box-arrow-up-right" size={14} />
                  <span className="sr-only">打开 {file.path.slice(cut)}</span>
                </button>
                <button type="button" className="icon-button" title="在文件夹中显示" onClick={() => openIt(true)}>
                  <Icon name="folder2-open" size={14} />
                  <span className="sr-only">在文件夹中显示 {file.path.slice(cut)}</span>
                </button>
              </>
            )}
          </span>
        )}
      </div>
      {note && (
        <p className={`file-note ${note.failed ? "failed" : ""}`} role="status">
          {note.text}
        </p>
      )}
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

/** The side panel's width in pixels: as the user left it, within what leaves the center column room. */
export const PANEL_WIDTH = { initial: 380, least: 280 };
export const fitPanel = (width: number, sidebarWidth = 236) => {
  if (typeof window === "undefined") return Math.max(width, PANEL_WIDTH.least);
  const overlay = window.innerWidth <= 1100;
  const ceiling = overlay ? window.innerWidth - 64 : window.innerWidth - sidebarWidth - 320;
  return Math.round(Math.min(Math.max(width, PANEL_WIDTH.least), Math.max(PANEL_WIDTH.least, ceiling)));
};

export interface SidePanelProps {
  files: ChangedFile[];
  runs: ToolItem[];
  todos: Todo[];
  /** What the 分身 made for 本尊: handed over, or left in the workspace. */
  products: Product[];
  /** The folder the 分身 works in, which what it made is shown from. */
  workspace?: string;
  width: number;
  sidebarWidth?: number;
  onResize: (width: number) => void;
  /** Nothing in it to show, so it stands as a strip; a tab pressed there asks for it to open anyway. */
  narrow: boolean;
  onWiden: () => void;
  /** The 分身 is at work now: the step of the plan it has in hand is lit. */
  working: boolean;
  /** The page is in replay mode: mutating actions (restore/copy) are disabled. */
  frozen?: boolean;
  /** Independent read-only side conversation (旁聊) bound to the main session; `visible` tells it when the tab is open. */
  renderAside?: (visible: boolean) => ReactNode;
  /** Dedicated pane for live background jobs and historical agent commands (`jobs` tab). */
  jobsPane?: ReactNode;
  /** Optional interactive PTY user terminal pane to render inside the `terminal` tab. */
  terminalPane?: ReactNode;
  /** Programmatic request to focus a specific tab (e.g. quoting lines into side chat or shortcut to terminal). */
  summonTab?: { id: number; tab: PanelTab } | null;
  /** Open a workspace file inside the center code viewer. */
  onInspectFile?: (relPath: string) => void;
  /** Open a file the 分身 changed with the system's program for it (or a historical snapshot), or with `reveal` show it in its folder. */
  onOpenFile?: (path: string, reveal: boolean, snapshot?: string, version?: number) => Promise<string>;
  /** Restore a deliverable back to a historical snapshot. */
  onRestoreArtifact?: (path: string, snapshot: string, version: number) => Promise<RestoredArtifact>;
  /** Save a historical deliverable version as a sibling copy in the workspace. */
  onSaveArtifactCopy?: (path: string, snapshot: string, version: number) => Promise<string>;
  /** Optional close callback when rendered as a narrow-window overlay drawer. */
  onCloseDrawer?: () => void;
}

export function SidePanel({
  files,
  runs,
  todos,
  products,
  workspace,
  width,
  sidebarWidth = 236,
  onResize,
  narrow,
  onWiden,
  working,
  frozen,
  renderAside,
  jobsPane,
  terminalPane,
  summonTab,
  onInspectFile,
  onOpenFile,
  onRestoreArtifact,
  onSaveArtifactCopy,
  onCloseDrawer,
}: SidePanelProps) {
  const hasAside = !!renderAside;
  const [openTabs, setOpenTabs] = useState<PanelTab[]>(() => (todos.length > 0 ? ["products", "plan"] : ["products"]));
  const [tab, setTab] = useState<PanelTab>("products");
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const addBtnRef = useRef<HTMLButtonElement>(null);
  const addMenuRef = useRef<HTMLDivElement>(null);
  useMenuFocus(addMenuOpen, addMenuRef, () => setAddMenuOpen(false), addBtnRef);
  const prevTodosLen = useRef(todos.length);

  // When the model creates a plan (todos transitions from empty to non-empty), auto-add the "plan" tab.
  useEffect(() => {
    const before = prevTodosLen.current;
    prevTodosLen.current = todos.length;
    if (before === 0 && todos.length > 0) {
      setOpenTabs((prev) => (prev.includes("plan") ? prev : [...prev, "plan"]));
      setTab((cur) => (cur === "products" && products.length === 0 ? "plan" : cur));
    }
  }, [todos.length, products.length]);

  const openAndSelect = (id: PanelTab) => {
    if (id === "chat" && !hasAside) return;
    setOpenTabs((prev) => (prev.includes(id) ? prev : [...prev, id]));
    setTab(id);
    setAddMenuOpen(false);
    if (narrow) onWiden();
  };

  const closeTab = (id: PanelTab) => {
    if (id === "products") return;
    setOpenTabs((prev) => {
      const next = prev.filter((item) => item !== id);
      if (tab === id) {
        const idx = prev.indexOf(id);
        setTab(next[Math.max(0, idx - 1)] ?? "products");
      }
      return next.length > 0 ? next : ["products"];
    });
  };

  useEffect(() => {
    if (!hasAside) {
      setOpenTabs((prev) => prev.filter((item) => item !== "chat"));
      if (tab === "chat") setTab("products");
    }
  }, [hasAside, tab]);

  useEffect(() => {
    if (!addMenuOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (addMenuRef.current?.contains(target) || addBtnRef.current?.contains(target)) return;
      setAddMenuOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setAddMenuOpen(false);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [addMenuOpen]);

  const lastSummon = useRef<number | null>(null);
  useEffect(() => {
    if (!summonTab || lastSummon.current === summonTab.id) return;
    lastSummon.current = summonTab.id;
    if (summonTab.tab === "chat" && !hasAside) return;
    setOpenTabs((prev) => (prev.includes(summonTab.tab) ? prev : [...prev, summonTab.tab]));
    setTab(summonTab.tab);
    setAddMenuOpen(false);
    if (narrow) onWiden();
  }, [summonTab, hasAside, narrow, onWiden]);

  const counts: Record<PanelTab, number> = {
    products: products.length,
    jobs: runs.length,
    terminal: 0,
    files: files.length,
    plan: todos.length,
    chat: 0,
  };
  // The steps done while the page was watched, for a moment: their ink is drawn through them as it happens.
  const [struck, setStruck] = useState<Set<string>>(new Set());
  const steps = useRef(new Map(todos.map((todo) => [todo.text, todo.status])));
  const strokes = useRef(new Set<ReturnType<typeof setTimeout>>());
  useEffect(() => {
    const now = todos.filter((todo) => todo.status === "done" && steps.current.has(todo.text) && steps.current.get(todo.text) !== "done").map((todo) => todo.text);
    steps.current = new Map(todos.map((todo) => [todo.text, todo.status]));
    if (now.length === 0) return;
    setStruck((was) => new Set([...was, ...now]));
    // Not taken back when the plan changes again: each stroke ends in its own time.
    const timer = setTimeout(() => {
      strokes.current.delete(timer);
      setStruck((was) => new Set([...was].filter((text) => !now.includes(text))));
    }, 1600);
    strokes.current.add(timer);
  }, [todos]);
  useEffect(() => () => strokes.current.forEach(clearTimeout), []);
  const [dragging, setDragging] = useState(false);
  // Read by the pointer's own events, which can come before the page has drawn again: the state above is for how the edge looks.
  const held = useRef(false);
  const hold = (down: boolean) => {
    held.current = down;
    setDragging(down);
  };
  const done = todos.filter((todo) => todo.status === "done").length;
  const visibleTabs = openTabs.filter((id) => id !== "chat" || hasAside).map((id) => TAB_DEFS[id]);
  const addableTabs = ADDABLE_ORDER.filter((id) => (id !== "chat" || hasAside) && !openTabs.includes(id)).map((id) => TAB_DEFS[id]);

  const badgeFor = (id: PanelTab): string | null => {
    if (id === "plan" && todos.length > 0) return `${done}/${todos.length}`;
    if (id === "products" && products.length > 0) return `${products.length}`;
    if (id === "files" && files.length > 0) return `${files.length}`;
    if (id === "jobs" && runs.length > 0) return `${runs.length}`;
    return null;
  };

  // After every hook: the strip and the panel must call the same ones, in the same order.
  if (narrow) {
    return (
      <aside className="panel strip" aria-label="交付物与扩展面板">
        <div className="strip-tabs" data-tauri-drag-region>
          {visibleTabs.map((each) => {
            const count = counts[each.id];
            const tip = each.id === "chat" || each.id === "terminal" ? each.title : `${each.name}${count > 0 ? ` (${count})` : ""} · ${each.title}`;
            return (
              <button
                type="button"
                key={each.id}
                className="tab strip-tab"
                title={tip}
                aria-label={each.name}
                onClick={() => {
                  setTab(each.id);
                  onWiden();
                }}
              >
                {each.icon}
                <span className="sr-only">{each.name}</span>
              </button>
            );
          })}
          {addableTabs.length > 0 && (
            <button
              type="button"
              className="tab strip-tab panel-add-strip"
              title="添加更多面板（分身任务、用户终端、改动、计划等）"
              aria-label="添加面板"
              onClick={() => {
                onWiden();
                setAddMenuOpen(true);
              }}
            >
              <Icon name="plus-lg" size={14} />
            </button>
          )}
        </div>
      </aside>
    );
  }
  return (
    <aside className={`panel ${tab === "chat" && hasAside ? "with-chat" : ""}`} aria-label="交付物与扩展面板">
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
        onPointerMove={(event) => held.current && onResize(fitPanel(window.innerWidth - event.clientX, sidebarWidth))}
        onPointerUp={() => hold(false)}
        onPointerCancel={() => hold(false)}
        onDoubleClick={() => onResize(fitPanel(PANEL_WIDTH.initial, sidebarWidth))}
        onKeyDown={(event) => {
          // Left widens the panel, right narrows it: the edge moves the way the arrow points.
          if (event.key === "ArrowLeft") onResize(fitPanel(width + 24, sidebarWidth));
          if (event.key === "ArrowRight") onResize(fitPanel(width - 24, sidebarWidth));
        }}
      />
      <div className="tabs" data-tauri-drag-region>
        {visibleTabs.map((each) => {
          const badge = badgeFor(each.id);
          const activeTab = tab === each.id;
          const closable = each.id !== "products";
          return (
            <div key={each.id} className={`tab-item ${activeTab ? "active" : ""}`}>
              <button type="button" className="tab" aria-pressed={activeTab} title={each.title} onClick={() => setTab(each.id)}>
                {each.icon}
                <span className="tab-word">{each.name}</span>
                {badge && (
                  <>
                    <span className="sr-only"> </span>
                    <span className="count">{badge}</span>
                  </>
                )}
              </button>
              {closable && (
                <button
                  type="button"
                  className="tab-close"
                  title={`关闭「${each.name}」标签`}
                  aria-label={`关闭 ${each.name}`}
                  onClick={(event) => {
                    event.stopPropagation();
                    closeTab(each.id);
                  }}
                >
                  ×
                </button>
              )}
            </div>
          );
        })}
        {addableTabs.length > 0 && (
          <div className="panel-add-wrap">
            <button
              ref={addBtnRef}
              type="button"
              className={`panel-add-btn ${addMenuOpen ? "open" : ""}`}
              title="添加面板页面"
              aria-label="添加面板页面"
              aria-expanded={addMenuOpen}
              aria-haspopup="menu"
              onClick={() => setAddMenuOpen((open) => !open)}
            >
              <Icon name="plus-lg" size={13} />
            </button>
          </div>
        )}
        {onCloseDrawer && (
          <>
            <span className="spacer" />
            <button type="button" className="icon-button small" title="收起右侧抽屉 (Esc)" aria-label="收起右侧抽屉" onClick={onCloseDrawer}>
              <Icon name="x-lg" size={13} />
            </button>
          </>
        )}
      </div>
      {addMenuOpen && addableTabs.length > 0 && (
        <div className="panel-add-menu" ref={addMenuRef} role="menu" aria-label="可添加的面板">
          {addableTabs.map((each) => {
            const badge = badgeFor(each.id);
            return (
              <button
                type="button"
                key={each.id}
                role="menuitem"
                className="panel-add-item"
                onClick={() => openAndSelect(each.id)}
              >
                <span className="panel-add-icon">{each.icon}</span>
                <span className="panel-add-text">
                  <span className="panel-add-name">
                    {each.name}
                    {badge && <span className="panel-add-badge">{badge}</span>}
                  </span>
                  <span className="panel-add-desc">{each.desc}</span>
                </span>
              </button>
            );
          })}
        </div>
      )}
      {hasAside && openTabs.includes("chat") && (
        <div className="side-chat-pane" style={{ display: tab === "chat" ? "flex" : "none" }}>
          {renderAside(tab === "chat")}
        </div>
      )}
      {terminalPane && openTabs.includes("terminal") && (
        <div className="side-terminal-host" style={{ display: tab === "terminal" ? "flex" : "none" }}>
          {terminalPane}
        </div>
      )}
      {(tab === "chat" && hasAside) || (tab === "terminal" && terminalPane) ? null : tab === "plan" ? (
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
                {/* The step in hand is lit only while the 分身 works: a plan left half done does not glow for good. */}
                <span className={`row-seal ${todo.status === "done" ? "done" : todo.status === "in_progress" ? (working ? "busy" : "hand") : ""}`} aria-hidden="true">
                  {todo.status === "in_progress" && working && <Light />}
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
      ) : tab === "products" ? (
        <div className="panel-body">
          <div className="panel-head">
            <h2>分身的交付物</h2>
            {products.length > 0 && <span className="panel-meta">{products.length} 个</span>}
          </div>
          {products.length === 0 && <p className="muted">分身还没有做出文件。它交付的文档、图片，和每一轮结束时在洞府里找到的新文件，都会列在这里。</p>}
          <div className="file-list">
            {products.map((product) => (
              <ProductRow
                key={product.path}
                product={product}
                workspace={workspace}
                working={working}
                frozen={frozen}
                onOpenFile={onOpenFile}
                onRestoreArtifact={onRestoreArtifact}
                onSaveArtifactCopy={onSaveArtifactCopy}
              />
            ))}
          </div>
        </div>
      ) : tab === "files" ? (
        <div className="panel-body">
          <div className="panel-head">
            <h2>本分身累计改动</h2>
            {files.length > 0 && (
              <span className="panel-meta">
                {files.length} 个，<span className="in">+{files.reduce((sum, file) => sum + file.added, 0)}</span> <span className="out">−{files.reduce((sum, file) => sum + file.removed, 0)}</span>
              </span>
            )}
          </div>
          {files.length === 0 ? (
            <p className="muted">这个分身还没有改过文件。此处统计本会话各轮累计编辑；查看与提交 Git 工作区变更请切到左栏「Git」。</p>
          ) : (
            <p className="panel-sub muted">本会话累计编辑记录（Git 暂存与提交见左栏「Git」）</p>
          )}
          <div className="file-list">
            {files.map((file) => (
              <FileRow key={file.path} file={file} onOpenFile={onOpenFile} onInspectFile={onInspectFile} />
            ))}
          </div>
        </div>
      ) : tab === "jobs" && jobsPane ? (
        jobsPane
      ) : (
        <div className="panel-body">
          <div className="panel-head">
            <h2>{tab === "terminal" ? "用户终端" : "分身命令记录"}</h2>
          </div>
          {tab === "terminal" ? (
            <p className="muted">当前模式不可打开交互式终端。</p>
          ) : (
            <>
              {runs.length === 0 && <p className="muted">分身还没有运行过命令。</p>}
              {runs.map((run) => {
                const meta = run.result!.meta!;
                return (
                  <pre className="terminal" key={run.id}>
                    <span className="prompt">$</span> {meta.cwd ? `[${meta.cwd}] ` : ""}{meta.command}
                    {run.name === "run_command" && meta.background ? <span className="muted">{`\n[后台运行，编号 ${meta.job}]`}</span> : null}
                    {run.name !== "run_command" ? <span className="muted">{`  （${meta.job}${run.name === "stop_command" ? "，停止" : ""}）`}</span> : null}
                    {"\n"}
                    {meta.output}
                    {meta.stopped ? <span className="failed">{`\n[${meta.stopped === "timed out" ? "超时停止" : "已停止"}]`}</span> : null}
                    {!meta.stopped && !meta.background && !meta.running && meta.exit_code !== 0 ? <span className="failed">{`\n[退出码 ${meta.exit_code ?? "未知"}]`}</span> : null}
                  </pre>
                );
              })}
            </>
          )}
        </div>
      )}
    </aside>
  );
}

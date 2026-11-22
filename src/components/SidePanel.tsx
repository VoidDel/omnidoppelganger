import { type ReactNode, useEffect, useRef, useState } from "react";
import type { ChangedFile, Product, ToolItem } from "../lib/fold";
import type { Todo } from "../lib/types";
import { Light } from "./Light";
import { Diff } from "./Markdown";

/** The seal on a step of the plan: nothing yet for one to come, 办 for the one in hand, 毕 for one done. */
const TODO_SEALS: Record<Todo["status"], string> = { pending: "", in_progress: "办", done: "毕" };

/** The panel's width while there is nothing in it: a strip that holds its tabs. */
export const PANEL_STRIP = 56;

type PanelTab = "files" | "terminal" | "plan" | "products";

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
  {
    id: "products",
    name: "产物",
    title: "产物：分身做出来的文档、图片等文件，交付的和这一轮留在洞府里的",
    icon: (
      // A parcel, tied: what is handed over.
      <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
        <path d="M2.5 4.9 8 2.2l5.5 2.7v6.2L8 13.8l-5.5-2.7z" />
        <path d="M2.5 4.9 8 7.6l5.5-2.7M8 7.6v6.2M5.2 3.5l5.5 2.8" />
      </svg>
    ),
  },
];

/** A file the 分身 made for 本尊: its name, opened when pressed, where it is, and its folder. */
function ProductRow({ product, workspace, onOpenFile }: { product: Product; workspace?: string; onOpenFile?: (path: string, reveal: boolean) => Promise<string> }) {
  const [note, setNote] = useState<{ text: string; failed: boolean } | null>(null);
  // Where it is, from the workspace when it is in it: the folders that tell one file from another, not the drive's.
  const root = workspace?.replace(/[\\/]+$/, "");
  const inside = !!root && product.path.length > root.length + 1 && product.path.slice(0, root.length).toLowerCase() === root.toLowerCase() && /[\\/]/.test(product.path[root.length]);
  const shown = inside ? product.path.slice(root!.length + 1) : product.path;
  const cut = Math.max(shown.lastIndexOf("/"), shown.lastIndexOf("\\")) + 1;
  const name = shown.slice(cut);
  const openIt = (reveal: boolean) => {
    if (!onOpenFile) return;
    setNote(null);
    onOpenFile(product.path, reveal).then(
      (done) => done === "revealed" && !reveal && setNote({ text: "这是可执行文件，不从这里直接运行，已在文件夹中显示。", failed: false }),
      (error) => setNote({ text: String(error), failed: true }),
    );
  };
  return (
    <div className="file-row product-row">
      <div className="file-line">
        <button type="button" className="file-head" disabled={!onOpenFile} title={`${product.path}\n用系统默认的程序打开`} onClick={() => openIt(false)}>
          <span className="file-where">
            <span className="file-name">{name}</span>
            {cut > 0 && <span className="file-dir">{shown.slice(0, cut - 1)}</span>}
          </span>
          {product.how === "painted" ? (
            <span className="chip done" title="分身请画师画的">
              作画
            </span>
          ) : product.how === "delivered" ? (
            <span className="chip done" title="分身做好之后交付的">
              交付
            </span>
          ) : (
            <span className="chip kin" title={`第 ${product.turn} 轮结束时在洞府里找到的：分身用脚本或命令做出来，没有说交付`}>
              留下
            </span>
          )}
        </button>
        {onOpenFile && (
          <span className="file-acts">
            <button type="button" className="icon-button" title="用系统默认的程序打开" onClick={() => openIt(false)}>
              <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                <path d="M9.5 2.5h4v4M13.5 2.5 7.5 8.5M12 9.5v4H2.5V4H6.5" />
              </svg>
              <span className="sr-only">打开 {name}</span>
            </button>
            <button type="button" className="icon-button" title="在文件夹中显示" onClick={() => openIt(true)}>
              <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                <path d="M1.8 4.2V12.6h12.4V5.8H7.6L6.2 4.2z" />
              </svg>
              <span className="sr-only">在文件夹中显示 {name}</span>
            </button>
          </span>
        )}
      </div>
      {note && (
        <p className={`file-note ${note.failed ? "failed" : ""}`} role="status">
          {note.text}
        </p>
      )}
    </div>
  );
}

/** A file the 分身 changed: where it is, how much it changed, and, opened, the changes themselves. */
function FileRow({ file, onOpenFile }: { file: ChangedFile; onOpenFile?: (path: string, reveal: boolean) => Promise<string> }) {
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
        {/* At the line's end: the file opened with the system's program for it, and its folder. */}
        {where && onOpenFile && (
          <span className="file-acts">
            <button type="button" className="icon-button" title="用系统默认的程序打开" onClick={() => openIt(false)}>
              <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                <path d="M9.5 2.5h4v4M13.5 2.5 7.5 8.5M12 9.5v4H2.5V4H6.5" />
              </svg>
              <span className="sr-only">打开 {file.path.slice(cut)}</span>
            </button>
            <button type="button" className="icon-button" title="在文件夹中显示" onClick={() => openIt(true)}>
              <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                <path d="M1.8 4.2V12.6h12.4V5.8H7.6L6.2 4.2z" />
              </svg>
              <span className="sr-only">在文件夹中显示 {file.path.slice(cut)}</span>
            </button>
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

/** The side panel's width in pixels: as the user left it, within what leaves the conversation room. */
export const PANEL_WIDTH = { initial: 360, least: 260 };
export const fitPanel = (width: number) => Math.round(Math.min(Math.max(width, PANEL_WIDTH.least), Math.max(PANEL_WIDTH.least, window.innerWidth - 248 - 420)));

export interface SidePanelProps {
  files: ChangedFile[];
  runs: ToolItem[];
  todos: Todo[];
  /** What the 分身 made for 本尊: handed over, or left in the workspace. */
  products: Product[];
  /** The folder the 分身 works in, which what it made is shown from. */
  workspace?: string;
  width: number;
  onResize: (width: number) => void;
  /** Nothing in it to show, so it stands as a strip; a tab pressed there asks for it to open anyway. */
  narrow: boolean;
  onWiden: () => void;
  /** The 分身 is at work now: the step of the plan it has in hand is lit. */
  working: boolean;
  /** Open a file the 分身 changed with the system's program for it, or with `reveal` show it in its folder. */
  onOpenFile?: (path: string, reveal: boolean) => Promise<string>;
}

export function SidePanel({ files, runs, todos, products, workspace, width, onResize, narrow, onWiden, working, onOpenFile }: SidePanelProps) {
  const [tab, setTab] = useState<PanelTab>("files");
  const counts: Record<PanelTab, number> = { files: files.length, terminal: runs.length, plan: todos.length, products: products.length };
  // When the first thing comes into a panel that held nothing, it opens on that.
  const had = useRef(counts);
  useEffect(() => {
    const before = had.current;
    had.current = counts;
    // Something made for 本尊 is what the work was for: the panel turns to it when the first of it comes.
    if (before.products === 0 && counts.products > 0) return setTab("products");
    if (before.files + before.terminal + before.plan + before.products > 0) return;
    const first = TABS.find((each) => before[each.id] === 0 && counts[each.id] > 0);
    if (first) setTab(first.id);
  }, [files.length, runs.length, todos.length, products.length]);
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
            <span className="sr-only">{each.id === "plan" || each.id === "products" ? `${each.name} ` : each.name}</span>
            {each.id === "plan" && todos.length > 0 && <span className="count">{`${done}/${todos.length}`}</span>}
            {each.id === "products" && products.length > 0 && <span className="count">{products.length}</span>}
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
            <h2>分身的产物</h2>
            {products.length > 0 && <span className="panel-meta">{products.length} 个</span>}
          </div>
          {products.length === 0 && <p className="muted">分身还没有做出文件。它交付的文档、图片，和每一轮结束时在洞府里找到的新文件，都会列在这里。</p>}
          <div className="file-list">
            {products.map((product) => (
              <ProductRow key={product.path} product={product} workspace={workspace} onOpenFile={onOpenFile} />
            ))}
          </div>
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
              <FileRow key={file.path} file={file} onOpenFile={onOpenFile} />
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
        </div>
      )}
    </aside>
  );
}

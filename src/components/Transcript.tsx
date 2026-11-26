import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { inFlight, land } from "../lib/flight";
import { type Item, type Row, type StreamBlock, type ToolItem, documentLabel, fileSize, shortCount, undoableTurns } from "../lib/fold";
import { useMenuFocus } from "../lib/menuFocus";
import { EMPTY_STREAM, useSessionStream } from "../lib/streamStore";
import type { Decision, Image, Mode, Reply, SessionItem, Status } from "../lib/types";
import type { ComposerQuoteInput } from "./Composer";
import { ConfirmBubble } from "./ConfirmBubble";
import { Icon } from "./Icon";
import { Light } from "./Light";
import { Diff, ModelText, StreamingMarkdown } from "./Markdown";
import { QuestionSheet } from "./Questions";
import {
  DelegateCard,
  type Family,
  type Files,
  FilesContext,
  Live,
  PlanCard,
  SignSheet,
  ToolCard,
  Written,
  presented,
  sealOf,
  useFresh,
  voided,
  weigh,
} from "./ToolCard";
import { Viewer, imageSource } from "./Viewer";

export { Diff };

type ResizeCallback = () => void;
const resizeRegistry = new Map<Element, Set<ResizeCallback>>();
let sharedResizeObserver: ResizeObserver | null = null;

function observeSharedResize(el: Element, cb: ResizeCallback): () => void {
  if (typeof ResizeObserver === "undefined") return () => undefined;
  if (!sharedResizeObserver) {
    sharedResizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const listeners = resizeRegistry.get(entry.target);
        if (listeners) {
          for (const fn of listeners) fn();
        }
      }
    });
  }
  let set = resizeRegistry.get(el);
  if (!set) {
    set = new Set();
    resizeRegistry.set(el, set);
    sharedResizeObserver.observe(el);
  }
  set.add(cb);
  return () => {
    const cur = resizeRegistry.get(el);
    if (!cur) return;
    cur.delete(cb);
    if (cur.size === 0) {
      resizeRegistry.delete(el);
      sharedResizeObserver?.unobserve(el);
    }
  };
}

const time = (ts: string) => new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

function notice(reason: string, error: string | null): string {
  switch (reason) {
    case "cancelled":
      return "本尊收回了分身。";
    case "refusal":
      return "此事分身不肯接：模型拒绝了这个请求。";
    case "max_tokens":
      return "分身一口气说得太长，被截断了，截断的操作没有执行。可以让它分几步来。";
    case "step_limit":
      return "这一轮步骤太多，分身自行收功了。";
    case "interrupted":
      return "上次关闭应用时，分身这一轮还没收功。";
    default:
      return error === "no API key is configured"
        ? "还没有这个宗门（模型供应商）的 API 密钥，分身出不了关。到设置里填上，再传一次令。"
        : `这一轮走火了，已经停下：${error ?? "原因不明"}`;
  }
}

interface ItemViewProps {
  item: Item;
  mode: Mode;
  family: Family;
  frozen: boolean;
  onDecide: (toolUseId: string, decision: Decision) => void;
  onAnswer: (toolUseId: string, answers: Reply[] | null) => void;
  onForward?: (text: string) => void;
}

function ItemView({ item, mode, family, frozen, onDecide, onAnswer, onForward }: ItemViewProps) {
  switch (item.kind) {
    case "text":
      return <ModelText text={item.text} onForward={onForward} />;
    case "thinking":
      return (
        <details className="thinking">
          <summary>分身推演了一番</summary>
          <p>{item.text}</p>
        </details>
      );
    case "tool":
      if (item.question) return <QuestionSheet tool={item} frozen={frozen} onAnswer={onAnswer} />;
      if (presented(item)) return <PlanCard tool={item} />;
      if (item.approval && !item.approval.decision && !item.result) {
        return <SignSheet tool={item} mode={mode} frozen={frozen} onDecide={onDecide} />;
      }
      return item.name === "delegate" ? <DelegateCard tool={item} family={family} /> : <ToolCard tool={item} />;
    case "fallback":
      return (
        <div className="notice">
          {item.from} 没有接这个请求，改由 {item.to} 继续。
        </div>
      );
    case "compacted":
      return (
        <details className="thinking">
          <summary>
            分身把约 {shortCount(item.tokensBefore)} token 的前情凝成了一段摘要
            {item.focus ? `（重点保留：${item.focus}）` : ""}
            {item.keptFromTurn ? `，并原样保留了第 ${item.keptFromTurn} 轮起的近期上下文` : "，之后只凭摘要接着做"}
          </summary>
          <p>{item.summary}</p>
        </details>
      );
    case "instructions":
      return item.content ? (
        <details className="thinking">
          <summary>分身读了洞府里的 {item.path}，之后都照里面的规矩办事</summary>
          <p>{item.content}</p>
        </details>
      ) : (
        <div className="notice">{item.path} 不在了，分身不再照它办事。</div>
      );
    case "restored":
      return (
        <div className="notice">
          本尊撤回了第 {item.turn} 轮起的改动。
          {item.restored.length > 0 && `已恢复原样：${item.restored.join("、")}。`}
          {item.skipped.length > 0 && `没有动：${item.skipped.join("、")}，分身改过之后它们又有变动，恢复会盖掉那些变动。`}
        </div>
      );
    case "artifact_restored":
      return (
        <div className="notice">
          本尊将 {item.path} 恢复到了 v{item.version}
          {item.previous ? "（恢复前已为原文件留存备份版本）" : ""}。
        </div>
      );
    case "notice":
      return <div className="notice">{notice(item.reason, item.error)}</div>;
    case "made":
      return <Made files={item.files} snapshots={item.snapshots} />;
  }
}

function Made({ files, snapshots }: { files: string[]; snapshots?: Record<string, string> }) {
  return (
    <div className="made">
      <p className="made-head">这一轮在洞府里留下了 {files.length} 个文件</p>
      {files.map((path) => {
        const name = path.replace(/^.*[\\/]/, "");
        return (
          <div className="made-file" key={path}>
            <span className="made-name mono" title={path}>
              {name}
            </span>
            <Written path={path} name={name} snapshot={snapshots?.[path]} />
          </div>
        );
      })}
    </div>
  );
}

function ThinkingLine({ text, open = true }: { text: string; open?: boolean }) {
  const line = useRef<HTMLSpanElement>(null);
  const words = useRef<HTMLSpanElement>(null);
  const [full, setFull] = useState(false);
  const tail = text.slice(-400).replace(/\s+/g, " ").trim();

  const measure = useCallback(() => {
    if (line.current && words.current) {
      const next = words.current.offsetWidth > line.current.clientWidth;
      setFull((prev) => (prev === next ? prev : next));
    }
  }, []);

  useLayoutEffect(() => {
    if (!open) return;
    measure();
  }, [text, open, measure]);

  useEffect(() => {
    const el = line.current;
    if (!open || !el) return;
    return observeSharedResize(el, measure);
  }, [open, measure]);

  return (
    <div className="thinking thinking-live" title={text.slice(-2000)}>
      <span className="label" data-lit="推演中：">
        推演中：
      </span>
      <span ref={line} className={`tail ${full ? "full" : ""}`}>
        <span ref={words}>{tail}</span>
      </span>
    </div>
  );
}

function Streaming({
  sessionId,
  blocks: givenBlocks,
  frozen,
}: {
  sessionId?: string;
  blocks?: StreamBlock[];
  frozen: boolean;
}) {
  const storeBlocks = useSessionStream(sessionId ?? null, !frozen && givenBlocks === undefined);
  const blocks = frozen ? EMPTY_STREAM : (givenBlocks ?? storeBlocks);

  return (
    <>
      {blocks.map((block) =>
        block.kind === "text" ? (
          <StreamingMarkdown key={block.index} text={block.text} />
        ) : block.kind === "thinking" ? (
          <ThinkingLine key={block.index} text={block.text} />
        ) : (
          <div key={block.index} className="did live fresh">
            <div className="did-head">
              <span className="seal" aria-hidden="true">
                <Light />起
              </span>
              <span className="verb">起手</span>
              <span className="target">{block.name}</span>
              <span className="leader" aria-hidden="true" />
              <span className="outcome">蓄势中</span>
            </div>
          </div>
        ),
      )}
    </>
  );
}

const FOLD = 4;

type Part = { kind: "item"; item: Item; key: string } | { kind: "act"; tool: ToolItem } | { kind: "fold"; tools: ToolItem[] };

const isLine = (item: Item): boolean =>
  item.kind === "tool" &&
  item.name !== "delegate" &&
  !item.question &&
  !presented(item) &&
  !(item.approval && !item.approval.decision && !item.result);

const looked = (tool: ToolItem) => weigh(tool) === "quiet" && !voided(tool);

function parts(items: Item[]): Part[] {
  const out: Part[] = [];
  for (let at = 0; at < items.length; ) {
    const item = items[at];
    if (!isLine(item)) {
      out.push({ kind: "item", item, key: item.kind === "tool" ? item.id : `item-${at}` });
      at++;
      continue;
    }
    let end = at;
    while (end < items.length && isLine(items[end]) && looked(items[end] as ToolItem)) end++;
    if (end - at >= FOLD) {
      out.push({ kind: "fold", tools: items.slice(at, end) as ToolItem[] });
      at = end;
    } else {
      const stop = Math.max(end, at + 1);
      for (const tool of items.slice(at, stop) as ToolItem[]) out.push({ kind: "act", tool });
      at = stop;
    }
  }
  return out;
}

function Folded({ tools }: { tools: ToolItem[] }) {
  const fresh = useFresh();
  const [open, setOpen] = useState(false);
  const counts = new Map<string, number>();
  for (const tool of tools) counts.set(sealOf(tool), (counts.get(sealOf(tool)) ?? 0) + 1);
  return (
    <>
      <div className={`did quiet folded ${fresh.className}`} style={fresh.style}>
        <button type="button" className="did-head" aria-expanded={open} onClick={() => setOpen(!open)}>
          <span className="seal" aria-hidden="true">
            览
          </span>
          <span className="verb">查阅</span>
          <span className="target">{[...counts].map(([seal, count]) => `${seal} ${count}`).join("，")}</span>
          <span className="leader" aria-hidden="true" />
          <span className="outcome">{tools.length} 处</span>
        </button>
      </div>
      {open && (
        <div className="folded-acts">
          {tools.map((tool) => (
            <ToolCard key={tool.id} tool={tool} />
          ))}
        </div>
      )}
    </>
  );
}

export interface Props {
  sessionId?: string;
  rows: Row[];
  stream?: StreamBlock[];
  queued: string[];
  status: Status;
  mode: Mode;
  origin: "user" | "agent" | "schedule";
  sessions: SessionItem[];
  copied: number;
  frozen: boolean;
  onOpen: (id: string) => void;
  onDecide: (toolUseId: string, decision: Decision) => void;
  onAnswer: (toolUseId: string, answers: Reply[] | null) => void;
  onUndo: (turn: number) => void;
  onFork?: (turn: number, text: string) => void;
  onOpenFile?: (path: string, reveal: boolean, snapshot?: string, version?: number) => Promise<string>;
  onPicture?: (path: string, snapshot?: string) => Promise<Image>;
  versionOf?: (path: string, snapshot?: string) => number | undefined;
  onForward?: (text: string) => void;
  onDeliverNow?: () => void;
  onQuoteToMain?: (quote: ComposerQuoteInput) => void;
  onQuoteToAside?: (quote: ComposerQuoteInput) => void;
}

function formatSelectionQuote(raw: string): ComposerQuoteInput {
  const trimmed = raw.trim();
  const lines = trimmed.split(/\r?\n/);
  const content = lines.map((line) => `> ${line}`).join("\n");
  const meta = lines.length > 1 ? `${lines.length} 行 · ${trimmed.length} 字` : `${trimmed.length} 字`;
  return { label: "引用对话", meta, content };
}

function UndoButton({ turn, onUndo }: { turn: number; onUndo: (turn: number) => void }) {
  const [asking, setAsking] = useState(false);
  return (
    <span className="bubble-anchor">
      <button
        type="button"
        className="icon-button danger"
        aria-expanded={asking}
        title="撤回这一轮起的改动：分身从这一轮起改的文件恢复原样"
        onClick={() => setAsking(!asking)}
      >
        <Icon name="arrow-counterclockwise" size={14} />
        <span className="sr-only">撤回这一轮起的改动</span>
      </button>
      {asking && (
        <ConfirmBubble
          label="撤回这一轮起的改动"
          confirm="确认撤回"
          onConfirm={() => (setAsking(false), onUndo(turn))}
          onCancel={() => setAsking(false)}
        >
          <p>分身从这一轮起改的文件都会恢复原样，它新建的会删掉。</p>
        </ConfirmBubble>
      )}
    </span>
  );
}

function SaidText({ text }: { text: string }) {
  const body = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [more, setMore] = useState(false);

  const measure = useCallback(() => {
    if (!open && body.current) {
      const next = body.current.scrollHeight > body.current.clientHeight + 1;
      setMore((prev) => (prev === next ? prev : next));
    }
  }, [open]);

  useLayoutEffect(() => {
    if (!open && body.current) {
      const next = body.current.scrollHeight > body.current.clientHeight + 1;
      setMore((prev) => (prev === next ? prev : next));
    }
  }, [text, open]);

  useEffect(() => {
    const el = body.current;
    if (open || !el) return;
    return observeSharedResize(el, measure);
  }, [open, measure]);

  return (
    <div className="said-body">
      <div ref={body} className={`said-text ${open ? "open" : "held"}`}>
        {text}
      </div>
      {(more || open) && (
        <button type="button" className="said-more" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? "收起" : "展开"}
        </button>
      )}
    </div>
  );
}

function Queued({ text, onDeliverNow }: { text: string; onDeliverNow?: () => void }) {
  const [sent, setSent] = useState(false);
  return (
    <div className="said-queued">
      <div className="original queued">
        <span className="seal" aria-hidden="true">
          音
        </span>
        <div className="said-text">{text || "（附件）"}</div>
        {onDeliverNow && (
          <button
            type="button"
            className="icon-button queued-now"
            disabled={sent}
            title={sent ? "正在立即传音…" : "立即传音：打断当前这一步，马上把这句话交给分身"}
            onClick={() => {
              setSent(true);
              onDeliverNow();
            }}
          >
            <Icon name="send" size={14} />
            <span className="sr-only">立即传音</span>
          </button>
        )}
      </div>
      <div className="said-foot">
        <span className="said-meta">{sent ? "正在打断当前步骤，立即传音…" : "传音在途，分身做完这一步就会听到"}</span>
      </div>
    </div>
  );
}

const SEAM = <div className="seam">以上是分叉时带过来的前情，以下是这个分叉自己的</div>;

interface TurnRowProps {
  row: Row;
  last: boolean;
  closing: boolean;
  task: "agent" | "schedule" | null;
  ownFirst: boolean;
  copied: number;
  canUndo: boolean;
  canFork: boolean;
  frozen: boolean;
  status: Status;
  mode: Mode;
  family: Family;
  sessionId?: string;
  stream?: StreamBlock[];
  hasOpenFile: boolean;
  fileNote: { path: string; text: string; failed: boolean } | null;
  onUndo: (turn: number) => void;
  onFork?: (turn: number, text: string) => void;
  onDecide: (toolUseId: string, decision: Decision) => void;
  onAnswer: (toolUseId: string, answers: Reply[] | null) => void;
  onForward?: (text: string) => void;
  onViewImage: (images: Image[], at: number) => void;
  onOpenAttached: (path: string, reveal: boolean) => void;
}

const TurnRow = memo(function TurnRow({
  row,
  last,
  closing,
  task,
  ownFirst,
  copied,
  canUndo,
  canFork,
  frozen,
  status,
  mode,
  family,
  sessionId,
  stream,
  hasOpenFile,
  fileNote,
  onUndo,
  onFork,
  onDecide,
  onAnswer,
  onForward,
  onViewImage,
  onOpenAttached,
}: TurnRowProps) {
  const who = task === "agent" ? "主分身交办" : task === "schedule" ? "功课交办" : row.prompt.steering ? "中途传音" : null;
  return (
    <section className={`turn ${last ? "latest" : ""} ${last && closing ? "closing" : ""}`}>
      {ownFirst && SEAM}
      <div className="turn-said">
        {row.prompt.images.length > 0 && (
          <div className="shots">
            {row.prompt.images.map((image, at) => (
              <button key={at} type="button" className="shot" title="点开看大图" onClick={() => onViewImage(row.prompt.images, at)}>
                <img src={imageSource(image)} alt={`本尊附的第 ${at + 1} 张图`} />
              </button>
            ))}
          </div>
        )}
        <div className="said-top">
          {canUndo && <UndoButton turn={row.turn} onUndo={onUndo} />}
          {canFork && (
            <button
              type="button"
              className="icon-button"
              title="从这里分叉：另起一个分身，带着这句话之前的对话，这句话放回输入框，改了再传。只分对话，洞府里的文件还是现在的样子"
              onClick={() => onFork?.(row.turn, row.prompt.text)}
            >
              <Icon name="signpost-split" size={14} />
              <span className="sr-only">从这里分叉</span>
            </button>
          )}
          <span className="said-when">
            {who && `${who}，`}
            {time(row.prompt.ts)}
          </span>
        </div>
        <div className="original" title={`${who ?? "本尊"}，${time(row.prompt.ts)}`}>
          <span className="seal" aria-hidden="true">
            {task === "agent" ? "遣" : task === "schedule" ? "课" : row.prompt.steering ? "音" : "令"}
          </span>
          {row.prompt.text && <SaidText text={row.prompt.text} />}
        </div>
        {row.prompt.files.length > 0 && (
          <div className="said-files">
            {row.prompt.files.map((file) => {
              const how = file.is_dir
                ? "文件夹路径随这句话给了分身，由分身自行读取处理"
                : file.inline
                  ? file.document
                    ? "取出的文字与文件路径随这句话一起给了分身，分身也可调用工具处理原文件"
                    : "它的内容随这句话一起给了分身"
                  : "文件路径随这句话给了分身，由分身自行调用工具读取处理";
              return (
                <span key={file.path} className="file-chip openable">
                  <button
                    type="button"
                    className="file-chip-open"
                    disabled={!hasOpenFile}
                    title={`${file.path}\n${how}\n点开：用系统默认的程序打开`}
                    onClick={() => onOpenAttached(file.path, false)}
                  >
                    {file.is_dir ? <Icon name="folder2" size={14} /> : <Icon name="file-earmark-text" size={14} />}
                    <span className="file-chip-name">{file.path.replace(/^.*[\\/]/, "")}</span>
                    <span className="file-chip-size">
                      {file.is_dir ? (
                        "文件夹"
                      ) : (
                        <>
                          {fileSize(file.bytes)}
                          {file.document ? `，${documentLabel(file.document, file.parts)}` : ""}
                        </>
                      )}
                    </span>
                  </button>
                  <button
                    type="button"
                    className="file-chip-reveal"
                    disabled={!hasOpenFile}
                    title="在文件夹中显示"
                    onClick={() => onOpenAttached(file.path, true)}
                  >
                    <Icon name="folder2-open" size={14} />
                    <span className="sr-only">在文件夹中显示 {file.path.replace(/^.*[\\/]/, "")}</span>
                  </button>
                </span>
              );
            })}
          </div>
        )}
        {fileNote && row.prompt.files.some((file) => file.path === fileNote.path) && (
          <p className={`file-note ${fileNote.failed ? "failed" : ""}`} role="status">
            {fileNote.text}
          </p>
        )}
      </div>
      <div className={`turn-did ${last && status === "running" ? "live" : ""}`}>
        {parts(row.items).map((part) =>
          part.kind === "act" ? (
            <ToolCard key={part.tool.id} tool={part.tool} />
          ) : part.kind === "fold" ? (
            <Folded key={`fold-${part.tools[0].id}`} tools={part.tools} />
          ) : (
            <ItemView
              key={part.key}
              item={part.item}
              mode={mode}
              family={family}
              frozen={frozen}
              onDecide={onDecide}
              onAnswer={onAnswer}
              onForward={frozen ? undefined : onForward}
            />
          ),
        )}
        {last && <Streaming sessionId={sessionId} blocks={stream} frozen={frozen} />}
        {last && copied > 0 && row.turn <= copied && SEAM}
        {last && (status === "running" || closing) && <span className="vein" aria-hidden="true" />}
        {last && status === "running" && <span className="vein-tip" aria-hidden="true" />}
      </div>
    </section>
  );
});

export function Transcript({
  sessionId,
  rows,
  stream,
  queued,
  status,
  mode,
  origin,
  sessions,
  copied,
  frozen,
  onOpen,
  onDecide,
  onAnswer,
  onUndo,
  onFork,
  onOpenFile,
  onPicture,
  versionOf,
  onForward,
  onDeliverNow,
  onQuoteToMain,
  onQuoteToAside,
}: Props) {
  const family = useMemo<Family>(() => ({ sessions, onOpen }), [sessions, onOpen]);
  const undoable = useMemo(() => undoableTurns(rows, copied), [rows, copied]);

  const live = useRef(false);
  const seen = useRef<number | null>(null);
  const count = rows.reduce((sum, row) => sum + row.items.length, 0);
  live.current = seen.current !== null && (status !== "idle" || frozen) && count - seen.current <= 12;
  useLayoutEffect(() => {
    seen.current = count;
  }, [count]);

  const [fileNote, setFileNote] = useState<{ path: string; text: string; failed: boolean } | null>(null);
  const noteTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(noteTimer.current), []);

  const onOpenFileRef = useRef(onOpenFile);
  onOpenFileRef.current = onOpenFile;
  const openFile = useCallback((path: string, reveal: boolean) => {
    const fn = onOpenFileRef.current;
    if (!fn) return;
    clearTimeout(noteTimer.current);
    setFileNote(null);
    fn(path, reveal)
      .then(
        (done) => {
          if (done === "revealed" && !reveal) {
            setFileNote({ path, text: "这是可执行文件，不从这里直接运行，已在文件夹中显示。", failed: false });
          }
        },
        (error) => setFileNote({ path, text: String(error), failed: true }),
      )
      .finally(() => {
        noteTimer.current = setTimeout(() => setFileNote(null), 5000);
      });
  }, []);

  const [viewing, setViewing] = useState<{ images: Image[]; at: number } | null>(null);
  const viewImage = useCallback((images: Image[], at: number) => setViewing({ images, at }), []);
  const [closing, setClosing] = useState(false);
  const was = useRef(status);
  const closes = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => {
    const finished = was.current === "running" && status === "idle";
    was.current = status;
    if (finished) {
      setClosing(true);
      clearTimeout(closes.current);
      closes.current = setTimeout(() => setClosing(false), 2200);
    } else if (status === "running") {
      clearTimeout(closes.current);
      setClosing(false);
    }
  }, [status]);
  useEffect(() => () => clearTimeout(closes.current), []);

  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const lastScrollTop = useRef(0);
  const said = useRef(rows.length + queued.length);
  const totalCount = rows.length + queued.length;
  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const arriving = totalCount > said.current && inFlight();
    said.current = totalCount;
    if (arriving) pinned.current = true;
    if (pinned.current) {
      element.scrollTop = element.scrollHeight;
      lastScrollTop.current = element.scrollTop;
    }
    if (arriving) {
      const messages = element.querySelectorAll<HTMLElement>(".turn-said .original, .turn-said .shots");
      land(messages[messages.length - 1] ?? null);
    }
  }, [totalCount, count, stream]);

  const files = useMemo<Files | null>(
    () => (onOpenFile && onPicture ? { open: onOpenFile, picture: onPicture, versionOf } : null),
    [onOpenFile, onPicture, versionOf],
  );

  const canQuote = !frozen && (!!onQuoteToMain || !!onQuoteToAside);
  const [selectionQuote, setSelectionQuote] = useState<{ text: string; top: number; left: number; below: boolean } | null>(null);
  const quoteBarRef = useRef<HTMLDivElement>(null);

  const syncSelectionQuote = () => {
    if (!canQuote) {
      setSelectionQuote(null);
      return;
    }
    const root = scroller.current;
    const sel = window.getSelection();
    if (!root || !sel || sel.isCollapsed || sel.rangeCount === 0) {
      setSelectionQuote(null);
      return;
    }
    const text = sel.toString().trim();
    if (!text) {
      setSelectionQuote(null);
      return;
    }
    const range = sel.getRangeAt(0);
    if (!root.contains(range.commonAncestorContainer)) {
      setSelectionQuote(null);
      return;
    }
    const rect = range.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) {
      setSelectionQuote(null);
      return;
    }
    const below = rect.top < 76;
    const top = below ? rect.bottom + 8 : rect.top - 8;
    const left = Math.min(Math.max(rect.left + rect.width / 2, 128), window.innerWidth - 128);
    setSelectionQuote({ text, top, left, below });
  };

  useEffect(() => {
    if (!selectionQuote) return;
    const onSelectionChange = () => {
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed || !sel.toString().trim()) {
        setSelectionQuote(null);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setSelectionQuote(null);
    };
    const onPointerDown = (event: PointerEvent) => {
      if (quoteBarRef.current?.contains(event.target as Node)) return;
      setSelectionQuote(null);
    };
    document.addEventListener("selectionchange", onSelectionChange);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("selectionchange", onSelectionChange);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("pointerdown", onPointerDown);
    };
  }, [selectionQuote]);

  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    selectionText: string;
    codeBlockText: string | null;
    linkHref: string | null;
    blockText: string | null;
    blockLabel: string;
    canScrollTop: boolean;
    canScrollBottom: boolean;
  } | null>(null);
  const contextMenuRef = useRef<HTMLDivElement>(null);
  useMenuFocus(!!contextMenu, contextMenuRef, () => setContextMenu(null));

  useEffect(() => {
    if (!contextMenu) return;
    const onPointerDown = (event: PointerEvent) => {
      if (contextMenuRef.current?.contains(event.target as Node)) return;
      setContextMenu(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setContextMenu(null);
    };
    const onBlur = () => setContextMenu(null);
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("blur", onBlur);
    };
  }, [contextMenu]);

  const openTranscriptContextMenu = (event: React.MouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.stopPropagation();
    setSelectionQuote(null);

    const root = scroller.current;
    const sel = window.getSelection();
    let selectionText = "";
    if (root && sel && !sel.isCollapsed && sel.rangeCount > 0) {
      const range = sel.getRangeAt(0);
      if (root.contains(range.commonAncestorContainer)) {
        selectionText = sel.toString();
      }
    }

    const targetEl = event.target instanceof HTMLElement ? event.target : null;
    const preEl = targetEl?.closest("pre");
    const codeBlockText = preEl ? (preEl.querySelector("code")?.textContent ?? preEl.textContent ?? "").trim() || null : null;
    const linkEl = targetEl?.closest<HTMLAnchorElement>("a[href]");
    const linkHref = linkEl?.href || null;

    const proseEl = targetEl?.closest<HTMLElement>(".prose");
    const originalEl = targetEl?.closest<HTMLElement>(".original .said-text");
    let blockText: string | null = null;
    let blockLabel = "复制整段内容";
    if (proseEl) {
      blockText = (proseEl.innerText || proseEl.textContent || "").trim() || null;
      blockLabel = "复制整段回复";
    } else if (originalEl) {
      blockText = (originalEl.innerText || originalEl.textContent || "").trim() || null;
      blockLabel = "复制这条消息";
    }

    const canScrollTop = !!root && root.scrollTop > 24;
    const canScrollBottom = !!root && root.scrollHeight - root.scrollTop - root.clientHeight > 24;

    const hasAnyAction =
      Boolean(selectionText.trim()) ||
      Boolean(codeBlockText) ||
      Boolean(linkHref) ||
      Boolean(blockText) ||
      canScrollTop ||
      canScrollBottom;
    if (!hasAnyAction) {
      setContextMenu(null);
      return;
    }

    const menuWidth = 188;
    const menuHeight = 240;
    const x = Math.min(event.clientX, Math.max(8, window.innerWidth - menuWidth - 8));
    const y = Math.min(event.clientY, Math.max(8, window.innerHeight - menuHeight - 8));
    setContextMenu({
      x,
      y,
      selectionText,
      codeBlockText,
      linkHref,
      blockText,
      blockLabel,
      canScrollTop,
      canScrollBottom,
    });
  };

  const sendSelectionQuote = (target: "main" | "aside") => {
    if (!selectionQuote) return;
    const formatted = formatSelectionQuote(selectionQuote.text);
    window.getSelection()?.removeAllRanges();
    setSelectionQuote(null);
    if (target === "main") onQuoteToMain?.(formatted);
    else onQuoteToAside?.(formatted);
  };

  return (
    <FilesContext.Provider value={files}>
      <Live.Provider value={live}>
        <div
          className="transcript"
          ref={scroller}
          onContextMenu={openTranscriptContextMenu}
          onMouseUp={(event) => {
            if (event.button !== 0) return;
            const target = event.target as Node | null;
            if (target && (contextMenuRef.current?.contains(target) || quoteBarRef.current?.contains(target))) return;
            requestAnimationFrame(syncSelectionQuote);
          }}
          onKeyUp={() => requestAnimationFrame(syncSelectionQuote)}
          onScroll={(event) => {
            const element = event.currentTarget;
            const scrollingUp = element.scrollTop < lastScrollTop.current;
            lastScrollTop.current = element.scrollTop;
            pinned.current = !scrollingUp && element.scrollHeight - element.scrollTop - element.clientHeight < 80;
            if (selectionQuote) setSelectionQuote(null);
            if (contextMenu) setContextMenu(null);
          }}
        >
          {rows.map((row, index) => {
            const last = index === rows.length - 1;
            const task = index === 0 && origin !== "user" ? origin : null;
            const ownFirst = copied > 0 && row.turn > copied && (index === 0 || rows[index - 1].turn <= copied);
            const canUndo = !frozen && status === "idle" && !row.prompt.steering && undoable.has(row.turn);
            const canFork = !!onFork && !row.prompt.steering && row.turn > 0;
            const matchingNote = fileNote && row.prompt.files.some((f) => f.path === fileNote.path) ? fileNote : null;
            return (
              <TurnRow
                key={row.key}
                row={row}
                last={last}
                closing={last && closing}
                task={task}
                ownFirst={ownFirst}
                copied={copied}
                canUndo={canUndo}
                canFork={canFork}
                frozen={frozen}
                status={last ? status : "idle"}
                mode={mode}
                family={family}
                sessionId={last ? sessionId : undefined}
                stream={last ? stream : undefined}
                hasOpenFile={!!onOpenFile}
                fileNote={matchingNote}
                onUndo={onUndo}
                onFork={onFork}
                onDecide={onDecide}
                onAnswer={onAnswer}
                onForward={onForward}
                onViewImage={viewImage}
                onOpenAttached={openFile}
              />
            );
          })}
          {queued.length > 0 && (
            <section className="turn turn-queued">
              <div className="turn-said">
                {queued.map((text, at) => (
                  <Queued key={at} text={text} onDeliverNow={frozen ? undefined : onDeliverNow} />
                ))}
              </div>
            </section>
          )}
          {viewing && <Viewer images={viewing.images} at={viewing.at} whose="本尊附的" onClose={() => setViewing(null)} />}
          {selectionQuote && (
            <div
              ref={quoteBarRef}
              className={`selection-quote-bar ${selectionQuote.below ? "below" : "above"}`}
              style={{ top: `${selectionQuote.top}px`, left: `${selectionQuote.left}px` }}
              role="toolbar"
              aria-label="引用选中内容"
              onMouseDown={(event) => event.preventDefault()}
            >
              {onQuoteToMain && (
                <button
                  type="button"
                  className="selection-quote-btn"
                  title="将选中内容作为引用填入当前分身输入框"
                  onClick={() => sendSelectionQuote("main")}
                >
                  <Icon name="quote" size={13} />
                  <span>引用到分身</span>
                </button>
              )}
              {onQuoteToMain && onQuoteToAside && <span className="selection-quote-sep" aria-hidden="true" />}
              {onQuoteToAside && (
                <button
                  type="button"
                  className="selection-quote-btn"
                  title="打开侧边栏旁聊并引用选中内容提问（不打断主分身）"
                  onClick={() => sendSelectionQuote("aside")}
                >
                  <Icon name="chat-left-quote" size={13} />
                  <span>旁聊提问</span>
                </button>
              )}
            </div>
          )}
          {contextMenu &&
            (() => {
              const trimmedSel = contextMenu.selectionText.trim();
              const hasSelection = trimmedSel.length > 0;
              const showBlockCopy = Boolean(contextMenu.blockText) && contextMenu.blockText !== trimmedSel;
              const hasMiddleSection = Boolean(contextMenu.codeBlockText) || Boolean(contextMenu.linkHref) || showBlockCopy;
              const hasScrollSection = contextMenu.canScrollTop || contextMenu.canScrollBottom;
              return (
                <div
                  ref={contextMenuRef}
                  className="tree-context-menu"
                  role="menu"
                  style={{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }}
                  onMouseDown={(event) => event.preventDefault()}
                  onContextMenu={(event) => event.preventDefault()}
                >
                  {hasSelection && (
                    <>
                      <button
                        type="button"
                        role="menuitem"
                        className="tree-context-item"
                        onClick={() => {
                          const text = contextMenu.selectionText;
                          setContextMenu(null);
                          navigator.clipboard.writeText(text).catch(() => undefined);
                        }}
                      >
                        复制
                      </button>
                      {!frozen && onQuoteToMain && (
                        <button
                          type="button"
                          role="menuitem"
                          className="tree-context-item"
                          onClick={() => {
                            const quote = formatSelectionQuote(trimmedSel);
                            window.getSelection()?.removeAllRanges();
                            setContextMenu(null);
                            onQuoteToMain(quote);
                          }}
                        >
                          引用到对话
                        </button>
                      )}
                      {!frozen && onQuoteToAside && (
                        <button
                          type="button"
                          role="menuitem"
                          className="tree-context-item"
                          onClick={() => {
                            const quote = formatSelectionQuote(trimmedSel);
                            window.getSelection()?.removeAllRanges();
                            setContextMenu(null);
                            onQuoteToAside(quote);
                          }}
                        >
                          旁聊提问
                        </button>
                      )}
                      {!frozen && onForward && (
                        <button
                          type="button"
                          role="menuitem"
                          className="tree-context-item"
                          onClick={() => {
                            window.getSelection()?.removeAllRanges();
                            setContextMenu(null);
                            onForward(trimmedSel);
                          }}
                        >
                          转给主分身
                        </button>
                      )}
                      {(hasMiddleSection || hasScrollSection) && <div className="tree-context-sep" role="separator" />}
                    </>
                  )}
                  {contextMenu.codeBlockText && (
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        const code = contextMenu.codeBlockText!;
                        setContextMenu(null);
                        navigator.clipboard.writeText(code).catch(() => undefined);
                      }}
                    >
                      复制代码块
                    </button>
                  )}
                  {contextMenu.linkHref && (
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        const href = contextMenu.linkHref!;
                        setContextMenu(null);
                        navigator.clipboard.writeText(href).catch(() => undefined);
                      }}
                    >
                      复制链接
                    </button>
                  )}
                  {showBlockCopy && (
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        const text = contextMenu.blockText!;
                        setContextMenu(null);
                        navigator.clipboard.writeText(text).catch(() => undefined);
                      }}
                    >
                      {contextMenu.blockLabel}
                    </button>
                  )}
                  {hasMiddleSection && hasScrollSection && <div className="tree-context-sep" role="separator" />}
                  {contextMenu.canScrollTop && (
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        setContextMenu(null);
                        setSelectionQuote(null);
                        pinned.current = false;
                        const el = scroller.current;
                        if (el) {
                          lastScrollTop.current = el.scrollTop;
                          el.scrollTo({ top: 0, behavior: "smooth" });
                        }
                      }}
                    >
                      回到顶部
                    </button>
                  )}
                  {contextMenu.canScrollBottom && (
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        setContextMenu(null);
                        pinned.current = true;
                        const el = scroller.current;
                        if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
                      }}
                    >
                      回到底部
                    </button>
                  )}
                </div>
              );
            })()}
        </div>
      </Live.Provider>
    </FilesContext.Provider>
  );
}

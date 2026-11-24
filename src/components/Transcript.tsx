import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import { inFlight, land } from "../lib/flight";
import { type Item, type Row, type StreamBlock, type ToolItem, documentLabel, fileSize, shortCount, undoableTurns } from "../lib/fold";
import type { Decision, Image, Mode, Reply, SessionItem, Status } from "../lib/types";
import { ConfirmBubble } from "./ConfirmBubble";
import { Light } from "./Light";
import { Diff, ModelText, markdownParts, markdownPlugins, markdownSyntax } from "./Markdown";
import { QuestionSheet } from "./Questions";
import { DelegateCard, type Family, type Files, FilesContext, Live, PlanCard, SignSheet, ToolCard, Written, presented, sealOf, useFresh, voided, weigh } from "./ToolCard";
import { Viewer, imageSource } from "./Viewer";

export { Diff };

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
      // What the 分身 asked 本尊: the form while it waits, the answers after.
      if (item.question) return <QuestionSheet tool={item} frozen={frozen} onAnswer={onAnswer} />;
      if (presented(item)) return <PlanCard tool={item} />;
      if (item.approval && !item.approval.decision && !item.result) return <SignSheet tool={item} mode={mode} frozen={frozen} onDecide={onDecide} />;
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
          <summary>分身把约 {shortCount(item.tokensBefore)} token 的前情凝成了一段摘要，之后只凭摘要接着做</summary>
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

/** What the turn left in the workspace without saying so, such as a document a script wrote: each with the way to open it and to its folder. */
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

/**
 * What the 分身 is thinking while it thinks: one line, its newest words. Once the line is full, it shows the end
 * of what was written and lets the start fade out at the left, so the line keeps up without growing.
 */
function ThinkingLine({ text }: { text: string }) {
  const line = useRef<HTMLSpanElement>(null);
  const words = useRef<HTMLSpanElement>(null);
  const [full, setFull] = useState(false);
  // Newlines would wrap in a line that must not; only the last few hundred characters can be seen anyway.
  const tail = text.slice(-400).replace(/\s+/g, " ").trim();
  // The words' own width against the line's: once they are held at the end, what they overflow is to the left,
  // which the line's scroll width does not count, so measuring that would undo the change it made.
  useLayoutEffect(() => {
    if (line.current && words.current) setFull(words.current.offsetWidth > line.current.clientWidth);
  });
  return (
    <div className="thinking thinking-live" title={text.slice(-2000)}>
      {/* The words twice, the second in jade over the first, which comes and goes: said once to a screen reader. */}
      <span className="label" data-lit="推演中：">
        推演中：
      </span>
      <span ref={line} className={`tail ${full ? "full" : ""}`}>
        <span ref={words}>{tail}</span>
      </span>
    </div>
  );
}

function Streaming({ blocks }: { blocks: StreamBlock[] }) {
  return (
    <>
      {blocks.map((block) =>
        block.kind === "text" ? (
          <div key={block.index} className="prose">
            <Markdown components={markdownParts} remarkPlugins={markdownSyntax} rehypePlugins={markdownPlugins}>
              {block.text}
            </Markdown>
          </div>
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

/** Fewer acts than this are simply listed; this many that only looked, one after another, fold under one line. */
const FOLD = 4;

type Part = { kind: "item"; item: Item; key: string } | { kind: "act"; tool: ToolItem } | { kind: "fold"; tools: ToolItem[] };

/** Whether a tool call is shown as a line: not a request waiting for 本尊, and not a party of 子分身. */
const isLine = (item: Item): boolean => item.kind === "tool" && item.name !== "delegate" && !item.question && !presented(item) && !(item.approval && !item.approval.decision && !item.result);

/** Whether an act only looked, and is done: such acts, several in a row, fold under one line. */
const looked = (tool: ToolItem) => weigh(tool) === "quiet" && !voided(tool);

/**
 * A turn's items as they are laid out, one after another with nothing nested, so that an act keeps its place
 * on the page (and what was opened in it) whatever comes to stand beside it. A stretch of acts that only looked
 * is folded under one line once there are enough of them, and stays folded as more join it: a 分身 reads a
 * great deal on its way to doing a little. An act still under way is not part of a fold until it is done.
 */
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
      // Too few to fold, or not an act that only looked: each on its own line.
      const stop = Math.max(end, at + 1);
      for (const tool of items.slice(at, stop) as ToolItem[]) out.push({ kind: "act", tool });
      at = stop;
    }
  }
  return out;
}

/** Acts that only looked, folded under one line that says how many of each; opened, they are listed again. */
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

interface Props {
  rows: Row[];
  stream: StreamBlock[];
  queued: string[];
  status: Status;
  mode: Mode;
  /** Who gave the session its first message: 本尊, the 分身 that set it off as a 子分身, or a 功课 that came due. */
  origin: "user" | "agent" | "schedule";
  /** Every session in the list, to tell how the 子分身 of this one stand, and the way to open one. */
  sessions: SessionItem[];
  /** In a fork: the last turn copied from the session it was forked from. 0 in any other session. */
  copied: number;
  /** The page shows the session as it stood at a point in its past (a replay): nothing on it acts. */
  frozen: boolean;
  onOpen: (id: string) => void;
  onDecide: (toolUseId: string, decision: Decision) => void;
  /** Answer the questions the 分身 asked: one reply each, or `null` to leave them to it. */
  onAnswer: (toolUseId: string, answers: Reply[] | null) => void;
  /** Undo the 分身's file edits from this turn on. */
  onUndo: (turn: number) => void;
  /** Start a fork that goes on from before this turn, with the turn's message to send again or change. Absent where a fork cannot be made. */
  onFork?: (turn: number, text: string) => void;
  /** Open a file attached to a message or written by the 分身 with the system's program for it (or a historical snapshot), or with `reveal` show it in its folder; answers what was done. */
  onOpenFile?: (path: string, reveal: boolean, snapshot?: string, version?: number) => Promise<string>;
  /** A picture the 分身 wrote, either from a recorded `snapshot` or as it is now. */
  onPicture?: (path: string, snapshot?: string) => Promise<Image>;
  /** 1-based version number for `(path, snapshot)` when the file has more than one version. */
  versionOf?: (path: string, snapshot?: string) => number | undefined;
  /** In a side conversation: hand what the 分身 said here to the main 分身, as words said to it during its work. */
  onForward?: (text: string) => void;
  /** Interrupt the step in flight so queued steering messages are heard immediately. */
  onDeliverNow?: () => void;
}

/** Undoing cannot itself be undone, so the icon asks once more, in a bubble, before it acts. */
function UndoButton({ turn, onUndo }: { turn: number; onUndo: (turn: number) => void }) {
  const [asking, setAsking] = useState(false);
  return (
    <span className="bubble-anchor">
      <button type="button" className="icon-button danger" aria-expanded={asking} title="撤回这一轮起的改动：分身从这一轮起改的文件恢复原样" onClick={() => setAsking(!asking)}>
        <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
          <path d="M5.5 6.5H10a3 3 0 0 1 0 6H6.5M7.6 4 5 6.5 7.6 9" />
        </svg>
        <span className="sr-only">撤回这一轮起的改动</span>
      </button>
      {asking && (
        <ConfirmBubble label="撤回这一轮起的改动" confirm="确认撤回" onConfirm={() => (setAsking(false), onUndo(turn))} onCancel={() => setAsking(false)}>
          <p>分身从这一轮起改的文件都会恢复原样，它新建的会删掉。</p>
        </ConfirmBubble>
      )}
    </span>
  );
}

/**
 * What a message says. It stays pinned over the work it asked for, so a long one is held to two lines until it
 * is opened, and the way to open it shows only when there is more to see.
 */
function SaidText({ text }: { text: string }) {
  const body = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [more, setMore] = useState(false);
  // Measured while held: whether its lines go past the two it is held to. Open, it is left as it was found.
  useLayoutEffect(() => {
    if (!open && body.current) setMore(body.current.scrollHeight > body.current.clientHeight + 1);
  });
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

/** A message sent while the 分身 works, which it has not heard yet. */
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
            <svg aria-hidden="true" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              <path d="M2.2 3.2 13.8 8 2.2 12.8l2.2-4.8-2.2-4.8zM4.4 8h4.2" />
            </svg>
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

export function Transcript({ rows, stream, queued, status, mode, origin, sessions, copied, frozen, onOpen, onDecide, onAnswer, onUndo, onFork, onOpenFile, onPicture, versionOf, onForward, onDeliverNow }: Props) {
  const family = { sessions, onOpen };
  const undoable = undoableTurns(rows, copied);
  /** Where a fork's own conversation begins: said once, between what it copied and what it did itself. */
  const seam = <div className="seam">以上是分叉时带过来的前情，以下是这个分叉自己的</div>;
  // What comes onto the page a little at a time is happening now; what comes all at once is the log being read.
  const live = useRef(false);
  const seen = useRef<number | null>(null);
  const count = rows.reduce((sum, row) => sum + row.items.length, 0);
  live.current = seen.current !== null && (status !== "idle" || frozen) && count - seen.current <= 12;
  useLayoutEffect(() => {
    seen.current = count;
  });
  /** What came of opening an attached file, under its message, for a moment: why it could not be, or that it was shown in its folder instead. */
  const [fileNote, setFileNote] = useState<{ path: string; text: string; failed: boolean } | null>(null);
  const noteTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(noteTimer.current), []);
  const openFile = (path: string, reveal: boolean) => {
    if (!onOpenFile) return;
    clearTimeout(noteTimer.current);
    setFileNote(null);
    onOpenFile(path, reveal)
      .then(
        (done) => {
          // A program is not run from here: it is shown in its folder, and that is said.
          if (done === "revealed" && !reveal) setFileNote({ path, text: "这是可执行文件，不从这里直接运行，已在文件夹中显示。", failed: false });
        },
        (error) => setFileNote({ path, text: String(error), failed: true }),
      )
      .finally(() => {
        noteTimer.current = setTimeout(() => setFileNote(null), 5000);
      });
  };
  /** The pictures of a message being looked at full size, and which of them. */
  const [viewing, setViewing] = useState<{ images: Image[]; at: number } | null>(null);
  /** For a moment after the 分身 finishes: the light on the turn's thread goes back up to the order. */
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
      // The next turn has begun before the light was home: it is that turn's light now.
      clearTimeout(closes.current);
      setClosing(false);
    }
  }, [status]);
  useEffect(() => () => clearTimeout(closes.current), []);
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  /** How many messages of 本尊's were on the page at the last render. */
  const said = useRef(rows.length + queued.length);
  // Follow new content only while the reader is already at the bottom. Before the page is painted,
  // so that a new message is never seen where it is not going to stay.
  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const count = rows.length + queued.length;
    // A message 本尊 just sent has come onto the page: go to it wherever the reader was, and fly it in.
    const arriving = count > said.current && inFlight();
    said.current = count;
    if (arriving) pinned.current = true;
    if (pinned.current) element.scrollTop = element.scrollHeight;
    if (arriving) {
      const messages = element.querySelectorAll<HTMLElement>(".turn-said .original, .turn-said .shots");
      land(messages[messages.length - 1] ?? null);
    }
  });
  // One for the life of the page's callbacks, so the cards under it are not drawn again for nothing.
  const files = useMemo<Files | null>(() => (onOpenFile && onPicture ? { open: onOpenFile, picture: onPicture, versionOf } : null), [onOpenFile, onPicture, versionOf]);
  return (
    <FilesContext.Provider value={files}>
      <Live.Provider value={live}>
        <div
          className="transcript"
          ref={scroller}
          onScroll={(event) => {
            const element = event.currentTarget;
            pinned.current = element.scrollHeight - element.scrollTop - element.clientHeight < 80;
          }}
        >
          {rows.map((row, index) => {
            const last = index === rows.length - 1;
            // The first message of such a session is a task handed to it, not something 本尊 said then.
            const task = index === 0 && origin !== "user" ? origin : null;
            const ownFirst = copied > 0 && row.turn > copied && (index === 0 || rows[index - 1].turn <= copied);
            // Who the message is from, said only when it is not simply 本尊's order: the seal says the rest.
            const who = task === "agent" ? "主分身交办" : task === "schedule" ? "功课交办" : row.prompt.steering ? "中途传音" : null;
            const canUndo = !frozen && status === "idle" && !row.prompt.steering && undoable.has(row.turn);
            const canFork = !!onFork && !row.prompt.steering && row.turn > 0;
            return (
              <section key={row.key} className={`turn ${last ? "latest" : ""} ${last && closing ? "closing" : ""}`}>
                {ownFirst && seam}
                <div className="turn-said">
                  {row.prompt.images.length > 0 && (
                    <div className="shots">
                      {row.prompt.images.map((image, at) => (
                        <button key={at} type="button" className="shot" title="点开看大图" onClick={() => setViewing({ images: row.prompt.images, at })}>
                          <img src={imageSource(image)} alt={`本尊附的第 ${at + 1} 张图`} />
                        </button>
                      ))}
                    </div>
                  )}
                  {/* Over the card's right end, small: the way to undo the work from here, to fork from here, and when it was said. */}
                  <div className="said-top">
                    {canUndo && <UndoButton turn={row.turn} onUndo={onUndo} />}
                    {canFork && (
                      <button
                        type="button"
                        className="icon-button"
                        title="从这里分叉：另起一个分身，带着这句话之前的对话，这句话放回输入框，改了再传。只分对话，洞府里的文件还是现在的样子"
                        onClick={() => onFork?.(row.turn, row.prompt.text)}
                      >
                        <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                          <path d="M8 14V9.5M8 9.5C8 6.5 4 7 4 3.5M8 9.5c0-3 4-2.5 4-6M2.5 5 4 3.5 5.5 5M10.5 5 12 3.5 13.5 5" />
                        </svg>
                        <span className="sr-only">从这里分叉</span>
                      </button>
                    )}
                    <span className="said-when">
                      {who && `${who}，`}
                      {time(row.prompt.ts)}
                    </span>
                  </div>
                  {/* The message, laid out as the 分身's own cards are: its seal first, then the words. */}
                  <div className="original" title={`${who ?? "本尊"}，${time(row.prompt.ts)}`}>
                    <span className="seal" aria-hidden="true">
                      {task === "agent" ? "遣" : task === "schedule" ? "课" : row.prompt.steering ? "音" : "令"}
                    </span>
                    {/* A message of pictures alone has no words to show. */}
                    {row.prompt.text && <SaidText text={row.prompt.text} />}
                  </div>
                  {/* Under the message: the files that came with it, each by its name. */}
                  {row.prompt.files.length > 0 && (
                    <div className="said-files">
                      {row.prompt.files.map((file) => {
                        const how = file.inline ? (file.document ? "取出的文字随这句话一起给了分身，图和排版不在其中" : "它的内容随这句话一起给了分身") : "它太大，没有随这句话发出，分身按路径自己去读";
                        return (
                          <span key={file.path} className="file-chip openable">
                            <button type="button" className="file-chip-open" disabled={!onOpenFile} title={`${file.path}\n${how}\n点开：用系统默认的程序打开`} onClick={() => openFile(file.path, false)}>
                              <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                                <path d="M4 1.8h5.2L12 4.6v9.6H4zM9.2 1.8v2.8H12M6 8h4M6 10.6h4" />
                              </svg>
                              <span className="file-chip-name">{file.path.replace(/^.*[\\/]/, "")}</span>
                              <span className="file-chip-size">
                                {fileSize(file.bytes)}
                                {file.document ? `，${documentLabel(file.document, file.parts)}` : ""}
                              </span>
                            </button>
                            <button type="button" className="file-chip-reveal" disabled={!onOpenFile} title="在文件夹中显示" onClick={() => openFile(file.path, true)}>
                              <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                                <path d="M1.8 4.2V12.6h12.4V5.8H7.6L6.2 4.2z" />
                              </svg>
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
                  {/* Words sent after the order while the 分身 works wait here, under what it is working on, until it hears them. */}
                  {last && queued.map((text, at) => <Queued key={at} text={text} onDeliverNow={frozen ? undefined : onDeliverNow} />)}
                </div>
                <div className={`turn-did ${last && status === "running" ? "live" : ""}`}>
                  {parts(row.items).map((part) =>
                    part.kind === "act" ? (
                      <ToolCard key={part.tool.id} tool={part.tool} />
                    ) : part.kind === "fold" ? (
                      // Named for the act it starts with, which stays as the fold grows.
                      <Folded key={`fold-${part.tools[0].id}`} tools={part.tools} />
                    ) : (
                      <ItemView key={part.key} item={part.item} mode={mode} family={family} frozen={frozen} onDecide={onDecide} onAnswer={onAnswer} onForward={frozen ? undefined : onForward} />
                    ),
                  )}
                  {last && <Streaming blocks={stream} />}
                  {/* Nothing of its own yet: the seam closes what was copied. */}
                  {last && copied > 0 && row.turn <= copied && seam}
                  {/* The light on the thread: down it while the 分身 works, back up it once when it is done. */}
                  {last && (status === "running" || closing) && <span className="vein" aria-hidden="true" />}
                  {last && status === "running" && <span className="vein-tip" aria-hidden="true" />}
                </div>
              </section>
            );
          })}
          {/* With no turn on the page yet to wait under, they stand by themselves. */}
          {rows.length === 0 && queued.length > 0 && (
            <section className="turn">
              <div className="turn-said">
                {queued.map((text, at) => (
                  <Queued key={at} text={text} onDeliverNow={frozen ? undefined : onDeliverNow} />
                ))}
              </div>
            </section>
          )}
          {viewing && <Viewer images={viewing.images} at={viewing.at} whose="本尊附的" onClose={() => setViewing(null)} />}
        </div>
      </Live.Provider>
    </FilesContext.Provider>
  );
}

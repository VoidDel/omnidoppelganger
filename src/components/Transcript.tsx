import { type ReactNode, useEffect, useRef, useState } from "react";
import Markdown from "react-markdown";
import { api } from "../lib/api";
import { type Item, type Row, type StreamBlock, type ToolItem, shortCount, undoableTurns } from "../lib/fold";
import type { Decision, DiffHunk, Mode, Status } from "../lib/types";

const EXTERNAL = /^(https?:|mailto:)/i;

/**
 * A link in the model's text. Following it inside the window would replace the
 * app with the page, so web links open in the system browser and anything
 * else (relative paths, anchors, other schemes) stays plain text.
 */
function Link({ href, children }: { href?: string; children?: ReactNode }) {
  if (!href || !EXTERNAL.test(href)) return <span title={href}>{children}</span>;
  return (
    <a
      href={href}
      title={href}
      onClick={(event) => {
        event.preventDefault();
        api.openExternal(href).catch((error) => console.error(error));
      }}
    >
      {children}
    </a>
  );
}

const markdownParts = { a: Link };

const time =(ts: string) => new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

function Diff({ hunk }: { hunk: DiffHunk }) {
  return (
    <div className="diff">
      {hunk.lines.map((line, index) => (
        <div key={index} className={`diff-line ${line.kind === "+" ? "added" : line.kind === "-" ? "removed" : ""}`}>
          <span className="sign">{line.kind === "-" ? "−" : line.kind}</span>
          <span className="text">{line.text || " "}</span>
        </div>
      ))}
    </div>
  );
}

/** The single character stamped on a tool card: read, search, edit, create, run. */
function sealOf(tool: ToolItem): string {
  switch (tool.name) {
    case "read_file":
      return "读";
    case "search":
      return "搜";
    case "find_files":
      return "寻";
    case "edit_file":
    case "write_file":
      return tool.result?.meta?.created ? "建" : "改";
    case "run_command":
      return "行";
    case "command_output":
      return "观";
    case "stop_command":
      return "止";
    default:
      return "术";
  }
}

/** What a finished or running tool call says in its one-line header. */
function describe(tool: ToolItem): { verb: string; target: string; outcome: string } {
  const text = (name: string) => (typeof tool.input[name] === "string" ? (tool.input[name] as string) : "");
  const { result } = tool;
  const meta = result?.meta;
  const declined = tool.approval?.decision === "deny";
  const waiting = declined ? "本尊驳回了" : meta?.refused ? "先谋后动，没有改" : tool.undone ? "本尊已撤回" : result ? "" : "运功中";
  switch (tool.name) {
    case "read_file":
      return { verb: "读取", target: text("path"), outcome: waiting || (result!.isError ? "没读到" : `${meta?.lines ?? 0} 行`) };
    case "search":
      return {
        verb: "搜索",
        target: text("pattern"),
        outcome: waiting || (result!.isError ? "搜索失败" : `${meta?.matches ?? 0} 处${meta?.truncated ? "以上" : ""}`),
      };
    case "find_files":
      return {
        verb: text("glob") ? "查找" : "列出",
        target: text("glob") || text("path") || "洞府",
        outcome: waiting || (result!.isError ? "没找成" : `${meta?.entries ?? 0} 项${meta?.truncated ? "以上" : ""}`),
      };
    case "edit_file":
    case "write_file":
      return {
        verb: meta?.created ? "新建" : tool.name === "write_file" ? "重写" : "修改",
        target: text("path"),
        outcome: waiting || (result!.isError ? "没改成" : `加 ${meta?.added ?? 0} 行，删 ${meta?.removed ?? 0} 行`),
      };
    case "run_command": {
      const ended = meta?.stopped === "timed out" ? "超时停止" : meta?.stopped ? "已停止" : meta?.exit_code === 0 ? "成功" : `退出码 ${meta?.exit_code ?? "未知"}`;
      if (meta?.background) return { verb: "后台运行", target: text("command"), outcome: waiting || `已起，编号 ${meta.job}` };
      return { verb: "运行", target: text("command"), outcome: waiting || (meta ? ended : "没有运行") };
    }
    case "command_output":
    case "stop_command": {
      // A background command, by its id and command line: still running, stopped, or ended by itself.
      const state = meta?.running ? "还在跑" : meta?.stopped ? "已停止" : meta?.exit_code === 0 ? "已结束，成功" : `已结束，退出码 ${meta?.exit_code ?? "未知"}`;
      return {
        verb: tool.name === "stop_command" ? "停止后台命令" : "看后台输出",
        target: meta?.command ? `${text("job")}  ${meta.command}` : text("job"),
        outcome: waiting || (meta?.command ? state : "没有这个后台命令"),
      };
    }
    default:
      return { verb: tool.name, target: "", outcome: waiting || (result!.isError ? "失败" : "完成") };
  }
}

function ToolCard({ tool }: { tool: ToolItem }) {
  const { verb, target, outcome } = describe(tool);
  const { result } = tool;
  const hunk = result && !result.isError ? result.meta?.hunk : undefined;
  const output = tool.name === "run_command" || tool.name === "command_output" || tool.name === "stop_command" ? result?.meta?.output : undefined;
  // A failed call shows what went wrong; a finished command keeps its output one click away.
  const failure = result?.isError && tool.name !== "run_command" && !result.meta?.command && tool.approval?.decision !== "deny" && !result.meta?.refused ? result.content : undefined;
  const body = hunk ? <Diff hunk={hunk} /> : output ? <pre>{output}</pre> : failure ? <pre>{failure}</pre> : null;
  const [open, setOpen] = useState(() => !!hunk || !!failure || (!!output && !!result?.isError));
  return (
    <div className="did">
      <button type="button" className="did-head" aria-expanded={body ? open : undefined} disabled={!body} onClick={() => setOpen(!open)}>
        <span className="seal" aria-hidden="true">
          {sealOf(tool)}
        </span>
        <span className="verb">{verb}</span>
        <span className="target" title={target}>
          {target}
        </span>
        <span className="outcome">{outcome}</span>
      </button>
      {body && open && <div className="did-body">{body}</div>}
    </div>
  );
}

const ASK: Record<string, { title: string; note: string; always: string }> = {
  command: { title: "分身要运行一条命令，请本尊定夺", note: "命令会在洞府（这个文件夹）内执行。", always: "之后同类命令都准" },
  edit: { title: "分身要改一个文件，请本尊定夺", note: "眼下的禁制是步步请示，每次改文件都先请示。整文件写入时，下面是它要写进去的全部内容。", always: "之后改文件都准" },
  outside_write: { title: "分身要改洞府之外的文件，请本尊定夺", note: "这个路径不在当前文件夹里。", always: "之后这类修改都准" },
  outside_read: { title: "分身要读洞府之外的内容，请本尊定夺", note: "这个路径不在当前文件夹里。", always: "之后这类读取都准" },
  git_write: {
    title: "分身要改 .git 里的文件，请本尊定夺",
    note: "这里放着 Git 的钩子和配置，改动后你自己运行 git 时可能执行其中的代码，所以每次都要请示。",
    always: "",
  },
};

/** What the session-wide button says. A command grant names the prefix it covers, e.g. `command:cargo test`. */
function alwaysLabel(grant: string, fallback: string): string {
  return grant.startsWith("command:") ? `之后以「${grant.slice("command:".length)}」开头的命令都准` : fallback;
}

/** The change an edit asks to make, drawn like a diff: what goes out, then what comes in. */
function proposed(tool: ToolItem): DiffHunk | null {
  const text = (name: string) => (typeof tool.input[name] === "string" ? (tool.input[name] as string) : "");
  const lines = (value: string, kind: "-" | "+") => (value ? value.replace(/\n$/, "").split("\n").map((line) => ({ kind, text: line })) : []);
  // One edit in the call's own fields, several in `edits`, or a whole file's new contents.
  const edits = Array.isArray(tool.input.edits) ? (tool.input.edits as Record<string, unknown>[]) : [tool.input];
  const of = (edit: Record<string, unknown>, name: string) => (typeof edit[name] === "string" ? (edit[name] as string) : "");
  const changed = tool.name === "write_file" ? lines(text("content"), "+") : edits.flatMap((edit) => [...lines(of(edit, "old_string"), "-"), ...lines(of(edit, "new_string"), "+")]);
  return changed.length ? { old_start: 0, new_start: 0, lines: changed } : null;
}

interface SignSheetProps {
  tool: ToolItem;
  mode: Mode;
  onDecide: (toolUseId: string, decision: Decision) => void;
}

function SignSheet({ tool, mode, onDecide }: SignSheetProps) {
  const approval = tool.approval!;
  const ask = ASK[approval.reason] ?? ASK.command;
  const oneByOne = approval.reason === "command" && !approval.grant;
  const change = approval.reason === "edit" ? proposed(tool) : null;
  return (
    <div className="sign-sheet">
      <h2>{ask.title}</h2>
      <div className="sign-detail">{approval.detail}</div>
      {change && <Diff hunk={change} />}
      <p>
        {ask.note}
        {approval.reason === "command" && mode === "auto" && "放手施为只自行放行只读、构建、测试类且不碰洞府之外的命令，这条不在其列。"}
        {oneByOne && "这条命令带有管道、分号、引号、变量之类的写法，或者会执行任意代码，只能逐条批准。"}
      </p>
      <div className="actions">
        <button type="button" className="button solid" onClick={() => onDecide(tool.id, "allow_once")}>
          准这一次
        </button>
        {approval.grant && (
          <button type="button" className="button outline red" onClick={() => onDecide(tool.id, "allow_session")}>
            {alwaysLabel(approval.grant, ask.always)}
          </button>
        )}
        {approval.grant?.startsWith("command:") && (
          <button type="button" className="button outline red" title="重启应用之后也算数，可以在设置里撤销" onClick={() => onDecide(tool.id, "allow_always")}>
            这处洞府里永远都准
          </button>
        )}
        <button type="button" className="button text" onClick={() => onDecide(tool.id, "deny")}>
          驳回
        </button>
      </div>
    </div>
  );
}

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
        ? "还没有这个功法出处（模型供应商）的 API 密钥，分身出不了关。到设置里填上，再传一次令。"
        : `这一轮走火了，已经停下：${error ?? "原因不明"}`;
  }
}

function ItemView({ item, mode, onDecide }: { item: Item; mode: Mode; onDecide: (toolUseId: string, decision: Decision) => void }) {
  switch (item.kind) {
    case "text":
      return (
        <div className="prose">
          <Markdown components={markdownParts}>{item.text}</Markdown>
        </div>
      );
    case "thinking":
      return (
        <details className="thinking">
          <summary>分身推演了一番</summary>
          <p>{item.text}</p>
        </details>
      );
    case "tool":
      return item.approval && !item.approval.decision && !item.result ? <SignSheet tool={item} mode={mode} onDecide={onDecide} /> : <ToolCard tool={item} />;
    case "fallback":
      return (
        <div className="notice">
          {item.from} 没有接这个请求，改由 {item.to} 继续。
        </div>
      );
    case "compacted":
      return (
        <details className="thinking">
          <summary>前情太长（约 {shortCount(item.tokensBefore)} token），分身把它凝成了一段摘要，之后只凭摘要接着做</summary>
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
    case "notice":
      return <div className="notice">{notice(item.reason, item.error)}</div>;
  }
}

function Streaming({ blocks }: { blocks: StreamBlock[] }) {
  return (
    <>
      {blocks.map((block) =>
        block.kind === "text" ? (
          <div key={block.index} className="prose">
            <Markdown components={markdownParts}>{block.text}</Markdown>
          </div>
        ) : block.kind === "thinking" ? (
          <div key={block.index} className="thinking">
            推演中：{block.text.slice(-160)}
          </div>
        ) : (
          <div key={block.index} className="did">
            <div className="did-head">
              <span className="seal" aria-hidden="true">
                起
              </span>
              <span className="verb">起手</span>
              <span className="target">{block.name}</span>
              <span className="outcome">蓄势中</span>
            </div>
          </div>
        ),
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
  onDecide: (toolUseId: string, decision: Decision) => void;
  /** Undo the 分身's file edits from this turn on. */
  onUndo: (turn: number) => void;
}

/** Undoing cannot itself be undone, so the button asks once more before it acts. */
function UndoButton({ turn, onUndo }: { turn: number; onUndo: (turn: number) => void }) {
  const [asking, setAsking] = useState(false);
  if (!asking) {
    return (
      <button type="button" className="button text small" onClick={() => setAsking(true)}>
        撤回这一轮起的改动
      </button>
    );
  }
  return (
    <span className="undo-confirm">
      分身从这一轮起改的文件都会恢复原样，它新建的会删掉。
      <button type="button" className="button outline red small" onClick={() => (setAsking(false), onUndo(turn))}>
        确认撤回
      </button>
      <button type="button" className="button text small" onClick={() => setAsking(false)}>
        算了
      </button>
    </span>
  );
}

export function Transcript({ rows, stream, queued, status, mode, onDecide, onUndo }: Props) {
  const undoable = undoableTurns(rows);
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  // Follow new content only while the reader is already at the bottom.
  useEffect(() => {
    const element = scroller.current;
    if (element && pinned.current) element.scrollTop = element.scrollHeight;
  });
  return (
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
        return (
          <section key={row.key} className="turn">
            <div className="turn-said">
              <div className={`original ${last && status === "running" ? "working" : ""}`}>{row.prompt.text}</div>
              <span className="said-meta">
                <span className="seal small" aria-hidden="true">
                  {row.prompt.steering ? "音" : "令"}
                </span>
                <span>
                  本尊，{time(row.prompt.ts)}
                  {row.prompt.steering ? "，中途传音" : ""}
                </span>
              </span>
              {status === "idle" && !row.prompt.steering && undoable.has(row.turn) && <UndoButton turn={row.turn} onUndo={onUndo} />}
            </div>
            <div className="turn-did">
              {row.items.map((item, at) => (
                <ItemView key={item.kind === "tool" ? item.id : at} item={item} mode={mode} onDecide={onDecide} />
              ))}
              {last && <Streaming blocks={stream} />}
            </div>
          </section>
        );
      })}
      {queued.map((text, index) => (
        <section key={`queued-${index}`} className="turn">
          <div className="turn-said">
            <div className="original queued">{text}</div>
            <span className="said-meta">传音在途，分身做完这一步就会听到</span>
          </div>
          <div className="turn-did" />
        </section>
      ))}
    </div>
  );
}

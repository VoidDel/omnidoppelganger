import { type ReactNode, useLayoutEffect, useRef, useState } from "react";
import { inFlight, land } from "../lib/flight";
import Markdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { api } from "../lib/api";
import { type Item, type Row, type StreamBlock, type ToolItem, mcpTool, shortCount, undoableTurns } from "../lib/fold";
import type { Decision, DiffHunk, Mode, SessionItem, Status } from "../lib/types";

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

/** A code block from the model's text, with a button that copies it. */
function CodeBlock({ children }: { children?: ReactNode }) {
  const block = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);
  const copy = () => {
    navigator.clipboard.writeText(block.current?.textContent ?? "").then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      (error) => console.error(error),
    );
  };
  return (
    <div className="code-block">
      <button type="button" className="button text small copy" onClick={copy}>
        {copied ? "已复制" : "复制"}
      </button>
      <pre ref={block}>{children}</pre>
    </div>
  );
}

/** A table in its own box, so that a wide one scrolls sideways there instead of pushing the page out. */
function Table({ children }: { children?: ReactNode }) {
  return (
    <div className="table-wrap">
      <table>{children}</table>
    </div>
  );
}

const markdownParts = { a: Link, pre: CodeBlock, table: Table };
// Tables, task lists, and struck-out words, as models write them.
const markdownSyntax = [remarkGfm];
// Colors code by its language; a block in a language it does not know stays plain.
const markdownPlugins = [rehypeHighlight];

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
    case "update_todos":
      return "谋";
    case "delegate":
      return "遣";
    case "load_skill":
      return "技";
    case "fetch_url":
      return "网";
    case "web_search":
      return "询";
    default:
      // A tool of an MCP server: an instrument borrowed from outside.
      return mcpTool(tool.name) ? "器" : "术";
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
    case "web_search":
      return { verb: "联网搜索", target: text("query"), outcome: waiting || (result!.isError ? "没搜成" : `${meta?.results ?? 0} 条`) };
    case "fetch_url":
      return { verb: "抓取网页", target: text("url"), outcome: waiting || (result!.isError ? "没取到" : `${shortCount(meta?.characters ?? 0)} 字`) };
    case "load_skill":
      return { verb: "研读技能", target: text("name"), outcome: waiting || (result!.isError ? "没有这门技能" : "已读") };
    case "update_todos": {
      const list = meta?.todos ?? [];
      const current = list.find((todo) => todo.status === "in_progress")?.text ?? "";
      return { verb: "记计划", target: current, outcome: waiting || (result!.isError ? "没记成" : `${list.filter((todo) => todo.status === "done").length} / ${list.length} 已办`) };
    }
    default: {
      const borrowed = mcpTool(tool.name);
      if (borrowed) return { verb: `法器 ${borrowed.server}`, target: borrowed.tool, outcome: waiting || (result!.isError ? "失败" : "完成") };
      return { verb: tool.name, target: "", outcome: waiting || (result!.isError ? "失败" : "完成") };
    }
  }
}

/** Where the 子分身 of the session on screen are looked up, and how one is opened. */
interface Family {
  sessions: SessionItem[];
  onOpen: (id: string) => void;
}

/**
 * A `delegate` call: the 子分身 it set off, each with how it stands and a way
 * in. One that waits for 本尊 is marked in cinnabar, since its request is on
 * its own page, not this one.
 */
function DelegateCard({ tool, family }: { tool: ToolItem; family: Family }) {
  const tasks = Array.isArray(tool.input.tasks) ? (tool.input.tasks as Record<string, unknown>[]) : [];
  const text = (value: unknown) => (typeof value === "string" ? value : "");
  const ended = tool.result?.meta?.agents ?? [];
  // A task gets its 子分身 as each one starts; until then, and if it never does, the task stands alone.
  const agents = tasks.map((task, at) => {
    const started = tool.agents?.[at];
    const live = started && family.sessions.find((session) => session.id === started.session);
    const reason = ended[at]?.reason;
    const state =
      live?.status === "waiting_approval" ? "await" : live?.status === "running" || (started && !tool.result && !live) ? "busy" : reason === "completed" ? "done" : reason || !started ? (tool.result ? "failed" : "pending") : "done";
    return { name: started?.name ?? (text(task.name) || `子分身 ${at + 1}`), task: text(task.task), session: started?.session, state };
  });
  const STATES: Record<string, string> = { await: "候旨", busy: "运功中", done: "已回报", failed: "没办成", pending: "待出发" };
  const count = (state: string) => agents.filter((agent) => agent.state === state).length;
  const outcome = tool.approval?.decision === "deny" ? "本尊驳回了" : !tool.result ? (count("await") ? `${count("await")} 个候旨` : "运功中") : `${count("done")} / ${agents.length} 已回报`;
  // A call the model got wrong sets nobody off; what was wrong is all there is to show.
  const failure = tool.result?.isError && agents.length === 0 ? tool.result.content : undefined;
  return (
    <div className="did delegate">
      <div className="did-head">
        <span className="seal" aria-hidden="true">
          遣
        </span>
        <span className="verb">派出子分身</span>
        <span className="target">{agents.length} 个，各办一件</span>
        <span className="outcome">{outcome}</span>
      </div>
      {failure && (
        <div className="did-body">
          <pre>{failure}</pre>
        </div>
      )}
      {agents.length > 0 && (
        <ul className="agents">
          {agents.map((agent, at) => (
            <li key={at}>
              <button type="button" className="agent" disabled={!agent.session} title={agent.session ? "去看这个子分身做了什么" : undefined} onClick={() => agent.session && family.onOpen(agent.session)}>
                <span className={`chip ${agent.state}`}>{STATES[agent.state]}</span>
                <span className="agent-name">{agent.name}</span>
                <span className="agent-task">{agent.task}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ToolCard({ tool }: { tool: ToolItem }) {
  const { verb, target, outcome } = describe(tool);
  const { result } = tool;
  const hunk = result && !result.isError ? result.meta?.hunk : undefined;
  const output = tool.name === "run_command" || tool.name === "command_output" || tool.name === "stop_command" ? result?.meta?.output : undefined;
  // A failed call shows what went wrong; a finished command keeps its output one click away.
  // What an MCP tool returned is its whole result, so it is kept one click away like a command's output.
  const returned = (result?.meta?.mcp || tool.name === "web_search") && !result?.isError ? result?.content : undefined;
  const failure = result?.isError && tool.name !== "run_command" && !result.meta?.command && tool.approval?.decision !== "deny" && !result.meta?.refused ? result.content : undefined;
  const body = hunk ? <Diff hunk={hunk} /> : output ? <pre>{output}</pre> : returned ? <pre>{returned}</pre> : failure ? <pre>{failure}</pre> : null;
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
  search: {
    title: "分身要联网搜索，请本尊定夺",
    note: "搜索词会发给智谱的搜索服务，每搜一次都要花灵石（基础版约 0.01 元）。",
    always: "之后搜索都准",
  },
  web: {
    title: "分身要上网取一个网页，请本尊定夺",
    note: "这个请求会发到洞府之外，网址里带的内容对方都看得到。准了之后，同一个网站的其他网页不再请示。",
    always: "之后这个网站都准",
  },
  mcp: {
    title: "分身要动用一件外来的法器（MCP 工具），请本尊定夺",
    note: "这件法器来自你在设置里接入的 MCP 服务，它具体做什么由那个服务决定，所以无论哪种禁制都要先请示。",
    always: "之后这件法器都准",
  },
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
        {(approval.grant === "search" || ["command:", "mcp:", "web:"].some((kind) => approval.grant?.startsWith(kind))) && (
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

function ItemView({ item, mode, family, onDecide }: { item: Item; mode: Mode; family: Family; onDecide: (toolUseId: string, decision: Decision) => void }) {
  switch (item.kind) {
    case "text":
      return (
        <div className="prose">
          <Markdown components={markdownParts} remarkPlugins={markdownSyntax} rehypePlugins={markdownPlugins}>
            {item.text}
          </Markdown>
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
      if (item.approval && !item.approval.decision && !item.result) return <SignSheet tool={item} mode={mode} onDecide={onDecide} />;
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
            <Markdown components={markdownParts} remarkPlugins={markdownSyntax} rehypePlugins={markdownPlugins}>
              {block.text}
            </Markdown>
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
  /** Who gave the session its first message: 本尊, the 分身 that set it off as a 子分身, or a 功课 that came due. */
  origin: "user" | "agent" | "schedule";
  /** Every session in the list, to tell how the 子分身 of this one stand, and the way to open one. */
  sessions: SessionItem[];
  onOpen: (id: string) => void;
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

/** A message sent while the 分身 works, which it has not heard yet. */
function Queued({ text }: { text: string }) {
  return (
    <div className="said-queued">
      <div className="original queued">{text || "（图）"}</div>
      <div className="said-foot">
        <span className="said-meta">传音在途，分身做完这一步就会听到</span>
      </div>
    </div>
  );
}

export function Transcript({ rows, stream, queued, status, mode, origin, sessions, onOpen, onDecide, onUndo }: Props) {
  const family = { sessions, onOpen };
  const undoable = undoableTurns(rows);
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
        // The first message of such a session is a task handed to it, not something 本尊 said then.
        const task = index === 0 && origin !== "user" ? origin : null;
        return (
          <section key={row.key} className={`turn ${last ? "latest" : ""}`}>
            <div className="turn-said">
              {row.prompt.images.length > 0 && (
                <div className="shots">
                  {row.prompt.images.map((image, at) => (
                    <img key={at} src={`data:${image.media_type};base64,${image.data}`} alt={`本尊附的第 ${at + 1} 张图`} />
                  ))}
                </div>
              )}
              {/* A message of pictures alone has no words to show. */}
              {(row.prompt.text || row.prompt.images.length === 0) && <div className={`original ${last && status === "running" ? "working" : ""}`}>{row.prompt.text}</div>}
              {/* Under the message, at its right: who said it and when, last in the line so it keeps its place. */}
              <div className="said-foot">
                {status === "idle" && !row.prompt.steering && undoable.has(row.turn) && <UndoButton turn={row.turn} onUndo={onUndo} />}
                <span className="said-meta">
                  <span className="seal small" aria-hidden="true">
                    {task === "agent" ? "遣" : task === "schedule" ? "课" : row.prompt.steering ? "音" : "令"}
                  </span>
                  <span>
                    {task === "agent" ? "主分身交办" : task === "schedule" ? "功课交办" : "本尊"}，{time(row.prompt.ts)}
                    {row.prompt.steering ? "，中途传音" : ""}
                  </span>
                </span>
              </div>
              {/* Words sent after the order while the 分身 works wait here, under what it is working on, until it hears them. */}
              {last && queued.map((text, at) => <Queued key={at} text={text} />)}
            </div>
            <div className="turn-did">
              {row.items.map((item, at) => (
                <ItemView key={item.kind === "tool" ? item.id : at} item={item} mode={mode} family={family} onDecide={onDecide} />
              ))}
              {last && <Streaming blocks={stream} />}
            </div>
          </section>
        );
      })}
      {/* With no turn on the page yet to wait under, they stand by themselves. */}
      {rows.length === 0 && queued.length > 0 && (
        <section className="turn">
          <div className="turn-said">
            {queued.map((text, at) => (
              <Queued key={at} text={text} />
            ))}
          </div>
        </section>
      )}
    </div>
  );
}

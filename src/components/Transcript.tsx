import { type CSSProperties, type ReactNode, createContext, isValidElement, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import { inFlight, land } from "../lib/flight";
import Markdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { api } from "../lib/api";
import { Light } from "./Light";
import { type Item, type Row, type StreamBlock, type ToolItem, editsFiles, fileSize, mcpTool, shortCount, undoableTurns } from "../lib/fold";
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

/** A code block from the model's text, with what it is written in and a button that copies it. */
function CodeBlock({ children }: { children?: ReactNode }) {
  const block = useRef<HTMLPreElement>(null);
  // The fence's own word for the language, which the highlighter leaves on the code as a class.
  const written = isValidElement<{ className?: string }>(children) ? /language-([\w+#.-]+)/.exec(children.props.className ?? "")?.[1] : undefined;
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
      <div className="code-tools">
        {written && <span className="lang">{written}</span>}
        <button type="button" className="button text small copy" onClick={copy}>
          {copied ? "已复制" : "复制"}
        </button>
      </div>
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

export function Diff({ hunk }: { hunk: DiffHunk }) {
  return (
    <div className="diff">
      {hunk.lines.map((line, index) => (
        <div key={index} className={`diff-line ${line.kind === "+" ? "added" : line.kind === "-" ? "removed" : ""}`}>
          <span className="sign">{line.kind === "-" ? "−" : line.kind}</span>
          <span className="text">
            {/^\s*/.exec(line.text)?.[0]}
            <span className="words">{line.text.trimStart() || " "}</span>
          </span>
        </div>
      ))}
    </div>
  );
}

/**
 * Whether what is put on the page now is put there as it happens, not read back from the log: an act that
 * arrives while the page is watched is stamped onto it; a page of old acts is simply shown.
 */
const Live = createContext<{ current: boolean }>({ current: false });

/** The things that came onto the page together last, counted, so that each can follow the one before it. */
const arrivals = { at: 0, count: 0 };

/**
 * Whether the thing that calls this came onto the page as it happened: decided once, when it comes. What it
 * gives is the class that says so and the thing's place among those that came with it, which sets how long
 * it waits its turn: several acts sent in one breath are stamped one after another, not all at once.
 */
function useFresh(): { className: string; style: CSSProperties | undefined } {
  const live = useContext(Live);
  const [nth] = useState(() => {
    if (!live.current) return null;
    const now = performance.now();
    if (now - arrivals.at > 60) arrivals.count = 0;
    arrivals.at = now;
    return Math.min(arrivals.count++, 12);
  });
  return nth === null ? { className: "", style: undefined } : { className: "fresh", style: { "--nth": nth } as CSSProperties };
}

/** How much an act weighs on the page, which its seal shows. */
type Weight = "live" | "failed" | "wrote" | "act" | "quiet";

/** Acts that only look: they change nothing and reach nowhere outside the machine. */
const LOOKS = new Set(["read_file", "search", "find_files", "load_skill", "update_todos", "command_output"]);

/** Whether nothing came of an act because it was not let to, or was taken back. */
const voided = (tool: ToolItem) => tool.approval?.decision === "deny" || !!tool.result?.meta?.refused || !!tool.undone;

function weigh(tool: ToolItem): Weight {
  const { result } = tool;
  // Turned down by 本尊, held back by 先谋后动, or taken back since: nothing came of it, and nothing went wrong.
  if (voided(tool)) return "quiet";
  if (!result) return "live";
  if (result.isError) return "failed";
  if (editsFiles(tool.name)) return "wrote";
  return LOOKS.has(tool.name) ? "quiet" : "act";
}

/** The single character stamped on an act: read, search, edit, create, run. */
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
  const waiting = declined ? (tool.approval?.timedOut ? "无人定夺，自动驳回" : "本尊驳回了") : meta?.refused ? "先谋后动，没有改" : tool.undone ? "本尊已撤回" : result ? "" : "运功中";
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
      // Said only where it holds: the command could write nowhere but the workspace and the temporary directories.
      const where = meta?.sandboxed ? "结界内，" : "";
      if (meta?.background) return { verb: "后台运行", target: text("command"), outcome: waiting || `${where}已起，编号 ${meta.job}` };
      return { verb: "运行", target: text("command"), outcome: waiting || (meta ? where + ended : "没有运行") };
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
      return { verb: "问天机阁", target: text("query"), outcome: waiting || (result!.isError ? "没问到" : `${meta?.results ?? 0} 条`) };
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
  const fresh = useFresh();
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
    <div className={`did delegate ${weigh(tool)} ${fresh.className}`} style={fresh.style}>
      <div className="did-head">
        <span className="seal" aria-hidden="true">
          遣
        </span>
        <span className="verb">派出子分身</span>
        <span className="target">{agents.length} 个，各办一件</span>
        <span className="leader" aria-hidden="true" />
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

/** One act, on one line: its seal, what was done to what, and how it came out. What it printed or changed opens under it. */
function ToolCard({ tool }: { tool: ToolItem }) {
  const fresh = useFresh();
  // Its place among those that came with it sets how long it waits to be shown. Having been shown under way,
  // it has waited: its seal comes down the moment it is done.
  const [begun] = useState(() => weigh(tool) === "live");
  const style = begun && weigh(tool) !== "live" && fresh.style ? ({ "--nth": 0 } as CSSProperties) : fresh.style;
  const { verb, target, outcome } = describe(tool);
  const { result } = tool;
  const hunk = result && !result.isError ? result.meta?.hunk : undefined;
  const output = tool.name === "run_command" || tool.name === "command_output" || tool.name === "stop_command" ? result?.meta?.output : undefined;
  // A failed call shows what went wrong; a finished command keeps its output one click away.
  // What an MCP tool returned is its whole result, so it is kept one click away like a command's output.
  const returned = (result?.meta?.mcp || tool.name === "web_search") && !result?.isError ? result?.content : undefined;
  const failure = result?.isError && tool.name !== "run_command" && !result.meta?.command && tool.approval?.decision !== "deny" && !result.meta?.refused ? result.content : undefined;
  const body = hunk ? <Diff hunk={hunk} /> : output ? <pre>{output}</pre> : returned ? <pre>{returned}</pre> : failure ? <pre>{failure}</pre> : null;
  // A change and a failure are open until closed, even when they come in after the line they belong to.
  const [chosen, setOpen] = useState<boolean | null>(null);
  const open = chosen ?? (!!hunk || !!failure || (!!output && !!result?.isError));
  return (
    <div className={`did ${weigh(tool)} ${voided(tool) ? "void" : ""} ${fresh.className}`} style={style}>
      <button type="button" className="did-head" aria-expanded={body ? open : undefined} disabled={!body} onClick={() => setOpen(!open)}>
        <span className="seal" aria-hidden="true">
          {weigh(tool) === "live" && <Light />}
          {sealOf(tool)}
        </span>
        <span className="verb">{verb}</span>
        <span className="target" title={target}>
          {target}
        </span>
        <span className="leader" aria-hidden="true" />
        <span className="outcome">{outcome}</span>
      </button>
      {/* Not 本尊's doing: the 护法 let it run, and says why. */}
      {tool.review?.allowed && (
        <div className="did-note" title={`护法是 ${tool.review.model}`}>
          护法放行{tool.review.reason ? `：${tool.review.reason}` : ""}
        </div>
      )}
      {body && open && <div className="did-body">{body}</div>}
    </div>
  );
}

const ASK: Record<string, { title: string; note: string; always: string }> = {
  command: { title: "分身要运行一条命令，请本尊定夺", note: "命令会在洞府（这个文件夹）内执行。", always: "之后同类命令都准" },
  unsandboxed: {
    title: "分身要出结界运行一条命令，请本尊定夺",
    note: "设置里布着结界（沙箱），命令本来只能往洞府和临时目录里写。分身说这一条得往别处写，所以要出结界跑：准了，它就能改这台机器上本尊能改的任何文件。出结界每次都要请示。",
    always: "",
  },
  search: {
    title: "分身要向天机阁打探（联网搜索），请本尊定夺",
    note: "要打探的话会发给智谱的搜索服务，每问一次都要花灵石（基础版约 0.01 元）。",
    always: "之后打探都准",
  },
  web: {
    title: "分身要上网取一个网页，请本尊定夺",
    note: "这个请求会发到洞府之外，网址里带的内容对方都看得到。准了之后，同一个网站的其他网页不再请示。",
    always: "之后这个网站都准",
  },
  mcp: {
    title: "分身要动用一件身外法器（MCP 工具），请本尊定夺",
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
  // A command of several is granted command by command, one per line.
  const commands = grant.split("\n").filter((part) => part.startsWith("command:")).map((part) => `「${part.slice("command:".length)}」`);
  return commands.length ? `之后以${commands.join("、")}开头的命令都准` : fallback;
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
  /** The page shows the past: the request is shown as it stood, with nothing to press. */
  frozen: boolean;
  onDecide: (toolUseId: string, decision: Decision) => void;
}

/** The seconds left until `expiresAt`, counted down while the page is open; null when there is no end. */
function useSecondsLeft(expiresAt: string | undefined, frozen: boolean): number | null {
  const left = () => (expiresAt ? Math.max(0, Math.ceil((new Date(expiresAt).getTime() - Date.now()) / 1000)) : null);
  const [seconds, setSeconds] = useState(left);
  useEffect(() => {
    if (!expiresAt || frozen) return;
    setSeconds(left());
    const timer = setInterval(() => setSeconds(left()), 250);
    return () => clearInterval(timer);
  }, [expiresAt, frozen]);
  return frozen ? null : seconds;
}

function SignSheet({ tool, mode, frozen, onDecide }: SignSheetProps) {
  const approval = tool.approval!;
  const seconds = useSecondsLeft(approval.expiresAt, frozen);
  /** Whether the explanation behind the mark after the title is open. */
  const [explained, setExplained] = useState(false);
  const ask = ASK[approval.reason] ?? ASK.command;
  const oneByOne = approval.reason === "command" && !approval.grant;
  const change = approval.reason === "edit" ? proposed(tool) : null;
  const why =
    ask.note +
    (approval.reason === "command" && mode === "auto" ? "放手施为只自行放行只读、构建、测试类且不碰洞府之外的命令，这条不在其列。" : "") +
    (oneByOne ? "这条命令带有管道、分号、引号、变量之类的写法，或者会执行任意代码，只能逐条批准。" : "");
  return (
    <div className={`sign-sheet ${frozen ? "frozen" : ""}`}>
      <div className="sign-head">
        <h2>{ask.title}</h2>
        {/* Why this asks and what allowing it means: a tip behind a mark, shown on hover or focus; a click keeps it open. */}
        <button type="button" className="sign-why" aria-expanded={explained} aria-label="这条请示的说明" onClick={() => setExplained(!explained)}>
          <span aria-hidden="true">!</span>
          <span className="tip" role="tooltip">
            {why}
          </span>
        </button>
        {/* In the corner, counting down: with nobody answering in time, the request declines itself. */}
        {seconds !== null && (
          <span className={`sign-countdown ${seconds <= 10 ? "urgent" : ""}`} role="timer" title="放手施为下不久等：到时无人定夺，就自动驳回，分身绕开它接着做">
            <strong>{seconds}</strong> 秒后自动驳回
          </span>
        )}
      </div>
      <div className="sign-detail">{approval.detail}</div>
      {/* Why it comes to 本尊 at all: the 护法 would not let it pass, or could not be asked. */}
      {tool.review && !tool.review.allowed && (
        <p className="sign-review">
          {tool.review.failed ? `护法没能审这一条（${tool.review.reason}），所以请本尊定夺。` : `护法没有放行${tool.review.reason ? `：${tool.review.reason}` : "。"}`}
        </p>
      )}
      {change && <Diff hunk={change} />}
      {frozen && <p className="muted">当时分身停在这里，候本尊定夺。</p>}
      <div className="actions" hidden={frozen}>
        <button type="button" className="button solid" onClick={() => onDecide(tool.id, "allow_once")}>
          准这一次
        </button>
        {approval.grant && (
          <button type="button" className="button outline red" onClick={() => onDecide(tool.id, "allow_session")}>
            {alwaysLabel(approval.grant, ask.always)}
          </button>
        )}
        {(approval.grant === "search" || ["command:", "mcp:", "web:"].some((kind) => approval.grant?.startsWith(kind))) && (
          <button type="button" className="button outline red" title="发一道通行符：重启应用之后也算数，可以在设置的「禁制」里收回" onClick={() => onDecide(tool.id, "allow_always")}>
            此府永准
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
        ? "还没有这个宗门（模型供应商）的 API 密钥，分身出不了关。到设置里填上，再传一次令。"
        : `这一轮走火了，已经停下：${error ?? "原因不明"}`;
  }
}

function ItemView({ item, mode, family, frozen, onDecide }: { item: Item; mode: Mode; family: Family; frozen: boolean; onDecide: (toolUseId: string, decision: Decision) => void }) {
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
    case "notice":
      return <div className="notice">{notice(item.reason, item.error)}</div>;
  }
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
const isLine = (item: Item): boolean => item.kind === "tool" && item.name !== "delegate" && !(item.approval && !item.approval.decision && !item.result);

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
  /** Undo the 分身's file edits from this turn on. */
  onUndo: (turn: number) => void;
  /** Start a fork that goes on from before this turn, with the turn's message to send again or change. Absent where a fork cannot be made. */
  onFork?: (turn: number, text: string) => void;
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
function Queued({ text }: { text: string }) {
  return (
    <div className="said-queued">
      <div className="original queued">
        <span className="seal" aria-hidden="true">
          音
        </span>
        <div className="said-text">{text || "（附件）"}</div>
      </div>
      <div className="said-foot">
        <span className="said-meta">传音在途，分身做完这一步就会听到</span>
      </div>
    </div>
  );
}

export function Transcript({ rows, stream, queued, status, mode, origin, sessions, copied, frozen, onOpen, onDecide, onUndo, onFork }: Props) {
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
  return (
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
                    <img key={at} src={`data:${image.media_type};base64,${image.data}`} alt={`本尊附的第 ${at + 1} 张图`} />
                  ))}
                </div>
              )}
              {/* Over the card's right end, small: the way to fork from here, and when it was said. */}
              <div className="said-top">
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
              <div className={`original ${last && status === "running" ? "working" : ""}`} title={`${who ?? "本尊"}，${time(row.prompt.ts)}`}>
                <span className="seal" aria-hidden="true">
                  {task === "agent" ? "遣" : task === "schedule" ? "课" : row.prompt.steering ? "音" : "令"}
                </span>
                {/* A message of pictures alone has no words to show. */}
                {row.prompt.text && <SaidText text={row.prompt.text} />}
              </div>
              {/* Under the message: the files that came with it, each by its name. */}
              {row.prompt.files.length > 0 && (
                <div className="said-files">
                  {row.prompt.files.map((file) => (
                    <span key={file.path} className="file-chip" title={`${file.path}\n${file.inline ? "它的内容随这句话一起给了分身" : "它太大，没有随消息发送：分身自己分段去读"}`}>
                      <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
                        <path d="M4 1.8h5.2L12 4.6v9.6H4zM9.2 1.8v2.8H12M6 8h4M6 10.6h4" />
                      </svg>
                      <span className="file-chip-name">{file.path.replace(/^.*[\\/]/, "")}</span>
                      <span className="file-chip-size">{fileSize(file.bytes)}</span>
                    </span>
                  ))}
                </div>
              )}
              {/* Under the message, at its right: what can be done from it. */}
              {canUndo && (
                <div className="said-foot">
                  <UndoButton turn={row.turn} onUndo={onUndo} />
                </div>
              )}
              {/* Words sent after the order while the 分身 works wait here, under what it is working on, until it hears them. */}
              {last && queued.map((text, at) => <Queued key={at} text={text} />)}
            </div>
            <div className={`turn-did ${last && status === "running" ? "live" : ""}`}>
              {parts(row.items).map((part) =>
                part.kind === "act" ? (
                  <ToolCard key={part.tool.id} tool={part.tool} />
                ) : part.kind === "fold" ? (
                  // Named for the act it starts with, which stays as the fold grows.
                  <Folded key={`fold-${part.tools[0].id}`} tools={part.tools} />
                ) : (
                  <ItemView key={part.key} item={part.item} mode={mode} family={family} frozen={frozen} onDecide={onDecide} />
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
              <Queued key={at} text={text} />
            ))}
          </div>
        </section>
      )}
    </div>
    </Live.Provider>
  );
}

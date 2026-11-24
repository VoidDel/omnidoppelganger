import { type CSSProperties, createContext, useContext, useEffect, useRef, useState } from "react";
import { type Item, type ToolItem, editsFiles, fileSize, mcpTool, shortCount, todosOf } from "../lib/fold";
import type { Decision, DiffHunk, Image, Mode, SessionItem } from "../lib/types";
import { Light } from "./Light";
import { Diff, ModelText } from "./Markdown";
import { useSecondsLeft } from "./Questions";
import { Viewer, imageSource } from "./Viewer";

/**
 * Whether what is put on the page now is put there as it happens, not read back from the log: an act that
 * arrives while the page is watched is stamped onto it; a page of old acts is simply shown.
 */
export const Live = createContext<{ current: boolean }>({ current: false });

/** What the cards may do with the files of the session: open one (or a historical snapshot), show it in its folder, and fetch a picture. */
export interface Files {
  open: (path: string, reveal: boolean, snapshot?: string, version?: number) => Promise<string>;
  picture: (path: string, snapshot?: string) => Promise<Image>;
  /** 1-based version number for `(path, snapshot)` when the file has more than one version in this session. */
  versionOf?: (path: string, snapshot?: string) => number | undefined;
}

export const FilesContext = createContext<Files | null>(null);

/** Kinds of file shown as a picture under the act that wrote them. */
const PICTURE_KINDS = /\.(png|jpe?g|gif|webp|svg|bmp|ico)$/i;

/**
 * Under an act that wrote or delivered a file: the way to open it with the system's program for it and to its folder,
 * and for a picture, the picture itself (from the step's snapshot when available), which opens at full size when pressed.
 */
export function Written({ path, name, snapshot, version }: { path: string; name: string; snapshot?: string; version?: number }) {
  const files = useContext(FilesContext);
  const resolvedVersion = version ?? files?.versionOf?.(path, snapshot);
  const picture = PICTURE_KINDS.test(path);
  const [image, setImage] = useState<Image | null>(null);
  const [note, setNote] = useState<{ text: string; failed: boolean } | null>(null);
  const [viewing, setViewing] = useState(false);
  useEffect(() => {
    if (!files || !picture) return;
    let current = true;
    files.picture(path, snapshot).then(
      (got) => current && setImage(got),
      (error) => current && setNote({ text: String(error), failed: true }),
    );
    return () => {
      current = false;
    };
  }, [files, path, picture, snapshot]);
  if (!files) return null;
  const open = (reveal: boolean) => {
    setNote(null);
    files.open(path, reveal, reveal ? undefined : snapshot, reveal ? undefined : resolvedVersion).then(
      (done) => done === "revealed" && !reveal && setNote({ text: "这是可执行文件，不从这里直接运行，已在文件夹中显示。", failed: false }),
      (error) => setNote({ text: String(error), failed: true }),
    );
  };
  return (
    <div className="written">
      {image && (
        <button
          type="button"
          className="shot written-shot"
          title={resolvedVersion ? `点开看大图（v${resolvedVersion} 快照）` : snapshot ? "点开看大图（这一步当时的版本）" : "点开看大图（显示的是文件眼下的样子）"}
          onClick={() => setViewing(true)}
        >
          <img src={imageSource(image)} alt={`分身写的 ${name}`} />
        </button>
      )}
      <div className="written-acts">
        <button
          type="button"
          className="button text small"
          title={`${path}\n${resolvedVersion ? `用系统默认的程序打开 v${resolvedVersion} 快照` : snapshot ? "用系统默认的程序打开这一步的版本" : "用系统默认的程序打开"}`}
          onClick={() => open(false)}
        >
          {resolvedVersion ? `打开 v${resolvedVersion}` : "打开"}
        </button>
        <button type="button" className="button text small" title="在文件夹中显示" onClick={() => open(true)}>
          在文件夹中显示
        </button>
        {note && (
          <span className={`file-note ${note.failed ? "failed" : ""}`} role="status">
            {note.text}
          </span>
        )}
      </div>
      {viewing && image && <Viewer images={[image]} at={0} whose="分身写的" onClose={() => setViewing(false)} />}
    </div>
  );
}

/** The things that came onto the page together last, counted, so that each can follow the one before it. */
const arrivals = { at: 0, count: 0 };

/**
 * Whether the thing that calls this came onto the page as it happened: decided once, when it comes. What it
 * gives is the class that says so and the thing's place among those that came with it, which sets how long
 * it waits its turn: several acts sent in one breath are stamped one after another, not all at once.
 */
export function useFresh(): { className: string; style: CSSProperties | undefined } {
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
export type Weight = "live" | "failed" | "wrote" | "act" | "quiet";

/** Acts that only look: they change nothing and reach nowhere outside the machine. */
const LOOKS = new Set(["read_file", "search", "find_files", "code_nav", "list_mcp_resources", "read_mcp_resource", "load_skill", "update_todos", "update_steps", "update_focus", "command_output"]);

/** Shorten verbose external CLI scratch/brain directory prefixes for readable one-line headers. */
function compactCliPaths(value: string): string {
  return value
    .replace(/(?:\/Users\/[^/\s]+|~|[A-Za-z]:\\Users\\[^\\\s]+)[/\\]\.gemini[/\\]antigravity[^/\\\s]*[/\\]brain[/\\][0-9a-f-]+[/\\]scratch[/\\]/gi, "[暂存]/")
    .replace(/(?:\/Users\/[^/\s]+|~|[A-Za-z]:\\Users\\[^\\\s]+)[/\\]\.gemini[/\\]antigravity[^/\\\s]*[/\\]brain[/\\][0-9a-f-]+[/\\]/gi, "[脑海]/");
}

/** Whether nothing came of an act because it was not let to, or was taken back. */
export const voided = (tool: ToolItem) => tool.approval?.decision === "deny" || !!tool.result?.meta?.refused || !!tool.undone;

export function weigh(tool: ToolItem): Weight {
  const { result } = tool;
  // Turned down by 本尊, held back by 先谋后动, or taken back since: nothing came of it, and nothing went wrong.
  if (voided(tool)) return "quiet";
  if (!result) return "live";
  if (result.isError) return "failed";
  if (editsFiles(tool.name)) return "wrote";
  return LOOKS.has(tool.name) ? "quiet" : "act";
}

/** The single character stamped on an act: read, search, edit, create, run. */
export function sealOf(tool: ToolItem): string {
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
    case "delete_file":
      return "删";
    case "move_file":
      return "移";
    case "run_command":
      return "行";
    case "command_output":
      return "观";
    case "command_input":
      return "输";
    case "list_mcp_resources":
    case "read_mcp_resource":
      return "器";
    case "present_plan":
      return "呈";
    case "stop_command":
      return "止";
    case "update_todos":
    case "update_steps":
      return "谋";
    case "delegate":
    case "invoke_subagent":
    case "Task":
      return "遣";
    case "manage_task":
      return "观";
    case "ask_question":
      return "问";
    case "load_skill":
      return "技";
    case "install_skill":
      return "习";
    case "add_mcp_server":
      return "纳";
    case "document_runtime":
      return "备";
    case "deliver_files":
      return "交";
    case "generate_image":
      return "画";
    case "fetch_url":
      return "网";
    case "web_search":
      return "询";
    case "ask_user":
      return "问";
    case "code_nav":
      return "察";
    case "language_server":
      return "备";
    default:
      // A tool of an MCP server: an instrument borrowed from outside.
      return mcpTool(tool.name) ? "器" : "术";
  }
}

/** Whether a call is a plan the 分身 presented, or is presenting: shown as the plan, not as a line. */
export const presented = (item: Item) => item.kind === "tool" && item.name === "present_plan" && !item.result?.isError && typeof item.input.plan === "string";

/** The plan the 分身 presented in 先谋后动: its title, and the plan as Markdown, to be read and copied. */
export function PlanCard({ tool }: { tool: ToolItem }) {
  const title = typeof tool.input.title === "string" && tool.input.title.trim() ? tool.input.title.trim() : null;
  return (
    <div className="plan-card">
      <div className="plan-head">
        <span className="plan-seal" aria-hidden="true">
          呈
        </span>
        <span>分身呈上的计划</span>
        {title && <strong className="plan-title">{title}</strong>}
      </div>
      <ModelText text={tool.input.plan as string} />
    </div>
  );
}

/** What each operation of `code_nav` is called in the conversation. */
const CODE_NAV: Record<string, string> = {
  definition: "查定义",
  references: "查引用",
  implementations: "查实现",
  hover: "看类型",
  symbols: "看大纲",
  find_symbol: "找符号",
  diagnostics: "查错",
};

/** What a finished or running tool call says in its one-line header. */
function describe(tool: ToolItem): { verb: string; target: string; outcome: string } {
  const text = (name: string) => (typeof tool.input[name] === "string" ? (tool.input[name] as string) : "");
  const { result } = tool;
  const meta = result?.meta;
  const declined = tool.approval?.decision === "deny";
  const refusedLabel = meta?.refusal === "read_only" ? "只读子分身，未执行" : meta?.refusal === "subagent" ? "子分身不可用" : "先谋后动，没有改";
  const waiting = declined ? (tool.approval?.timedOut ? "无人定夺，自动驳回" : "本尊驳回了") : meta?.refused ? refusedLabel : tool.undone ? "本尊已撤回" : result ? "" : "运功中";
  switch (tool.name) {
    case "read_file": {
      // A picture is looked at, not read: its kind and size instead of a count of lines.
      const kind = meta?.picture?.replace("image/", "").toUpperCase();
      const looked = kind && [kind, meta?.width && meta?.height ? `${meta.width}×${meta.height}` : ""].filter(Boolean).join("，");
      const lines = meta?.lines ?? 0;
      const countText = lines > 0 || meta?.shown !== undefined ? `${lines} 行` : "已读";
      return { verb: kind ? "看图" : "读取", target: compactCliPaths(text("path")), outcome: waiting || (result!.isError ? "没读到" : looked || countText) };
    }
    case "search":
      return {
        verb: "搜索",
        target: text("pattern"),
        outcome:
          waiting ||
          (result!.isError
            ? "搜索失败"
            : meta?.files_only
              ? `${typeof meta.files === "number" ? meta.files : 0} 个文件${meta.truncated ? "以上" : ""}`
              : `${meta?.matches ?? 0} 处${meta?.truncated ? "以上" : ""}`),
      };
    case "find_files":
      return {
        verb: text("glob") ? "查找" : "列出",
        target: compactCliPaths(text("glob") || text("path")) || "洞府",
        outcome: waiting || (result!.isError ? "没找成" : `${meta?.entries ?? 0} 项${meta?.truncated ? "以上" : ""}`),
      };
    case "edit_file":
    case "write_file": {
      const added = meta?.added ?? 0;
      const removed = meta?.removed ?? 0;
      const hasCounts = added > 0 || removed > 0 || !!meta?.after;
      const doneLabel = hasCounts ? `加 ${added} 行，删 ${removed} 行` : tool.name === "write_file" || meta?.created ? "已写入" : "已修改";
      return {
        verb: meta?.created ? "新建" : tool.name === "write_file" ? "重写" : "修改",
        target: compactCliPaths(text("path")),
        outcome: waiting || (result!.isError ? "没改成" : doneLabel),
      };
    }
    case "delete_file":
      return {
        verb: "删除",
        target: compactCliPaths(text("path")),
        outcome: waiting || (result!.isError ? "没删成" : meta?.removed !== undefined ? `删去 ${meta.removed} 行` : fileSize(meta?.bytes ?? 0)),
      };
    case "move_file":
      return { verb: "挪动", target: `${compactCliPaths(text("path"))} → ${compactCliPaths(text("to"))}`, outcome: waiting || (result!.isError ? "没挪成" : "已挪") };
    case "run_command": {
      const hasCode = meta?.exit_code !== undefined && meta?.exit_code !== null;
      const ended =
        meta?.stopped === "timed out"
          ? "超时停止"
          : meta?.stopped
            ? "已停止"
            : hasCode
              ? meta!.exit_code === 0
                ? "成功"
                : `退出码 ${meta!.exit_code}`
              : result?.isError
                ? "执行失败"
                : "成功";
      // Said only where it holds: the command could write nowhere but the workspace and the temporary directories.
      const where = meta?.sandboxed ? "结界内，" : "";
      const cwd = meta?.cwd ?? text("cwd").trim();
      const cmdText = compactCliPaths(text("command"));
      const target = cwd && cwd !== "." ? `[${compactCliPaths(cwd)}] ${cmdText}` : cmdText;
      if (meta?.background) return { verb: "后台运行", target, outcome: waiting || `${where}已起，编号 ${meta.job}` };
      return { verb: "运行", target, outcome: waiting || (meta || result ? where + ended : "没有运行") };
    }
    case "list_mcp_resources":
      return { verb: "看法器资源", target: text("server"), outcome: waiting || (result!.isError ? "没看到" : `${meta?.results ?? 0} 项`) };
    case "read_mcp_resource":
      return { verb: "读法器资源", target: `${text("server")} · ${text("uri")}`, outcome: waiting || (result!.isError ? "没读到" : "已读") };
    case "present_plan":
      return { verb: "呈上计划", target: text("title"), outcome: waiting || (result!.isError ? "没呈上" : "已呈上") };
    case "command_input": {
      // What was typed, into which job; then how the job stands after it.
      const sent = text("text") ? `← ${text("text")}` : "";
      const closed = tool.input.close === true ? "（结束输入）" : "";
      const state = meta?.running ? "还在跑" : meta?.stopped ? "已停止" : meta?.exit_code === 0 ? "已结束，成功" : `已结束，退出码 ${meta?.exit_code ?? "未知"}`;
      return { verb: "输入后台命令", target: `${text("job")}  ${sent}${closed}`, outcome: waiting || (result!.isError ? "没输进去" : state) };
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
      // Who searched, when it was not Zhipu: the provider the 分身 works with.
      return { verb: "问天机阁", target: text("query"), outcome: waiting || (result!.isError ? "没问到" : `${meta?.engine ? `${meta.engine} 代搜，` : ""}${meta?.results ?? 0} 条`) };
    case "fetch_url":
      return { verb: "抓取网页", target: text("url"), outcome: waiting || (result!.isError ? "没取到" : `${shortCount(meta?.characters ?? 0)} 字`) };
    case "load_skill":
      return { verb: "研读秘籍", target: text("name"), outcome: waiting || (result!.isError ? "没有这部秘籍" : "已读") };
    case "install_skill":
      return {
        verb: "习得秘籍",
        target: text("path") ? `${text("source")}  ${text("path")}` : text("source"),
        outcome: waiting || (result!.isError ? "没装成" : `「${meta?.skill ?? ""}」装给${meta?.scope === "user" ? "所有分身" : "这处洞府"}`),
      };
    case "document_runtime":
      return {
        verb: tool.input.install === true ? "备置文房" : "查看文房",
        target: "做文档、画图用的 Python",
        outcome: waiting || (result!.isError ? "没备成" : meta?.state === "ready" ? (meta.libreoffice ? "已就绪，有 LibreOffice" : "已就绪") : "还没安装"),
      };
    case "deliver_files": {
      const handed = Array.isArray(meta?.files) ? meta.files : [];
      const named = (Array.isArray(tool.input.paths) ? (tool.input.paths as unknown[]) : []).filter((path): path is string => typeof path === "string");
      return {
        verb: "交付文件",
        target: (handed.length ? handed : named).map((path) => path.replace(/^.*[\\/]/, "")).join("、"),
        outcome: waiting || (result!.isError ? "没交成" : `${handed.length} 个`),
      };
    }
    case "generate_image":
      return {
        verb: "作画",
        target: meta?.path ?? text("path"),
        outcome: waiting || (result!.isError ? "没画成" : [meta?.painter, meta?.size].filter(Boolean).join("，")),
      };
    case "add_mcp_server":
      return {
        verb: "收纳法器",
        target: text("name"),
        outcome:
          waiting ||
          (meta?.state === "ready" ? `已连上，${meta.tools?.length ?? 0} 个工具` : meta?.state === "failed" ? "加进清单了，没连上" : meta?.state === "connecting" ? "加进清单了，正在连接" : "没加成"),
      };
    case "ask_user":
      return { verb: "请教本尊", target: "", outcome: waiting || (result!.isError ? "没问成" : "已答") };
    case "language_server": {
      const state = meta?.state === "missing" ? "没装" : meta?.state === "downloaded" ? (tool.input.install === true ? "已下载" : "用下载的") : "已装";
      return { verb: tool.input.install === true ? "下载语言服务" : "看语言服务", target: meta?.server ?? text("language"), outcome: waiting || (result!.isError ? "没成" : state) };
    }
    case "code_nav": {
      const operation = text("operation");
      const verb = CODE_NAV[operation] ?? "察看代码";
      const line = typeof tool.input.line === "number" ? `:${tool.input.line}` : "";
      const what = text("symbol") || text("query");
      const counted = operation === "diagnostics" ? `${meta?.errors ?? 0} 错，共 ${meta?.results ?? 0} 条` : operation === "hover" ? "已得" : `${meta?.results ?? 0} 处`;
      return { verb, target: [`${text("path")}${line}`, what].filter(Boolean).join("  "), outcome: waiting || (result!.isError ? "没查成" : counted) };
    }
    case "update_todos":
    case "update_steps": {
      const list = todosOf(tool) ?? [];
      const current = list.find((todo) => todo.status === "in_progress")?.text ?? "";
      return { verb: "记计划", target: current, outcome: waiting || (result!.isError ? "没记成" : `${list.filter((todo) => todo.status === "done").length} / ${list.length} 已办`) };
    }
    default: {
      const borrowed = mcpTool(tool.name);
      if (borrowed) return { verb: `法器 ${borrowed.server}`, target: borrowed.tool, outcome: waiting || (result!.isError ? "失败" : "完成") };
      const cliTarget = text("focus") || text("Role") || text("description") || text("TaskId") || text("path") || text("command");
      const cliVerb =
        tool.name === "update_focus"
          ? "心神聚焦"
          : tool.name === "invoke_subagent" || tool.name === "Task"
            ? "派出子分身"
            : tool.name === "manage_task"
              ? "察看后台任务"
              : tool.name === "ask_question"
                ? "请教本尊"
                : tool.name;
      return { verb: cliVerb, target: cliTarget, outcome: waiting || (result!.isError ? "失败" : "完成") };
    }
  }
}

/** Where the 子分身 of the session on screen are looked up, and how one is opened. */
export interface Family {
  sessions: SessionItem[];
  onOpen: (id: string) => void;
}

/**
 * A `delegate` call: the 子分身 it set off, each with how it stands and a way
 * in. One that waits for 本尊 is marked in cinnabar, since its request is on
 * its own page, not this one.
 */
export function DelegateCard({ tool, family }: { tool: ToolItem; family: Family }) {
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
    return { name: started?.name ?? (text(task.name) || `子分身 ${at + 1}`), task: text(task.task), readOnly: task.read_only === true, session: started?.session, state };
  });
  const STATES: Record<string, string> = { await: "候旨", busy: "运功中", done: "已回报", failed: "没办成", pending: "待出发" };
  const count = (state: string) => agents.filter((agent) => agent.state === state).length;
  const outcome = tool.approval?.decision === "deny" ? "本尊驳回了" : !tool.result ? (count("await") ? `${count("await")} 个候旨` : "运功中") : `${count("done")} / ${agents.length} 已回报`;
  // A call the model got wrong sets nobody off; what was wrong is all there is to show.
  const failure = tool.result?.isError && agents.length === 0 ? tool.result.content : undefined;
  return (
    <div className={`did delegate ${weigh(tool)} ${fresh.className}`} style={fresh.style}>
      <div className="did-head">
        {/* While it is under way its edge is the light going round, as every act's is: the seal has no border of its own then. */}
        <span className="seal" aria-hidden="true">
          {weigh(tool) === "live" && <Light />}遣
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
                {agent.readOnly && <span className="chip kin">只读</span>}
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
export function ToolCard({ tool }: { tool: ToolItem }) {
  const fresh = useFresh();
  // Its place among those that came with it sets how long it waits to be shown. Having been shown under way,
  // it has waited: its seal comes down the moment it is done.
  const [begun] = useState(() => weigh(tool) === "live");
  const style = begun && weigh(tool) !== "live" && fresh.style ? ({ "--nth": 0 } as CSSProperties) : fresh.style;
  const { verb, target, outcome } = describe(tool);
  const { result } = tool;
  const hunks = result && !result.isError ? (result.meta?.hunks?.length ? result.meta.hunks : result.meta?.hunk ? [result.meta.hunk] : []) : [];
  const output = tool.name === "run_command" || tool.name === "command_output" || tool.name === "command_input" || tool.name === "stop_command" ? result?.meta?.output : undefined;
  // A failed call shows what went wrong; a finished command keeps its output one click away.
  // What an MCP tool returned is its whole result, so it is kept one click away like a command's output.
  const returned = (result?.meta?.mcp || ["web_search", "code_nav", "list_mcp_resources", "read_mcp_resource"].includes(tool.name)) && !result?.isError ? result?.content : undefined;
  const failure = result?.isError && !result.meta?.command && tool.approval?.decision !== "deny" && !result.meta?.refused ? result.content : undefined;
  const body =
    hunks.length > 0 ? (
      <>
        {hunks.map((hunk, at) => (
          <Diff key={at} hunk={hunk} />
        ))}
      </>
    ) : output ? (
      <pre>{output}</pre>
    ) : returned ? (
      <pre>{returned}</pre>
    ) : failure ? (
      <pre>{failure}</pre>
    ) : null;
  // The file an edit wrote, by where it is: what can be opened from under the act.
  // A picture it looked at is shown the same way, from where it lies.
  const written =
    (editsFiles(tool.name) || tool.name === "generate_image" || (tool.name === "read_file" && !!result?.meta?.picture)) && result && !result.isError && result.meta?.file
      ? result.meta.file
      : null;
  // What was handed over, made some other way: each opened from under the act as a written file is.
  const handed = tool.name === "deliver_files" && result && !result.isError && Array.isArray(result.meta?.files) ? result.meta.files : [];
  // A change and a failure are open until closed, even when they come in after the line they belong to; but a
  // picture's text is not what is looked at, the picture is.
  const [chosen, setOpen] = useState<boolean | null>(null);
  const open = chosen ?? ((hunks.length > 0 && !(written && PICTURE_KINDS.test(written))) || !!failure || (!!output && !!result?.isError));
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
      {written && !voided(tool) && <Written path={written} name={written.replace(/^.*[\\/]/, "")} snapshot={result?.meta?.snapshot} />}
      {handed.map((path) => (
        <Written key={path} path={path} name={path.replace(/^.*[\\/]/, "")} snapshot={result?.meta?.snapshots?.[path]} />
      ))}
    </div>
  );
}

const ASK: Record<string, { title: string; note: string; always: string }> = {
  command: { title: "分身要运行一条命令，请本尊定夺", note: "命令会在洞府（这个文件夹）内执行。", always: "之后同类命令都准" },
  outside_command: {
    title: "分身要在洞府之外或 .git 目录中运行命令，请本尊定夺",
    note: "这条命令的工作目录不在当前洞府内（或在 .git 目录中），所以每次都要请示。",
    always: "",
  },
  unsandboxed: {
    title: "分身要出结界运行一条命令，请本尊定夺",
    note: "设置里布着结界（沙箱），命令本来只能往洞府和临时目录里写。分身说这一条得往别处写，所以要出结界跑：准了，它就能改这台机器上本尊能改的任何文件。出结界每次都要请示。",
    always: "",
  },
  search: {
    title: "分身要向天机阁打探（联网搜索），请本尊定夺",
    note: "要打探的话会发给智谱的搜索服务，用的是 GLM（智谱）的密钥。",
    always: "之后打探都准",
  },
  web: {
    title: "分身要上网取一个网页，请本尊定夺",
    note: "这个请求会发到洞府之外，网址里带的内容对方都看得到。准了之后，同一个网站的其他网页不再请示。",
    always: "之后这个网站都准",
  },
  mcp: {
    title: "分身要动用一件身外法器（MCP 工具），请本尊定夺",
    note: "这件法器来自你在「法器」里接入的 MCP 服务，它具体做什么由那个服务决定，所以无论哪种禁制都要先请示。",
    always: "之后这件法器都准",
  },
  image: {
    title: "分身要请画师作一幅画（生成图片），请本尊定夺",
    note: "下面是要画的内容和存放的位置。会请设置「神通 → 丹青」里选的画师作画，画好存进洞府，在对话里直接能看。",
    always: "之后作画都准",
  },
  install_skill: {
    title: "分身要装一部秘籍，请本尊定夺",
    note: "秘籍是一份做事说明，装上以后分身遇到对得上的事会照着做；里面也可能带着脚本。下面是从哪里装、装到哪里：.odg/skills 只对这处洞府生效，~/.odg/skills 对所有分身生效。从网上的仓库装时会先下载下来。无论哪种禁制，每次都要请示。",
    always: "",
  },
  install_runtime: {
    title: "分身要下载文档运行环境，请本尊定夺",
    note: "做 Word、Excel、PPT、PDF 和画图要用到一套 Python 和几个库。准了之后，会从 Python 官方构建的发布处和 PyPI 下载下面这些，逐个按事先定好的校验码核对，装到 ~/.odg/runtime，不动你电脑上已有的 Python。只下载这一次，以后所有分身都能用；在设置的「神通」里可以查看或移除。无论哪种禁制，每次都要请示。",
    always: "",
  },
  add_mcp_server: {
    title: "分身要添一件身外法器（MCP 服务），请本尊定夺",
    note: "准了之后它就写进「法器」的清单并马上连接：本机的会在这台电脑上启动下面的程序，远程的会连到下面的地址，此后每个分身都能用它的工具（每次动用仍要请示）。下面是要写进清单的全部内容，密钥也在其中，看清了再准。无论哪种禁制，每次都要请示。",
    always: "",
  },
  edit: { title: "分身要改一个文件，请本尊定夺", note: "眼下的禁制是步步请示，每次改文件都先请示。整文件写入时，下面是它要写进去的全部内容。", always: "之后改文件都准" },
  outside_write: { title: "分身要改洞府之外的文件，请本尊定夺", note: "这个路径不在当前文件夹里。", always: "之后这类修改都准" },
  outside_read: { title: "分身要读洞府之外的内容，请本尊定夺", note: "这个路径不在当前文件夹里。", always: "之后这类读取都准" },
  install_lsp: {
    title: "分身要下载语言服务，请本尊定夺",
    note: "代码导航要用这门语言的语言服务，这台电脑上没装。准了之后，从它的官方发布处下载下面这一份，按事先定好的校验码核对后放进 ~/.odg/lsp，不动系统里的东西。只下载这一次，以后所有分身都能用；在设置的「神通」里可以查看或移除。无论哪种禁制，每次都要请示。",
    always: "",
  },
  mcp_resource: {
    title: "分身要读身外法器里的资源，请本尊定夺",
    note: "资源是法器（MCP 服务）拿出来给人读的东西，比如文档、记录、表结构。读的时候要把资源的地址发给那个服务，它可能在别的机器上。",
    always: "之后读这个法器的资源都准",
  },
  input: {
    title: "分身要给正在运行的命令输入，请本尊定夺",
    note: "这条命令在后台运行，分身要往它的输入里写下面这段文字，就像在终端里打字后按回车。打进去的东西会让它照着做：给 python、shell 这类程序输入，等于让它执行代码。",
    always: "之后给这条命令的输入都准",
  },
  lsp: {
    title: "分身要启动语言服务来察看代码，请本尊定夺",
    note: "TypeScript 的语言服务会加载项目 node_modules 里的 TypeScript 来分析代码，这算是运行项目里的代码。准了之后，查定义、查引用这类不改文件的操作就能用。别的语言的服务都设成不运行项目代码，不用请示。",
    always: "之后都准",
  },
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

export interface SignSheetProps {
  tool: ToolItem;
  mode: Mode;
  /** The page shows the past: the request is shown as it stood, with nothing to press. */
  frozen: boolean;
  onDecide: (toolUseId: string, decision: Decision) => void;
}

export function SignSheet({ tool, mode, frozen, onDecide }: SignSheetProps) {
  const approval = tool.approval!;
  const seconds = useSecondsLeft(approval.expiresAt, frozen);
  /** Whether the explanation behind the mark after the title is open. */
  const [explained, setExplained] = useState(false);
  const sheetRef = useRef<HTMLDivElement>(null);
  const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  useEffect(() => {
    if (frozen) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return;
      const el = sheetRef.current;
      if (!el || el.offsetParent === null) return;
      if (document.querySelector("dialog[open], .slash-menu, [role='menu'], .confirm-bubble, .combo-list")) return;
      // Only the first visible SignSheet handles global approval shortcuts.
      const firstVisible = [...document.querySelectorAll<HTMLElement>(".sign-sheet:not(.ask-sheet):not(.frozen)")].find((node) => node.offsetParent !== null);
      if (firstVisible && firstVisible !== el) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest(".xterm, .aside-pane")) return;
      const inField = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target?.isContentEditable;
      const hasTyped =
        (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) && target.value.trim().length > 0;
      const mod = event.metaKey || event.ctrlKey;
      if (mod && !event.altKey && event.key === "Enter" && !hasTyped) {
        event.preventDefault();
        if (event.shiftKey && approval.grant) onDecide(tool.id, "allow_session");
        else if (!event.shiftKey) onDecide(tool.id, "allow_once");
        return;
      }
      if (mod && !event.altKey && !event.shiftKey && event.key === "Backspace" && !inField) {
        event.preventDefault();
        onDecide(tool.id, "deny");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [frozen, approval.grant, onDecide, tool.id]);
  const asked = ASK[approval.reason] ?? ASK.command;
  // Deleting and moving are asked about as edits are, under their own word.
  const doing = tool.name === "delete_file" ? "删除" : tool.name === "move_file" ? "挪动" : null;
  const ask = doing ? { ...asked, title: asked.title.replace("要改", `要${doing}`) } : asked;
  const oneByOne = approval.reason === "command" && !approval.grant;
  const change = approval.reason === "edit" ? proposed(tool) : null;
  const why =
    ask.note +
    (approval.reason === "command" && mode === "auto" ? "放手施为只自行放行只读、构建、测试类且不碰洞府之外的命令，这条不在其列。" : "") +
    (oneByOne ? "这条命令带有管道、分号、引号、变量之类的写法，或者会执行任意代码，只能逐条批准。" : "");
  return (
    <div ref={sheetRef} className={`sign-sheet ${frozen ? "frozen" : ""}`}>
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
        {(approval.grant === "search" || ["command:", "mcp:", "mcp-resource:", "web:", "lsp:"].some((kind) => approval.grant?.startsWith(kind))) && (
          <button type="button" className="button outline red" title="发一道通行符：重启应用之后也算数，可以在设置的「禁制」里收回" onClick={() => onDecide(tool.id, "allow_always")}>
            此府永准
          </button>
        )}
        <button type="button" className="button text" onClick={() => onDecide(tool.id, "deny")}>
          驳回
        </button>
        <span className="sign-keys" aria-hidden="true">
          <kbd>{isMac ? "⌘↵" : "Ctrl+Enter"}</kbd> 准
          {approval.grant ? (
            <>
              {" · "}
              <kbd>{isMac ? "⌘⇧↵" : "Ctrl+Shift+Enter"}</kbd> 常准
            </>
          ) : null}
          {" · "}
          <kbd>{isMac ? "⌘⌫" : "Ctrl+Backspace"}</kbd> 驳
        </span>
      </div>
    </div>
  );
}

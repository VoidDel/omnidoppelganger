import { useEffect, useRef, useState } from "react";
import Markdown from "react-markdown";
import type { Item, Row, StreamBlock, ToolItem } from "../lib/fold";
import type { Decision, DiffHunk, Status } from "../lib/types";

const time = (ts: string) => new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

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
    case "edit_file":
      return tool.result?.meta?.created ? "建" : "改";
    case "run_command":
      return "行";
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
  const waiting = declined ? "本尊驳回了" : result ? "" : "运功中";
  switch (tool.name) {
    case "read_file":
      return { verb: "读取", target: text("path"), outcome: waiting || (result!.isError ? "没读到" : `${meta?.lines ?? 0} 行`) };
    case "search":
      return {
        verb: "搜索",
        target: text("pattern"),
        outcome: waiting || (result!.isError ? "搜索失败" : `${meta?.matches ?? 0} 处${meta?.truncated ? "以上" : ""}`),
      };
    case "edit_file":
      return {
        verb: meta?.created ? "新建" : "修改",
        target: text("path"),
        outcome: waiting || (result!.isError ? "没改成" : `加 ${meta?.added ?? 0} 行，删 ${meta?.removed ?? 0} 行`),
      };
    case "run_command": {
      const ended = meta?.stopped === "timed out" ? "超时停止" : meta?.stopped ? "已停止" : meta?.exit_code === 0 ? "成功" : `退出码 ${meta?.exit_code ?? "未知"}`;
      return { verb: "运行", target: text("command"), outcome: waiting || (meta ? ended : "没有运行") };
    }
    default:
      return { verb: tool.name, target: "", outcome: waiting || (result!.isError ? "失败" : "完成") };
  }
}

function ToolCard({ tool }: { tool: ToolItem }) {
  const { verb, target, outcome } = describe(tool);
  const { result } = tool;
  const hunk = result && !result.isError ? result.meta?.hunk : undefined;
  const output = tool.name === "run_command" ? result?.meta?.output : undefined;
  // A failed call shows what went wrong; a finished command keeps its output one click away.
  const failure = result?.isError && tool.name !== "run_command" && tool.approval?.decision !== "deny" ? result.content : undefined;
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

function SignSheet({ tool, onDecide }: { tool: ToolItem; onDecide: (toolUseId: string, decision: Decision) => void }) {
  const approval = tool.approval!;
  const ask = ASK[approval.reason] ?? ASK.command;
  const oneByOne = approval.reason === "command" && !approval.grant;
  return (
    <div className="sign-sheet">
      <h2>{ask.title}</h2>
      <div className="sign-detail">{approval.detail}</div>
      <p>
        {ask.note}
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
      return error === "no API key is configured" ? "还没有 API 密钥，分身出不了关。到设置里填上 Anthropic 密钥，再传一次令。" : `这一轮走火了，已经停下：${error ?? "原因不明"}`;
  }
}

function ItemView({ item, onDecide }: { item: Item; onDecide: (toolUseId: string, decision: Decision) => void }) {
  switch (item.kind) {
    case "text":
      return (
        <div className="prose">
          <Markdown>{item.text}</Markdown>
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
      return item.approval && !item.approval.decision && !item.result ? <SignSheet tool={item} onDecide={onDecide} /> : <ToolCard tool={item} />;
    case "fallback":
      return (
        <div className="notice">
          {item.from} 没有接这个请求，改由 {item.to} 继续。
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
            <Markdown>{block.text}</Markdown>
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
  onDecide: (toolUseId: string, decision: Decision) => void;
}

export function Transcript({ rows, stream, queued, status, onDecide }: Props) {
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
            </div>
            <div className="turn-did">
              {row.items.map((item, at) => (
                <ItemView key={item.kind === "tool" ? item.id : at} item={item} onDecide={onDecide} />
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

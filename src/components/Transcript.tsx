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

/** What a finished or running tool call says in its one-line header. */
function describe(tool: ToolItem): { verb: string; target: string; outcome: string } {
  const text = (name: string) => (typeof tool.input[name] === "string" ? (tool.input[name] as string) : "");
  const { result } = tool;
  const meta = result?.meta;
  const declined = tool.approval?.decision === "deny";
  const waiting = declined ? "你拒绝了" : result ? "" : "进行中";
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
  command: { title: "要运行一条命令，等你签字", note: "命令会在工作区里执行。", always: "之后的命令都批准" },
  outside_write: { title: "要改工作区外面的文件，等你签字", note: "这个路径不在当前工作区里。", always: "之后这类修改都批准" },
  outside_read: { title: "要读工作区外面的内容，等你签字", note: "这个路径不在当前工作区里。", always: "之后这类读取都批准" },
};

function SignSheet({ tool, onDecide }: { tool: ToolItem; onDecide: (toolUseId: string, decision: Decision) => void }) {
  const ask = ASK[tool.approval!.reason] ?? ASK.command;
  return (
    <div className="sign-sheet">
      <h2>{ask.title}</h2>
      <div className="sign-detail">{tool.approval!.detail}</div>
      <p>{ask.note}</p>
      <div className="actions">
        <button type="button" className="button solid" onClick={() => onDecide(tool.id, "allow_once")}>
          批准这一次
        </button>
        <button type="button" className="button outline" onClick={() => onDecide(tool.id, "allow_session")}>
          {ask.always}
        </button>
        <button type="button" className="button text" onClick={() => onDecide(tool.id, "deny")}>
          拒绝
        </button>
      </div>
    </div>
  );
}

function notice(reason: string, error: string | null): string {
  switch (reason) {
    case "cancelled":
      return "你停止了分身。";
    case "refusal":
      return "模型拒绝了这个请求。";
    case "max_tokens":
      return "回复太长被截断了，被截断的操作没有执行。可以让分身分几步来。";
    case "step_limit":
      return "这一轮步骤太多，分身停下来了。";
    case "interrupted":
      return "上次关闭应用时，这一轮还没做完。";
    default:
      return error === "no API key is configured" ? "还没有 API 密钥。到设置里填上 Anthropic 密钥，再发一次。" : `这一轮出错停下了：${error ?? "原因不明"}`;
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
          <summary>分身想了一下</summary>
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
            正在想：{block.text.slice(-160)}
          </div>
        ) : (
          <div key={block.index} className="did">
            <div className="did-head">
              <span className="verb">准备</span>
              <span className="target">{block.name}</span>
              <span className="outcome">正在写参数</span>
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
                你，{time(row.prompt.ts)}
                {row.prompt.steering ? "，中途插话" : ""}
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
            <span className="said-meta">排队中，分身做完这一步就会看到</span>
          </div>
          <div className="turn-did" />
        </section>
      ))}
    </div>
  );
}

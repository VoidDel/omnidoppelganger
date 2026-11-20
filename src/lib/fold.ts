// Folds the session log into what the window draws. The log is the only input,
// so reopening a session shows exactly what was on screen before.

import type { ContentBlock, Decision, DiffHunk, DocumentKind, Envelope, Image, StreamDelta, Todo, Tokens, ToolMeta } from "./types";

export interface ToolItem {
  kind: "tool";
  id: string;
  name: string;
  input: Record<string, unknown>;
  result?: { content: string; isError: boolean; meta: ToolMeta | null };
  /** `grant` is what allowing for the session covers; without one, the request is approved each time. */
  approval?: { reason: string; detail: string; grant?: string; decision?: Decision; expiresAt?: string; timedOut?: boolean };
  /** What the 护法 made of the call: let it run, or left it to 本尊; `failed` when it could not be asked. */
  review?: { allowed: boolean; reason: string; model: string; failed: boolean };
  /** The turn the call belongs to. */
  turn: number;
  /** The user undid this edit. */
  undone?: boolean;
  /** The 子分身 a `delegate` call has set off so far, each a session of its own. */
  agents?: { session: string; name: string }[];
}

export type Item =
  | { kind: "text"; text: string }
  | { kind: "thinking"; text: string }
  | ToolItem
  | { kind: "fallback"; from: string; to: string }
  | { kind: "compacted"; summary: string; tokensBefore: number }
  | { kind: "restored"; turn: number; restored: string[]; skipped: string[] }
  /** The project's instructions for agents were read; empty `content` means the file is gone. */
  | { kind: "instructions"; path: string; content: string }
  | { kind: "notice"; reason: string; error: string | null };

/** One row of the transcript: what the user said on the left, what the agent did about it on the right. */
export interface Row {
  key: string;
  /** The turn the message opened or, for a steering message, joined. */
  turn: number;
  /** `files`: what was attached, each with whether its text went with the message, and for a document what kind and its parts. */
  prompt: {
    text: string;
    ts: string;
    steering: boolean;
    images: Image[];
    files: { path: string; bytes: number; inline: boolean; document?: DocumentKind; parts?: number }[];
  };
  items: Item[];
}

/** The server and the tool a call to an MCP server's tool names, from `mcp__<server>__<tool>`; null for a built-in tool. */
export function mcpTool(name: string): { server: string; tool: string } | null {
  const match = /^mcp__(.+?)__(.+)$/.exec(name);
  return match ? { server: match[1], tool: match[2] } : null;
}

/** Whether a tool changes files: its result carries a diff and can be undone. */
export const editsFiles = (name: string) => name === "edit_file" || name === "write_file";

function blockItem(block: ContentBlock, turn: number): Item | null {
  switch (block.type) {
    case "text":
      return block.text.trim() ? { kind: "text", text: block.text } : null;
    case "thinking":
      return block.thinking.trim() ? { kind: "thinking", text: block.thinking } : null;
    case "tool_use":
      return { kind: "tool", id: block.id, name: block.name, input: block.input ?? {}, turn };
    case "fallback":
      return { kind: "fallback", from: block.from.model, to: block.to.model };
    default:
      // Block types added to the API later have nothing to show yet.
      return null;
  }
}

export function fold(events: Envelope[]): Row[] {
  const rows: Row[] = [];
  const tools = new Map<string, ToolItem>();
  const current = () => rows[rows.length - 1];
  let turn = 0;
  for (const event of events) {
    switch (event.type) {
      case "turn/start":
        turn = event.data.turn;
        break;
      case "user/message":
        rows.push({ key: `row-${event.seq}`, turn, prompt: {
            text: event.data.text,
            ts: event.ts,
            steering: event.data.steering,
            images: event.data.images ?? [],
            files: (event.data.files ?? []).map((file) => ({
              path: file.path,
              bytes: file.bytes,
              inline: file.content !== undefined,
              document: file.document,
              parts: file.parts,
            })),
          },
          items: [],
        });
        break;
      case "files/restored":
        for (const tool of tools.values()) {
          if (tool.turn >= event.data.turn && editsFiles(tool.name) && event.data.restored.includes(tool.result?.meta?.path ?? "")) tool.undone = true;
        }
        current()?.items.push({ kind: "restored", turn: event.data.turn, restored: event.data.restored, skipped: event.data.skipped });
        break;
      case "assistant/message":
        for (const block of event.data.content) {
          const item = blockItem(block, turn);
          if (!item || !current()) continue;
          current().items.push(item);
          if (item.kind === "tool") tools.set(item.id, item);
        }
        break;
      case "subagent/started": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool) tool.agents = [...(tool.agents ?? []), { session: event.data.session, name: event.data.name }];
        break;
      }
      case "approval/requested": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool) tool.approval = { reason: event.data.reason, detail: event.data.detail, grant: event.data.grant, expiresAt: event.data.expires_at };
        break;
      }
      case "approval/resolved": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool?.approval) {
          tool.approval.decision = event.data.decision;
          tool.approval.timedOut = !!event.data.timed_out;
        }
        break;
      }
      case "guardian/reviewed": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool) tool.review = { allowed: event.data.allowed, reason: event.data.reason, model: event.data.model, failed: !!event.data.failed };
        break;
      }
      case "tool/result": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool) tool.result = { content: event.data.content, isError: event.data.is_error, meta: event.data.meta };
        break;
      }
      case "instructions/loaded":
        current()?.items.push({ kind: "instructions", path: event.data.path, content: event.data.content });
        break;
      case "context/compacted":
        current()?.items.push({ kind: "compacted", summary: event.data.summary, tokensBefore: event.data.tokens_before });
        break;
      case "turn/end":
        if (event.data.reason !== "completed" && current()) {
          current().items.push({ kind: "notice", reason: event.data.reason, error: event.data.error });
        }
        break;
      default:
        break;
    }
  }
  return rows;
}

/**
 * A count of tokens or characters in the K and M that models' windows are given in: 123456 as 123K, 1048576 as
 * 1.05M. Below ten of a unit it keeps a decimal, which is where the difference shows.
 */
export const shortCount = (count: number) => {
  const scaled = (unit: number, suffix: string) => {
    const value = count / unit;
    const places = value < 10 ? (unit === 1_000_000 ? 2 : 1) : 0;
    return `${Number(value.toFixed(places))}${suffix}`;
  };
  if (count >= 1_000_000) return scaled(1_000_000, "M");
  if (count >= 1_000) return scaled(1_000, "K");
  return String(count);
};

/** What the session has spent, and how full its context is. */
export interface Usage {
  /** Tokens the conversation holds now: what the last request sent and got back. */
  context: number;
  /** Totals over every request of the session, summaries included. */
  spent: Tokens;
}

export function usage(events: Envelope[]): Usage {
  const spent: Tokens = { input: 0, cached: 0, output: 0 };
  let context = 0;
  const add = (tokens: Tokens | undefined) => {
    if (!tokens) return;
    spent.input += tokens.input;
    spent.cached += tokens.cached;
    spent.output += tokens.output;
  };
  for (const event of events) {
    if (event.type === "assistant/message") {
      add(event.data.tokens);
      if (event.data.tokens) context = event.data.tokens.input + event.data.tokens.output;
    } else if (event.type === "guardian/reviewed") {
      // The 护法's requests are spent too; they are no part of the conversation.
      add(event.data.tokens);
    } else if (event.type === "context/compacted") {
      add(event.data.tokens);
      // Only the summary is left, which is what its request wrote.
      context = event.data.tokens?.output ?? 0;
    }
  }
  return { context, spent };
}

/** A content block of the message being generated. */
export interface StreamBlock {
  index: number;
  kind: "text" | "thinking" | "tool";
  text: string;
  name?: string;
}

export function applyDelta(blocks: StreamBlock[], delta: StreamDelta): StreamBlock[] {
  if (delta.type === "reset") return [];
  if (delta.type === "block_stop") return blocks;
  const next = blocks.slice();
  const at = next.findIndex((block) => block.index === delta.index);
  const existing = at >= 0 ? next[at] : undefined;
  const put = (block: StreamBlock) => {
    if (at >= 0) next[at] = block;
    else next.push(block);
  };
  switch (delta.type) {
    case "text_delta":
      put({ index: delta.index, kind: "text", text: (existing?.text ?? "") + delta.text });
      break;
    case "thinking_delta":
      put({ index: delta.index, kind: "thinking", text: (existing?.text ?? "") + delta.text });
      break;
    case "tool_use_start":
      put({ index: delta.index, kind: "tool", text: "", name: delta.name });
      break;
    case "tool_input_delta":
      put({ index: delta.index, kind: "tool", text: (existing?.text ?? "") + delta.partial, name: existing?.name });
      break;
  }
  return next;
}

/** A file's size as it is said: bytes, then KB, then MB with a decimal. */
export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** What a document attached is, as its chip says it: `3 页`, `Word 文档`, `2 个工作表`, `12 张幻灯片`. */
export function documentLabel(document: DocumentKind, parts?: number): string {
  switch (document) {
    case "pdf":
      return `${parts ?? 0} 页`;
    case "word":
      return "Word 文档";
    case "excel":
      return `${parts ?? 0} 个工作表`;
    case "powerpoint":
      return `${parts ?? 0} 张幻灯片`;
  }
}

export interface ChangedFile {
  path: string;
  added: number;
  removed: number;
  /** The 分身 made the file, rather than changing one that was there. */
  created: boolean;
  /** Each edit's change, in the order they were made. */
  hunks: DiffHunk[];
}

/** Files the agent edited in this session, with line counts summed per file, in first-edit order. */
export function changedFiles(rows: Row[]): ChangedFile[] {
  const files = new Map<string, ChangedFile>();
  for (const item of rows.flatMap((row) => row.items)) {
    if (item.kind !== "tool" || !editsFiles(item.name) || !item.result || item.result.isError || item.undone) continue;
    const meta = item.result.meta;
    if (!meta?.path) continue;
    const file = files.get(meta.path) ?? { path: meta.path, added: 0, removed: 0, created: !!meta.created, hunks: [] };
    file.added += meta.added ?? 0;
    file.removed += meta.removed ?? 0;
    if (meta.hunk) file.hunks.push(meta.hunk);
    files.set(meta.path, file);
  }
  return [...files.values()];
}

/** An edit the user can still undo: it succeeded, has a snapshot, and was not undone. */
const undoable = (item: Item) => item.kind === "tool" && editsFiles(item.name) && !!item.result && !item.result.isError && !!item.result.meta?.after && !item.undone;

/**
 * The turns from which undoing would restore something: each turn that has
 * an edit left to undo, or comes before one that has. Undoing from a turn
 * takes every later turn's edits with it.
 */
export function undoableTurns(rows: Row[], copied = 0): Set<number> {
  // In a fork, the turns copied from the other session are that session's to undo: it keeps what the files held.
  const own = rows.filter((row) => row.turn > copied);
  const last = Math.max(0, ...own.filter((row) => row.items.some(undoable)).map((row) => row.turn));
  return new Set(own.map((row) => row.turn).filter((turn) => turn > 0 && turn <= last));
}

/** The to-do list as the agent last wrote it; empty when it never wrote one. */
export function todos(rows: Row[]): Todo[] {
  const written = rows.flatMap((row) => row.items).filter((item): item is ToolItem => item.kind === "tool" && item.name === "update_todos" && !!item.result && !item.result.isError);
  return written[written.length - 1]?.result?.meta?.todos ?? [];
}

/** Commands that ran, in order, for the terminal panel. */
export function commandRuns(rows: Row[]): ToolItem[] {
  // A background command's output arrives through the calls that read or stop it.
  const shows = (name: string) => name === "run_command" || name === "command_output" || name === "stop_command";
  return rows.flatMap((row) => row.items).filter((item): item is ToolItem => item.kind === "tool" && shows(item.name) && !!item.result?.meta?.command);
}

/** The approval the session is waiting on, if any. */
export function pendingApproval(rows: Row[]): ToolItem | undefined {
  return rows
    .flatMap((row) => row.items)
    .find((item): item is ToolItem => item.kind === "tool" && !!item.approval && !item.approval.decision && !item.result);
}

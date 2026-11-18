// Folds the session log into what the window draws. The log is the only input,
// so reopening a session shows exactly what was on screen before.

import type { ContentBlock, Decision, Envelope, StreamDelta, Tokens, ToolMeta } from "./types";

export interface ToolItem {
  kind: "tool";
  id: string;
  name: string;
  input: Record<string, unknown>;
  result?: { content: string; isError: boolean; meta: ToolMeta | null };
  /** `grant` is what allowing for the session covers; without one, the request is approved each time. */
  approval?: { reason: string; detail: string; grant?: string; decision?: Decision };
  /** The turn the call belongs to. */
  turn: number;
  /** The user undid this edit. */
  undone?: boolean;
}

export type Item =
  | { kind: "text"; text: string }
  | { kind: "thinking"; text: string }
  | ToolItem
  | { kind: "fallback"; from: string; to: string }
  | { kind: "compacted"; summary: string; tokensBefore: number }
  | { kind: "restored"; turn: number; restored: string[]; skipped: string[] }
  | { kind: "notice"; reason: string; error: string | null };

/** One row of the transcript: what the user said on the left, what the agent did about it on the right. */
export interface Row {
  key: string;
  /** The turn the message opened or, for a steering message, joined. */
  turn: number;
  prompt: { text: string; ts: string; steering: boolean };
  items: Item[];
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
        rows.push({ key: `row-${event.seq}`, turn, prompt: { text: event.data.text, ts: event.ts, steering: event.data.steering }, items: [] });
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
      case "approval/requested": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool) tool.approval = { reason: event.data.reason, detail: event.data.detail, grant: event.data.grant };
        break;
      }
      case "approval/resolved": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool?.approval) tool.approval.decision = event.data.decision;
        break;
      }
      case "tool/result": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool) tool.result = { content: event.data.content, isError: event.data.is_error, meta: event.data.meta };
        break;
      }
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

/** 123456 as 12.3 万, the way large counts read in Chinese. */
export const shortCount = (count: number) => (count >= 10000 ? `${(count / 10000).toFixed(count >= 1000000 ? 0 : 1).replace(/\.0$/, "")} 万` : String(count));

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

export interface ChangedFile {
  path: string;
  added: number;
  removed: number;
}

/** Files the agent edited in this session, with line counts summed per file, in first-edit order. */
export function changedFiles(rows: Row[]): ChangedFile[] {
  const files = new Map<string, ChangedFile>();
  for (const item of rows.flatMap((row) => row.items)) {
    if (item.kind !== "tool" || !editsFiles(item.name) || !item.result || item.result.isError || item.undone) continue;
    const meta = item.result.meta;
    if (!meta?.path) continue;
    const file = files.get(meta.path) ?? { path: meta.path, added: 0, removed: 0 };
    file.added += meta.added ?? 0;
    file.removed += meta.removed ?? 0;
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
export function undoableTurns(rows: Row[]): Set<number> {
  const last = Math.max(0, ...rows.filter((row) => row.items.some(undoable)).map((row) => row.turn));
  return new Set(rows.map((row) => row.turn).filter((turn) => turn > 0 && turn <= last));
}

/** Commands that ran, in order, for the terminal panel. */
export function commandRuns(rows: Row[]): ToolItem[] {
  return rows.flatMap((row) => row.items).filter((item): item is ToolItem => item.kind === "tool" && item.name === "run_command" && !!item.result?.meta);
}

/** The approval the session is waiting on, if any. */
export function pendingApproval(rows: Row[]): ToolItem | undefined {
  return rows
    .flatMap((row) => row.items)
    .find((item): item is ToolItem => item.kind === "tool" && !!item.approval && !item.approval.decision && !item.result);
}

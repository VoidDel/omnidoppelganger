// Folds the session log into what the window draws. The log is the only input,
// so reopening a session shows exactly what was on screen before.

import type { ContentBlock, Decision, DiffHunk, DocumentKind, Envelope, Image, Question, Reply, StreamDelta, Todo, Tokens, ToolMeta } from "./types";

export interface ToolItem {
  kind: "tool";
  id: string;
  name: string;
  input: Record<string, unknown>;
  result?: { content: string; isError: boolean; meta: ToolMeta | null };
  /** `grant` is what allowing for the session covers; without one, the request is approved each time. */
  approval?: { reason: string; detail: string; grant?: string; decision?: Decision; expiresAt?: string; timedOut?: boolean };
  /**
   * What an `ask_user` call put to 本尊, and once it is over what came of it: an answer to each question, or none
   * when 本尊 left it to the 分身 (`declined`) or nobody answered in time (`timedOut`).
   */
  question?: { questions: Question[]; expiresAt?: string; answered?: { answers: Reply[]; declined: boolean; timedOut: boolean } };
  /** What the 护法 made of the call: let it run, or left it to 本尊; `failed` when it could not be asked. */
  review?: { allowed: boolean; reason: string; model: string; failed: boolean };
  /** The turn the call belongs to. */
  turn: number;
  /** Timestamp of the tool's result event, when completed. */
  ts?: string;
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
  /** A deliverable restored to an earlier snapshotted version. */
  | { kind: "artifact_restored"; file: string; path: string; version: number; snapshot: string; bytes: number; previous?: string; previousBytes?: number; ts?: string }
  /** Files the turn left in the workspace that no tool told of (or modified after a tool told of them): absolute paths. */
  | { kind: "made"; turn: number; files: string[]; snapshots?: Record<string, string>; sizes?: Record<string, number>; ts?: string }
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
export const editsFiles = (name: string) => name === "edit_file" || name === "write_file" || name === "delete_file" || name === "move_file";

/** The paths, as shown, of the files an edit changed: a move changes two. */
const touched = (tool: ToolItem) => [tool.result?.meta?.path, tool.result?.meta?.to].filter((path): path is string => !!path);

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
          if (tool.turn >= event.data.turn && editsFiles(tool.name) && touched(tool).some((path) => event.data.restored.includes(path))) tool.undone = true;
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
      case "question/asked": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool) tool.question = { questions: event.data.questions, expiresAt: event.data.expires_at };
        break;
      }
      case "question/answered": {
        const tool = tools.get(event.data.tool_use_id);
        if (tool?.question) tool.question.answered = { answers: event.data.answers ?? [], declined: !!event.data.declined, timedOut: !!event.data.timed_out };
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
        if (tool) {
          tool.result = { content: event.data.content, isError: event.data.is_error, meta: event.data.meta };
          tool.ts = event.ts;
        }
        break;
      }
      case "instructions/loaded":
        current()?.items.push({ kind: "instructions", path: event.data.path, content: event.data.content });
        break;
      case "files/made": {
        // What the turn's own acts already show at the same content snapshot is not said twice.
        const shown = new Map<string, string | undefined>();
        for (const item of current()?.items ?? []) {
          if (item.kind !== "tool" || !item.result || item.result.isError) continue;
          const meta = item.result.meta;
          if (item.name === "deliver_files" && Array.isArray(meta?.files)) {
            for (const path of meta.files) shown.set(path, meta.snapshots?.[path]);
          } else if ((item.name === "edit_file" || item.name === "write_file" || item.name === "generate_image") && meta?.file) {
            shown.set(meta.file, meta.snapshot);
          }
        }
        const madeSnaps = event.data.snapshots;
        const files = event.data.files.filter((path) => {
          if (!shown.has(path)) return true;
          const madeSnap = madeSnaps?.[path];
          const prevSnap = shown.get(path);
          return !!madeSnap && !!prevSnap && madeSnap !== prevSnap;
        });
        if (files.length) {
          const item: Extract<Item, { kind: "made" }> = { kind: "made", turn: event.data.turn, files, ts: event.ts };
          if (madeSnaps) {
            const keptSnaps: Record<string, string> = {};
            const keptSizes: Record<string, number> = {};
            for (const path of files) {
              if (madeSnaps[path]) keptSnaps[path] = madeSnaps[path];
              if (event.data.sizes?.[path] !== undefined) keptSizes[path] = event.data.sizes[path];
            }
            if (Object.keys(keptSnaps).length) item.snapshots = keptSnaps;
            if (Object.keys(keptSizes).length) item.sizes = keptSizes;
          }
          current()?.items.push(item);
        }
        break;
      }
      case "artifact/restored":
        current()?.items.push({
          kind: "artifact_restored",
          file: event.data.file,
          path: event.data.path,
          version: event.data.version,
          snapshot: event.data.snapshot,
          bytes: event.data.bytes,
          previous: event.data.previous,
          previousBytes: event.data.previous_bytes,
          ts: event.ts,
        });
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
    } else if (event.type === "tool/result") {
      // So is a search a provider ran in a request of its own.
      add(event.data.meta?.tokens);
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
  /** Where it is on disk, to be opened from the list; absent in logs from before it was kept. */
  file?: string;
  /** Each edit's change, in the order they were made. */
  hunks: DiffHunk[];
  /** The 分身 deleted it, or moved it away, last: it is not there to open. */
  gone?: boolean;
  /** It came here by a move, from this path. */
  movedFrom?: string;
}

/** Files the agent edited in this session, with line counts summed per file, in first-edit order. */
export function changedFiles(rows: Row[]): ChangedFile[] {
  const files = new Map<string, ChangedFile>();
  for (const item of rows.flatMap((row) => row.items)) {
    if (item.kind !== "tool" || !editsFiles(item.name) || !item.result || item.result.isError || item.undone) continue;
    const meta = item.result.meta;
    if (!meta?.path) continue;
    const entry = (path: string): ChangedFile => files.get(path) ?? { path, added: 0, removed: 0, created: false, hunks: [] };
    if (item.name === "move_file" && meta.to) {
      // Where it went is the row that counts; where it was, if already listed, is now empty.
      const left = files.get(meta.path);
      if (left) Object.assign(left, { gone: true, file: undefined });
      const arrived = entry(meta.to);
      Object.assign(arrived, { movedFrom: left?.movedFrom ?? meta.path, gone: undefined, file: meta.file ?? arrived.file });
      files.set(meta.to, arrived);
      continue;
    }
    const file = entry(meta.path);
    if (!files.has(meta.path)) file.created = !!meta.created;
    // Gone if deleted last; written again, it is back.
    file.gone = item.name === "delete_file" || undefined;
    file.file = file.gone ? undefined : (meta.file ?? file.file);
    file.added += meta.added ?? 0;
    file.removed += meta.removed ?? 0;
    if (meta.hunks?.length) file.hunks.push(...meta.hunks);
    else if (meta.hunk) file.hunks.push(meta.hunk);
    files.set(meta.path, file);
  }
  return [...files.values()];
}

/** Document and visual files written via `write_file`/`edit_file` that count as user deliverables. */
const WRITTEN_DELIVERABLE = /\.(md|markdown|svg|html|htm|csv)$/i;
const SKILL_FILE = /(?:^|[\\/])\.odg[\\/]skills[\\/]/i;
const isWrittenDeliverable = (path: string) => WRITTEN_DELIVERABLE.test(path) && !SKILL_FILE.test(path);

/** One snapshotted version of a deliverable recorded during the session. */
export interface ProductVersion {
  /** 1-based version number, oldest first. */
  version: number;
  /** Turn in which this version was captured. */
  turn: number;
  how: "painted" | "delivered" | "written" | "found" | "backup";
  /** Content-addressed snapshot name in the session's `snapshots/` folder, when captured. */
  snapshot?: string;
  /** Size in bytes when captured. */
  bytes?: number;
  /** ISO timestamp when this version was recorded. */
  ts?: string;
}

/** A file the 分身 made for 本尊: painted, handed over, written as a document/chart, or found at turn end, with its version history. */
export interface Product {
  /** Where it is on disk. */
  path: string;
  /** Turn of the latest version. */
  turn: number;
  /** How it came: painted, delivered, written, or found. More explicit acts outrank passive finding. */
  how: "painted" | "delivered" | "written" | "found";
  /** All distinct versions recorded across the session, oldest first (v1, v2, ...). */
  versions: ProductVersion[];
  /** Snapshot currently active on disk (latest version by default, or the restored snapshot). */
  currentSnapshot?: string;
}

const HOW_RANK: Record<Product["how"], number> = { found: 0, written: 1, delivered: 2, painted: 3 };
const bestHow = (a: Product["how"], b: Product["how"]): Product["how"] => (HOW_RANK[b] > HOW_RANK[a] ? b : a);

/** What the 分身 made for 本尊 in this session, newest first, each with its version history. */
export function products(rows: Row[]): Product[] {
  const made = new Map<string, Product>();
  const record = (path: string, turn: number, how: Product["how"], snapshot?: string, bytes?: number, ts?: string) => {
    const existing = made.get(path);
    if (!existing) {
      made.set(path, {
        path,
        turn,
        how,
        versions: [{ version: 1, turn, how, ...(snapshot ? { snapshot } : {}), ...(bytes !== undefined ? { bytes } : {}), ...(ts ? { ts } : {}) }],
        ...(snapshot ? { currentSnapshot: snapshot } : {}),
      });
      return;
    }
    const last = existing.versions[existing.versions.length - 1];
    existing.how = bestHow(existing.how, how);
    if (snapshot && last?.snapshot === snapshot) {
      // Same exact bytes already recorded as the latest version: upgrade metadata without creating a duplicate version.
      if (last.how !== "backup") last.how = bestHow(last.how, how);
      if (last.bytes === undefined && bytes !== undefined) last.bytes = bytes;
      if (!last.ts && ts) last.ts = ts;
      existing.currentSnapshot = snapshot;
    } else if (!snapshot && !last?.snapshot) {
      // Legacy log without snapshots: update the single record in place.
      existing.turn = turn;
      if (last) {
        last.turn = turn;
        if (last.how !== "backup") last.how = bestHow(last.how, how);
      }
    } else {
      existing.turn = turn;
      existing.versions.push({
        version: existing.versions.length + 1,
        turn,
        how,
        ...(snapshot ? { snapshot } : {}),
        ...(bytes !== undefined ? { bytes } : {}),
        ...(ts ? { ts } : {}),
      });
      if (snapshot) existing.currentSnapshot = snapshot;
    }
    // Move to the end so the most recently updated product appears first when reversed.
    made.delete(path);
    made.set(path, existing);
  };

  for (const row of rows) {
    for (const item of row.items) {
      if (item.kind === "made") {
        for (const path of item.files) {
          record(path, item.turn, "found", item.snapshots?.[path], item.sizes?.[path], item.ts);
        }
      } else if (item.kind === "tool" && item.result && !item.result.isError) {
        const meta = item.result.meta;
        if (item.name === "deliver_files" && Array.isArray(meta?.files)) {
          for (const path of meta.files) {
            record(path, item.turn, "delivered", meta.snapshots?.[path], meta.sizes?.[path], item.ts);
          }
        } else if (item.name === "generate_image" && meta?.file) {
          record(meta.file, item.turn, "painted", meta.snapshot, meta.bytes, item.ts);
        } else if ((item.name === "write_file" || item.name === "edit_file") && meta?.file && isWrittenDeliverable(meta.file)) {
          record(meta.file, item.turn, "written", meta.snapshot, meta.bytes, item.ts);
        }
      } else if (item.kind === "artifact_restored") {
        const existing = made.get(item.file);
        if (existing) {
          if (item.previous && !existing.versions.some((v) => v.snapshot === item.previous)) {
            existing.versions.push({
              version: existing.versions.length + 1,
              turn: row.turn,
              how: "backup",
              snapshot: item.previous,
              ...(item.previousBytes !== undefined ? { bytes: item.previousBytes } : {}),
              ...(item.ts ? { ts: item.ts } : {}),
            });
          }
          existing.currentSnapshot = item.snapshot;
          made.delete(item.file);
          made.set(item.file, existing);
        }
      }
    }
  }
  return [...made.values()].reverse();
}

/** An edit the user can still undo: it succeeded, has a snapshot, and was not undone. */
const undoable = (item: Item) =>
  item.kind === "tool" && editsFiles(item.name) && !!item.result && !item.result.isError && !!(item.result.meta?.after || item.result.meta?.changes?.length) && !item.undone;

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
  const shows = (name: string) => name === "run_command" || name === "command_output" || name === "command_input" || name === "stop_command";
  return rows.flatMap((row) => row.items).filter((item): item is ToolItem => item.kind === "tool" && shows(item.name) && !!item.result?.meta?.command);
}

/** Whether an `ask_user` call is waiting on 本尊's answers: asked, and neither answered nor ended. */
export const asking = (tool: ToolItem) => !!tool.question && !tool.question.answered && !tool.result;

/** The approval the session is waiting on, if any. */
export function pendingApproval(rows: Row[]): ToolItem | undefined {
  return rows
    .flatMap((row) => row.items)
    .find((item): item is ToolItem => item.kind === "tool" && !!item.approval && !item.approval.decision && !item.result);
}

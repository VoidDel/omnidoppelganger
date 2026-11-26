// Folds the session log into what the window draws. The log is the only input,
// so reopening a session shows exactly what was on screen before.

import { formatMessage, getCurrentLocale, type Locale } from "../i18n";
import { estimateTokensUsd } from "./pricing";
import type {
  ContentBlock,
  Decision,
  DiffHunk,
  DocumentKind,
  Envelope,
  Image,
  Question,
  Reply,
  StreamDelta,
  Todo,
  Tokens,
  ToolMeta,
} from "./types";

export interface ToolItem {
  kind: "tool";
  id: string;
  name: string;
  input: Record<string, unknown>;
  result?: { content: string; isError: boolean; meta: ToolMeta | null; images?: Image[] };
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
  | { kind: "compacted"; summary: string; tokensBefore: number; focus?: string; keptFromTurn?: number }
  | { kind: "restored"; turn: number; restored: string[]; skipped: string[] }
  /** A deliverable restored to an earlier snapshotted version. */
  | {
      kind: "artifact_restored";
      file: string;
      path: string;
      version: number;
      snapshot: string;
      bytes: number;
      previous?: string;
      previousBytes?: number;
      ts?: string;
    }
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
    files: { path: string; bytes: number; inline: boolean; document?: DocumentKind; parts?: number; is_dir?: boolean }[];
  };
  items: Item[];
}

/** The server and the tool a call to an MCP server's tool names, from `mcp__<server>__<tool>`; null for a built-in tool. */
export function mcpTool(name: string): { server: string; tool: string } | null {
  const match = /^mcp__(.+?)__(.+)$/.exec(name);
  return match ? { server: match[1], tool: match[2] } : null;
}

/** Whether a tool changes files: its result carries a diff and can be undone. */
export const editsFiles = (name: string) =>
  name === "edit_file" || name === "write_file" || name === "delete_file" || name === "move_file";

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

interface ToolLocation {
  rowIdx: number;
  itemIdx: number;
}

export interface FoldState {
  rows: Row[];
  tools: Map<string, ToolLocation>;
  turn: number;
  /** Rows with index `< frozenCount` belong to an earlier snapshot and must be copied on write before mutation. */
  frozenCount: number;
  clonedRows: Set<number>;
}

export function createFoldState(): FoldState {
  return {
    rows: [],
    tools: new Map(),
    turn: 0,
    frozenCount: 0,
    clonedRows: new Set(),
  };
}

function ensureMutableRow(state: FoldState, rowIdx: number): Row | undefined {
  const existing = state.rows[rowIdx];
  if (!existing) return undefined;
  if (rowIdx < state.frozenCount && !state.clonedRows.has(rowIdx)) {
    const cloned: Row = { ...existing, items: existing.items.slice() };
    state.rows[rowIdx] = cloned;
    state.clonedRows.add(rowIdx);
    return cloned;
  }
  return existing;
}

function ensureMutableTool(state: FoldState, id: string): ToolItem | undefined {
  const loc = state.tools.get(id);
  if (!loc) return undefined;
  const row = state.rows[loc.rowIdx];
  if (!row) return undefined;
  const item = row.items[loc.itemIdx];
  if (!item || item.kind !== "tool") return undefined;
  if (loc.rowIdx < state.frozenCount) {
    const mutRow = ensureMutableRow(state, loc.rowIdx)!;
    const cur = mutRow.items[loc.itemIdx] as ToolItem;
    const clonedTool: ToolItem = {
      ...cur,
      ...(cur.approval ? { approval: { ...cur.approval } } : {}),
      ...(cur.question ? { question: { ...cur.question } } : {}),
    };
    mutRow.items[loc.itemIdx] = clonedTool;
    return clonedTool;
  }
  return item;
}

/** Advance `FoldState` by a single `Envelope`, preserving strict `===` references for untouched frozen rows. */
export function foldStep(state: FoldState, event: Envelope): void {
  const pushToCurrent = (item: Item): ToolLocation | null => {
    const rowIdx = state.rows.length - 1;
    if (rowIdx < 0) return null;
    const row = ensureMutableRow(state, rowIdx)!;
    const itemIdx = row.items.length;
    row.items.push(item);
    return { rowIdx, itemIdx };
  };

  switch (event.type) {
    case "turn/start":
      state.turn = event.data.turn;
      break;
    case "user/message":
      state.rows.push({
        key: `row-${event.seq}`,
        turn: state.turn,
        prompt: {
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
            is_dir: file.is_dir,
          })),
        },
        items: [],
      });
      break;
    case "files/restored":
      for (const [id, loc] of state.tools.entries()) {
        const candidate = state.rows[loc.rowIdx]?.items[loc.itemIdx];
        if (
          candidate &&
          candidate.kind === "tool" &&
          candidate.turn >= event.data.turn &&
          editsFiles(candidate.name) &&
          touched(candidate).some((path) => event.data.restored.includes(path))
        ) {
          const mutTool = ensureMutableTool(state, id);
          if (mutTool) mutTool.undone = true;
        }
      }
      pushToCurrent({ kind: "restored", turn: event.data.turn, restored: event.data.restored, skipped: event.data.skipped });
      break;
    case "assistant/message":
      for (const block of event.data.content) {
        const item = blockItem(block, state.turn);
        if (!item || state.rows.length === 0) continue;
        const loc = pushToCurrent(item);
        if (loc && item.kind === "tool") state.tools.set(item.id, loc);
      }
      break;
    case "subagent/started": {
      const tool = ensureMutableTool(state, event.data.tool_use_id);
      if (tool) tool.agents = [...(tool.agents ?? []), { session: event.data.session, name: event.data.name }];
      break;
    }
    case "approval/requested": {
      const tool = ensureMutableTool(state, event.data.tool_use_id);
      if (tool) {
        tool.approval = {
          reason: event.data.reason,
          detail: event.data.detail,
          grant: event.data.grant,
          expiresAt: event.data.expires_at,
        };
      }
      break;
    }
    case "question/asked": {
      const tool = ensureMutableTool(state, event.data.tool_use_id);
      if (tool) tool.question = { questions: event.data.questions, expiresAt: event.data.expires_at };
      break;
    }
    case "question/answered": {
      const tool = ensureMutableTool(state, event.data.tool_use_id);
      if (tool?.question) {
        tool.question = {
          ...tool.question,
          answered: {
            answers: event.data.answers ?? [],
            declined: !!event.data.declined,
            timedOut: !!event.data.timed_out,
          },
        };
      }
      break;
    }
    case "approval/resolved": {
      const tool = ensureMutableTool(state, event.data.tool_use_id);
      if (tool?.approval) {
        tool.approval = {
          ...tool.approval,
          decision: event.data.decision,
          timedOut: !!event.data.timed_out,
        };
      }
      break;
    }
    case "guardian/reviewed": {
      const tool = ensureMutableTool(state, event.data.tool_use_id);
      if (tool) {
        tool.review = {
          allowed: event.data.allowed,
          reason: event.data.reason,
          model: event.data.model,
          failed: !!event.data.failed,
        };
      }
      break;
    }
    case "tool/result": {
      const tool = ensureMutableTool(state, event.data.tool_use_id);
      if (tool) {
        tool.result = {
          content: event.data.content,
          isError: event.data.is_error,
          meta: event.data.meta,
          images: event.data.images,
        };
        tool.ts = event.ts;
      }
      break;
    }
    case "instructions/loaded":
      pushToCurrent({ kind: "instructions", path: event.data.path, content: event.data.content });
      break;
    case "files/made": {
      const curRow = state.rows[state.rows.length - 1];
      const shown = new Map<string, string | undefined>();
      for (const item of curRow?.items ?? []) {
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
        pushToCurrent(item);
      }
      break;
    }
    case "artifact/restored":
      pushToCurrent({
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
      pushToCurrent({
        kind: "compacted",
        summary: event.data.summary,
        tokensBefore: event.data.tokens_before,
        focus: event.data.focus,
        keptFromTurn: event.data.kept_from_turn,
      });
      break;
    case "turn/end":
      if (event.data.reason !== "completed" && state.rows.length > 0) {
        pushToCurrent({ kind: "notice", reason: event.data.reason, error: event.data.error });
      }
      break;
    default:
      break;
  }
}

export interface FoldCache {
  events: Envelope[];
  length: number;
  firstEvent: Envelope | undefined;
  lastEvent: Envelope | undefined;
  state: FoldState;
  rows: Row[];
}

/**
 * Incrementally fold `events` from `prev` when `events` appends to the previously folded log,
 * freezing untouched historical `Row` references (`===`) so completed turns skip re-rendering.
 */
export function foldIncremental(prev: FoldCache | null, events: Envelope[]): FoldCache {
  if (
    prev &&
    events.length >= prev.length &&
    (prev.length === 0 ||
      (events[0] === prev.firstEvent && events[prev.length - 1] === prev.lastEvent))
  ) {
    if (events.length === prev.length) return prev;
    const nextState: FoldState = {
      rows: prev.state.rows.slice(),
      tools: new Map(prev.state.tools),
      turn: prev.state.turn,
      frozenCount: prev.state.rows.length,
      clonedRows: new Set(),
    };
    for (let i = prev.length; i < events.length; i++) {
      foldStep(nextState, events[i]);
    }
    return {
      events,
      length: events.length,
      firstEvent: events[0],
      lastEvent: events[events.length - 1],
      state: nextState,
      rows: nextState.rows,
    };
  }

  const fresh = createFoldState();
  for (const event of events) {
    foldStep(fresh, event);
  }
  return {
    events,
    length: events.length,
    firstEvent: events[0],
    lastEvent: events[events.length - 1],
    state: fresh,
    rows: fresh.rows,
  };
}

export function fold(events: Envelope[]): Row[] {
  return foldIncremental(null, events).rows;
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

/** Per-category token breakdown of the active conversation context. */
export interface UsageBreakdown {
  /** System prompt, tool schemas, and project instructions (CLAUDE.md / AGENTS.md). */
  prompt: number;
  /** User messages and attached images/files. */
  input: number;
  /** Assistant text, thinking, and tool call arguments. */
  output: number;
  /** Listed skill catalog and `load_skill` contents. */
  skill: number;
  /** MCP tool calls and MCP resource reads. */
  mcp: number;
  /** Built-in tool outputs (`read_file`, `run_command`, `grep`, etc.). */
  tools: number;
}

export interface Usage {
  /** Tokens the conversation holds now: what the last request sent and got back. */
  context: number;
  /** Totals over every request of the session, summaries included. */
  spent: Tokens;
  /** Breakdown of the current context window by category. */
  breakdown: UsageBreakdown;
}

/** Fast CJK + ASCII token estimator for categorizing context usage. */
export function estimateTokens(text: string | undefined | null): number {
  if (!text) return 0;
  let cjk = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if ((code >= 0x4e00 && code <= 0x9fff) || (code >= 0x3000 && code <= 0x303f) || (code >= 0xff00 && code <= 0xffef)) {
      cjk++;
    }
  }
  const other = text.length - cjk;
  return Math.ceil(cjk * 0.7 + other / 4);
}

interface CachedTokenEst {
  input: number;
  output: number;
  skill: number;
  mcp: number;
  tools: number;
  instructions?: number;
  skillsCatalog?: number;
  summary?: number;
}

const tokenEstCache = new WeakMap<Envelope, CachedTokenEst>();

function estimateEnvelopeTokens(event: Envelope): CachedTokenEst {
  const cached = tokenEstCache.get(event);
  if (cached) return cached;

  const est: CachedTokenEst = { input: 0, output: 0, skill: 0, mcp: 0, tools: 0 };
  if (event.type === "instructions/loaded") {
    est.instructions = estimateTokens(event.data.content);
  } else if (event.type === "skills/listed") {
    est.skillsCatalog = event.data.skills.reduce(
      (sum, s) => sum + estimateTokens(s.name) + estimateTokens(s.description) + 6,
      0,
    );
  } else if (event.type === "user/message") {
    est.input += estimateTokens(event.data.text);
    est.input += (event.data.images?.length ?? 0) * 1000;
    for (const file of event.data.files ?? []) {
      est.input += file.content ? estimateTokens(file.content) : 30;
    }
  } else if (event.type === "assistant/message") {
    for (const block of event.data.content ?? []) {
      if (block.type === "text") est.output += estimateTokens(block.text);
      else if (block.type === "thinking") est.output += estimateTokens(block.thinking);
      else if (block.type === "tool_use") {
        const argTokens = estimateTokens(JSON.stringify(block.input ?? {})) + 12;
        if (block.name === "load_skill") est.skill += argTokens;
        else if (mcpTool(block.name) || block.name === "list_mcp_resources" || block.name === "read_mcp_resource") {
          est.mcp += argTokens;
        } else {
          est.output += argTokens;
        }
      }
    }
  } else if (event.type === "tool/result") {
    const bodyTokens = estimateTokens(event.data.content) + (event.data.images?.length ?? 0) * 1000;
    if (event.data.tool === "load_skill") est.skill += bodyTokens;
    else if (mcpTool(event.data.tool) || event.data.tool === "list_mcp_resources" || event.data.tool === "read_mcp_resource") {
      est.mcp += bodyTokens;
    } else {
      est.tools += bodyTokens;
    }
  } else if (event.type === "context/compacted") {
    est.summary = event.data.tokens?.output ?? estimateTokens(event.data.summary);
  }

  tokenEstCache.set(event, est);
  return est;
}

export function usage(events: Envelope[]): Usage {
  const spent: Tokens = { input: 0, cached: 0, output: 0 };
  let context = 0;
  const raw: UsageBreakdown = { prompt: 0, input: 0, output: 0, skill: 0, mcp: 0, tools: 0 };
  let instructionsTokens = 0;
  let skillsCatalogTokens = 0;
  const turnTokens = new Map<number, UsageBreakdown>();
  let currentTurn = 0;

  const add = (tokens: Tokens | undefined) => {
    if (!tokens) return;
    spent.input += tokens.input;
    spent.cached += tokens.cached;
    spent.output += tokens.output;
  };

  const recordTurnDelta = (turn: number, est: CachedTokenEst) => {
    let bucket = turnTokens.get(turn);
    if (!bucket) {
      bucket = { prompt: 0, input: 0, output: 0, skill: 0, mcp: 0, tools: 0 };
      turnTokens.set(turn, bucket);
    }
    bucket.input += est.input;
    bucket.output += est.output;
    bucket.skill += est.skill;
    bucket.mcp += est.mcp;
    bucket.tools += est.tools;
    raw.input += est.input;
    raw.output += est.output;
    raw.skill += est.skill;
    raw.mcp += est.mcp;
    raw.tools += est.tools;
  };

  // Locate the last compaction event in reverse so discarded pre-compaction turns skip character-level token scanning,
  // while still estimating retained turns (`turn >= kept_from_turn`).
  let lastCompactedIdx = -1;
  let lastKeptFromTurn: number | undefined;
  for (let i = events.length - 1; i >= 0; i--) {
    const ev = events[i];
    if (ev.type === "context/compacted") {
      lastCompactedIdx = i;
      lastKeptFromTurn = ev.data.kept_from_turn;
      break;
    }
  }

  for (let i = 0; i < events.length; i++) {
    const event = events[i];
    if (event.type === "turn/start") {
      currentTurn = event.data.turn;
      continue;
    }

    const skipPreCompaction =
      i < lastCompactedIdx && (lastKeptFromTurn === undefined || currentTurn < lastKeptFromTurn);

    if (skipPreCompaction) {
      if (event.type === "instructions/loaded") {
        instructionsTokens = estimateEnvelopeTokens(event).instructions ?? 0;
      } else if (event.type === "skills/listed") {
        skillsCatalogTokens = estimateEnvelopeTokens(event).skillsCatalog ?? 0;
      } else if (event.type === "assistant/message" || event.type === "guardian/reviewed" || event.type === "context/compacted") {
        add(event.data.tokens);
      } else if (event.type === "tool/result") {
        add(event.data.meta?.tokens);
      }
      continue;
    }

    if (event.type === "instructions/loaded") {
      instructionsTokens = estimateEnvelopeTokens(event).instructions ?? 0;
    } else if (event.type === "skills/listed") {
      skillsCatalogTokens = estimateEnvelopeTokens(event).skillsCatalog ?? 0;
    } else if (event.type === "user/message") {
      recordTurnDelta(currentTurn, estimateEnvelopeTokens(event));
    } else if (event.type === "assistant/message") {
      add(event.data.tokens);
      if (event.data.tokens) context = event.data.tokens.input + event.data.tokens.output;
      recordTurnDelta(currentTurn, estimateEnvelopeTokens(event));
    } else if (event.type === "guardian/reviewed") {
      add(event.data.tokens);
    } else if (event.type === "tool/result") {
      add(event.data.meta?.tokens);
      recordTurnDelta(currentTurn, estimateEnvelopeTokens(event));
    } else if (event.type === "context/compacted") {
      add(event.data.tokens);
      const est = estimateEnvelopeTokens(event);
      const summaryOut = est.summary ?? 0;
      const keptFrom = event.data.kept_from_turn;
      raw.input = 0;
      raw.output = summaryOut;
      raw.skill = 0;
      raw.mcp = 0;
      raw.tools = 0;

      if (keptFrom !== undefined) {
        for (const [turn, tb] of [...turnTokens.entries()]) {
          if (turn < keptFrom) {
            turnTokens.delete(turn);
          } else {
            raw.input += tb.input;
            raw.output += tb.output;
            raw.skill += tb.skill;
            raw.mcp += tb.mcp;
            raw.tools += tb.tools;
          }
        }
        const retainedEst =
          raw.input + (raw.output - summaryOut) + raw.skill + raw.mcp + raw.tools;
        context =
          (event.data.tokens?.output ?? summaryOut) +
          instructionsTokens +
          skillsCatalogTokens +
          retainedEst;
      } else {
        turnTokens.clear();
        context = event.data.tokens?.output ?? 0;
      }
    }
  }

  raw.prompt = instructionsTokens;
  raw.skill += skillsCatalogTokens;

  const nonPrompt = raw.input + raw.output + raw.skill + raw.mcp + raw.tools;
  const totalEst = raw.prompt + nonPrompt;
  let breakdown: UsageBreakdown = raw;
  if (context > 0) {
    if (totalEst <= context) {
      breakdown = { ...raw, prompt: raw.prompt + (context - totalEst) };
    } else if (totalEst > 0) {
      const scale = context / totalEst;
      breakdown = {
        prompt: Math.round(raw.prompt * scale),
        input: Math.round(raw.input * scale),
        output: Math.round(raw.output * scale),
        skill: Math.round(raw.skill * scale),
        mcp: Math.round(raw.mcp * scale),
        tools: Math.round(raw.tools * scale),
      };
    }
  }
  return { context, spent, breakdown };
}

/** Sum the estimated USD cost across every priced request in `events` (preserving exact per-turn model rates). */
export function sessionCostUsd(events: Envelope[], fallbackProvider = "anthropic", fallbackModel = ""): number | null {
  let provider = fallbackProvider;
  let model = fallbackModel;
  let total = 0;
  let anyPriced = false;
  for (const event of events) {
    if (event.type === "session/created") {
      provider = event.data.provider ?? "anthropic";
      model = event.data.model;
    } else if (event.type === "model/changed") {
      if (event.data.provider) provider = event.data.provider;
      model = event.data.model;
    } else if (event.type === "assistant/message" && event.data.tokens) {
      const c = estimateTokensUsd(provider, event.data.model || model, event.data.tokens);
      if (c !== null) {
        total += c;
        anyPriced = true;
      }
    } else if (event.type === "guardian/reviewed" && event.data.tokens) {
      const c = estimateTokensUsd(provider, event.data.model || model, event.data.tokens);
      if (c !== null) {
        total += c;
        anyPriced = true;
      }
    } else if (event.type === "context/compacted" && event.data.tokens) {
      const c = estimateTokensUsd(provider, model, event.data.tokens);
      if (c !== null) {
        total += c;
        anyPriced = true;
      }
    }
  }
  return anyPriced ? total : null;
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

/** What a document attached is, as its chip says it: `3 页`, `Word 文档`, `2 个工作表`, `12 张幻灯片` (or localized). */
export function documentLabel(document: DocumentKind, parts?: number, locale: Locale = getCurrentLocale()): string {
  switch (document) {
    case "pdf":
      return formatMessage(locale, "doc.pdfPages", { count: parts ?? 0 });
    case "word":
      return formatMessage(locale, "doc.word");
    case "excel":
      return formatMessage(locale, "doc.excelSheets", { count: parts ?? 0 });
    case "powerpoint":
      return formatMessage(locale, "doc.pptSlides", { count: parts ?? 0 });
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
      const left = files.get(meta.path);
      if (left) Object.assign(left, { gone: true, file: undefined });
      const arrived = entry(meta.to);
      Object.assign(arrived, { movedFrom: left?.movedFrom ?? meta.path, gone: undefined, file: meta.file ?? arrived.file });
      files.set(meta.to, arrived);
      continue;
    }
    const file = entry(meta.path);
    if (!files.has(meta.path)) file.created = !!meta.created;
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
      if (last.how !== "backup") last.how = bestHow(last.how, how);
      if (last.bytes === undefined && bytes !== undefined) last.bytes = bytes;
      if (!last.ts && ts) last.ts = ts;
      existing.currentSnapshot = snapshot;
    } else if (!snapshot && !last?.snapshot) {
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
  item.kind === "tool" &&
  editsFiles(item.name) &&
  !!item.result &&
  !item.result.isError &&
  !!(item.result.meta?.after || item.result.meta?.changes?.length) &&
  !item.undone;

/**
 * The turns from which undoing would restore something: each turn that has
 * an edit left to undo, or comes before one that has. Undoing from a turn
 * takes every later turn's edits with it.
 */
export function undoableTurns(rows: Row[], copied = 0): Set<number> {
  const own = rows.filter((row) => row.turn > copied);
  const last = Math.max(0, ...own.filter((row) => row.items.some(undoable)).map((row) => row.turn));
  return new Set(own.map((row) => row.turn).filter((turn) => turn > 0 && turn <= last));
}

/** Extract normalized todos from either native `update_todos` metadata or external CLI `update_steps` / `TodoWrite` inputs. */
export function todosOf(item: ToolItem): Todo[] | null {
  if (item.result?.meta?.todos) return item.result.meta.todos;
  if (item.name !== "update_todos" && item.name !== "update_steps" && item.name !== "TodoWrite") return null;
  const raw = Array.isArray(item.input.todos)
    ? item.input.todos
    : Array.isArray(item.input.steps)
      ? item.input.steps
      : null;
  if (!raw) return null;
  const parsed: Todo[] = [];
  for (const entry of raw) {
    if (!entry || typeof entry !== "object") continue;
    const rec = entry as Record<string, unknown>;
    const text = [rec.text, rec.title, rec.content].find((v): v is string => typeof v === "string" && v.trim().length > 0)?.trim();
    if (!text) continue;
    const rawStatus = typeof rec.status === "string" ? rec.status : "pending";
    const status: Todo["status"] =
      rawStatus === "done" || rawStatus === "completed" || rawStatus === "skipped"
        ? "done"
        : rawStatus === "in_progress" || rawStatus === "active"
          ? "in_progress"
          : "pending";
    parsed.push({ text, status });
  }
  return parsed;
}

/** The to-do list as the agent last wrote it; empty when it never wrote one. */
export function todos(rows: Row[]): Todo[] {
  const written = rows
    .flatMap((row) => row.items)
    .filter((item): item is ToolItem => item.kind === "tool" && !!item.result && !item.result.isError && todosOf(item) !== null);
  const last = written[written.length - 1];
  return last ? (todosOf(last) ?? []) : [];
}

/** Commands that ran, in order, for the terminal panel. */
export function commandRuns(rows: Row[]): ToolItem[] {
  const shows = (name: string) =>
    name === "run_command" || name === "command_output" || name === "command_input" || name === "stop_command";
  return rows
    .flatMap((row) => row.items)
    .filter((item): item is ToolItem => item.kind === "tool" && shows(item.name) && !!item.result?.meta?.command);
}

/** Whether an `ask_user` call is waiting on 本尊's answers: asked, and neither answered nor ended. */
export const asking = (tool: ToolItem) => !!tool.question && !tool.question.answered && !tool.result;

/** The approval the session is waiting on, if any. */
export function pendingApproval(rows: Row[]): ToolItem | undefined {
  return rows
    .flatMap((row) => row.items)
    .find((item): item is ToolItem => item.kind === "tool" && !!item.approval && !item.approval.decision && !item.result);
}

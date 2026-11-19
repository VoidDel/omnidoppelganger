// Shapes shared with the Rust side. Field names match the JSON the commands and events carry.

export type Decision = "allow_once" | "allow_session" | "allow_always" | "deny";
export type Status = "idle" | "running" | "waiting_approval";
/** What runs without asking: every edit asks, edits run, nothing is edited, or safe commands run too. */
export type Mode = "manual" | "edit" | "plan" | "auto";

/** A picture attached to a message: its format and its bytes in base64. */
export interface Image {
  media_type: string;
  data: string;
}

/** What one request cost. `input` is the whole prompt, the cached part included. */
export interface Tokens {
  input: number;
  cached: number;
  output: number;
}

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "tool_use"; id: string; name: string; input: Record<string, unknown> }
  | { type: "fallback"; from: { model: string }; to: { model: string } };

export type SessionEvent =
  | { type: "session/created"; data: { id: string; workspace: string; provider?: string; model: string; format: number; parent?: string; schedule?: string; fork?: { session: string; turn: number } } }
  | { type: "subagent/started"; data: { tool_use_id: string; session: string; name: string } }
  | { type: "session/renamed"; data: { title: string } }
  | { type: "model/changed"; data: { model: string } }
  | { type: "instructions/loaded"; data: { path: string; content: string } }
  | { type: "skills/listed"; data: { skills: Skill[] } }
  | { type: "turn/start"; data: { turn: number } }
  | { type: "user/message"; data: { text: string; steering: boolean; mode?: Mode; images?: Image[] } }
  | { type: "assistant/message"; data: { content: ContentBlock[]; stop_reason: string | null; model: string; tokens?: Tokens } }
  | { type: "context/compacted"; data: { summary: string; tokens_before: number; tokens?: Tokens } }
  | { type: "approval/requested"; data: { tool_use_id: string; tool: string; reason: string; detail: string; grant?: string } }
  | { type: "approval/resolved"; data: { tool_use_id: string; decision: Decision } }
  | { type: "tool/result"; data: { tool_use_id: string; tool: string; content: string; is_error: boolean; meta: ToolMeta | null } }
  | { type: "files/restored"; data: { turn: number; restored: string[]; skipped: string[] } }
  | { type: "turn/end"; data: { turn: number; reason: string; error: string | null } };

export type Envelope = { seq: number; ts: string } & SessionEvent;

export interface DiffHunk {
  old_start: number;
  new_start: number;
  lines: { kind: " " | "-" | "+"; text: string }[];
}

/** Presentation data a tool attaches to its result. Each tool fills its own fields. */
export interface ToolMeta {
  path?: string;
  lines?: number;
  shown?: number;
  pattern?: string;
  glob?: string;
  /** How many files or directory entries `find_files` returned. */
  entries?: number;
  matches?: number;
  files?: number;
  truncated?: boolean;
  created?: boolean;
  added?: number;
  removed?: number;
  hunk?: DiffHunk | null;
  command?: string;
  exit_code?: number | null;
  stopped?: string | null;
  duration_ms?: number;
  output?: string;
  /** What `web_search` looked for, and how many results came back. */
  query?: string;
  results?: number;
  /** The address `fetch_url` ended at, its title, and how many characters of text it holds. */
  url?: string;
  title?: string | null;
  characters?: number;
  /** The skill `load_skill` read. */
  skill?: string;
  /** The call went to a tool of an MCP server. */
  mcp?: boolean;
  /** The to-do list `update_todos` recorded. */
  todos?: Todo[];
  /** The id of a background command, like `b1`. */
  job?: string;
  /** `run_command` left the command running and returned. */
  background?: boolean;
  /** A background command is still running. */
  running?: boolean;
  /** The mode refused the call without asking: an edit in plan mode. */
  refused?: boolean;
  /** Set on an edit that can be undone: the fingerprint of what it wrote. */
  after?: string;
  /** The subagents a `delegate` call set off, and why each one's turn ended; `session` is null for one that could not start. */
  agents?: { session: string | null; name: string; reason: string }[];
}

/** A skill as the 分身 is told of it: instructions it can load when a task calls for them. */
export interface Skill {
  name: string;
  description: string;
}

/** One item of the 分身's to-do list. */
export interface Todo {
  text: string;
  status: "pending" | "in_progress" | "done";
}

/** The Git worktree made for a session that works apart from the folder's own files. */
export interface Worktree {
  /** The folder the user chose; the session is listed under it. */
  origin: string;
  branch: string;
  checkout: string;
}

export interface SessionItem {
  id: string;
  title: string;
  /** Set when the session works in a worktree of its own; its `workspace` is then inside that. */
  worktree: Worktree | null;
  /** Id of the session whose 分身 set this one off as a 子分身; null for one the user started. */
  parent: string | null;
  /** Name of the 功课 (schedule) that started the session and gave it its first message; null otherwise. */
  schedule: string | null;
  /** Set for a fork: the session its conversation was copied from, and the last turn that was copied (0 for none). */
  fork: { session: string; turn: number } | null;
  workspace: string;
  /** Id of the model provider the session runs on. */
  provider: string;
  model: string;
  mode: Mode;
  /** The number of tokens at which the conversation is compacted into a summary. */
  context_limit: number;
  /** The most that limit can be for this session's model. */
  context_ceiling: number;
  /** The Git branch checked out in the workspace; null when it is not a repository. */
  branch: string | null;
  /** Whether the model takes pictures; null when its provider's documentation does not say. */
  vision: boolean | null;
  created_at: string;
  updated_at: string;
  turn_open: boolean;
  awaiting_approval: boolean;
  status: Status;
}

export type SessionView = SessionItem & { events: Envelope[] };

export type StreamDelta =
  | { type: "text_delta"; index: number; text: string }
  | { type: "thinking_delta"; index: number; text: string }
  | { type: "tool_use_start"; index: number; id: string; name: string }
  | { type: "tool_input_delta"; index: number; partial: string }
  | { type: "block_stop"; index: number }
  | { type: "reset" };

/** When a 功课 is due. Days of the week run from 1, Monday, to 7, Sunday; none means every day. */
export type When = { kind: "once"; at: string } | { kind: "daily"; time: string; days: number[] } | { kind: "every"; minutes: number };

/** A 功课: a task that starts by itself, in a session made for it each time. */
export interface Schedule {
  /** Empty in one that is new; the backend gives it. */
  id: string;
  name: string;
  workspace: string;
  prompt: string;
  when: When;
  mode: Mode;
  /** Whether each session gets a Git worktree of its own. */
  isolated: boolean;
  enabled: boolean;
  /** RFC 3339 time it is next due; absent when it is off or has no time left. */
  next?: string;
  /** How its last start went: the session it started, or why it started none. */
  last?: { at: string; session?: string; error?: string };
}

export type UiEvent =
  | { kind: "log"; session_id: string; envelope: Envelope }
  | { kind: "stream"; session_id: string; delta: StreamDelta }
  | { kind: "status"; session_id: string; status: Status };

export interface Settings {
  /** Provider and model of sessions created from now on. */
  provider: string;
  model: string;
  effort: string;
  /** The mode new sessions start in. */
  mode: Mode;
  /** Conversations are compacted once they hold about this many tokens. */
  context_limit: number;
  /** Which search engine `web_search` asks. */
  search_engine: string;
  theme: "system" | "light" | "dark";
  /** Addresses that replace a provider's default, by provider id. */
  base_urls: Record<string, string>;
}

export interface KeyStatus {
  source: "saved" | "env" | "none";
  preview: string | null;
  /** The saved key is in a plain-text file, not in the system's credential manager. */
  plaintext: boolean;
}

/** A model provider as the core describes it. Pairs are `[id, display name]` and `[address, what it is]`. */
export interface Provider {
  id: string;
  name: string;
  protocol: "anthropic" | "responses" | { chat: string };
  base_url: string;
  other_urls: [string, string][];
  key_env: string | null;
  models: [string, string][];
}

export interface SettingsView {
  settings: Settings;
  keys: Record<string, KeyStatus>;
  providers: Provider[];
  efforts: string[];
  home: string;
  /** Command approvals given for good, by workspace path. */
  grants: Record<string, string[]>;
  /** The engines a web search can ask, `[id, what it is]`, and whether a GLM key is saved for searching to work. */
  search_engines: [string, string][];
  search_ready: boolean;
  /** The user's own skills, and the directory they are kept in. */
  skills: Skill[];
  skills_directory: string;
  /** The text of the MCP server list, and how each server it names stands. */
  mcp_config: string;
  mcp_servers: McpServer[];
}

/** An MCP server from the user's list and whether it is connected. */
export interface McpServer {
  name: string;
  state: "connecting" | "ready" | "failed";
  error: string | null;
  /** The names of the tools it offers. */
  tools: string[];
}

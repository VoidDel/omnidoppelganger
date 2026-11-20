// Shapes shared with the Rust side. Field names match the JSON the commands and events carry.

export type Decision = "allow_once" | "allow_session" | "allow_always" | "deny";
export type Status = "idle" | "running" | "waiting_approval";
/** What runs without asking: every edit asks, edits run, nothing is edited, or safe commands run too. */
export type Mode = "manual" | "edit" | "plan" | "auto" | "bypass";

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

/** One question the 分身 puts to 本尊 with `ask_user`: answered by one option, any of them, or in words. */
export interface Question {
  /** A few words naming it, on its tab. */
  header: string;
  question: string;
  kind: "single" | "multiple" | "text";
  options?: { label: string; description?: string }[];
}

/** 本尊's answer to one question: the labels chosen, and anything written. Both empty when left unanswered. */
export interface Reply {
  chosen?: string[];
  text?: string;
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
  | { type: "model/changed"; data: { model: string; provider?: string } }
  | { type: "effort/changed"; data: { effort: string } }
  | { type: "instructions/loaded"; data: { path: string; content: string } }
  | { type: "skills/listed"; data: { skills: Skill[] } }
  | { type: "turn/start"; data: { turn: number } }
  | { type: "user/message"; data: { text: string; steering: boolean; mode?: Mode; images?: Image[]; files?: Attached[] } }
  | { type: "assistant/message"; data: { content: ContentBlock[]; stop_reason: string | null; model: string; tokens?: Tokens } }
  | { type: "context/compacted"; data: { summary: string; tokens_before: number; tokens?: Tokens } }
  | { type: "approval/requested"; data: { tool_use_id: string; tool: string; reason: string; detail: string; grant?: string; expires_at?: string } }
  | { type: "approval/resolved"; data: { tool_use_id: string; decision: Decision; timed_out?: boolean } }
  | { type: "question/asked"; data: { tool_use_id: string; questions: Question[]; expires_at?: string } }
  | { type: "question/answered"; data: { tool_use_id: string; answers?: Reply[]; declined?: boolean; timed_out?: boolean } }
  | { type: "guardian/reviewed"; data: { tool_use_id: string; model: string; allowed: boolean; reason: string; verdict?: string; failed?: boolean; tokens?: Tokens } }
  | { type: "tool/result"; data: { tool_use_id: string; tool: string; content: string; is_error: boolean; meta: ToolMeta | null } }
  | { type: "files/restored"; data: { turn: number; restored: string[]; skipped: string[] } }
  | { type: "files/made"; data: { turn: number; files: string[] } }
  | { type: "turn/end"; data: { turn: number; reason: string; error: string | null } };

export type Envelope = { seq: number; ts: string } & SessionEvent;

/** A file the user attached to a message, by where it is on disk, with its text when that went with the message. */
export interface Attached {
  path: string;
  bytes: number;
  content?: string;
  /** For a document, what kind it is; its `content` is then the text taken out of it. */
  document?: DocumentKind;
  /** For a document, how many pages, sheets, or slides it has; none for Word. */
  parts?: number;
}

/** The kinds of document whose text is taken out to be read: see `crates/odg-core/src/documents.rs`. */
export type DocumentKind = "pdf" | "word" | "excel" | "powerpoint";

/**
 * What a path the user picked to attach turned out to be: a file (`inline` when its text will go with the
 * message; otherwise the 分身 reads it in parts; `document` and `parts` for a document), a picture to send as
 * one, or neither, with why.
 */
export type Picked =
  | { kind: "file"; path: string; name: string; bytes: number; inline: boolean; document?: DocumentKind; parts?: number }
  | { kind: "image"; name: string; image: Image }
  | {
      kind: "refused";
      name: string;
      why: "missing" | "directory" | "binary" | "too_large" | "image_too_large" | "doc_unreadable" | "doc_locked" | "doc_no_text" | "doc_old_format";
    };

export interface DiffHunk {
  old_start: number;
  new_start: number;
  lines: { kind: " " | "-" | "+"; text: string }[];
}

/** Presentation data a tool attaches to its result. Each tool fills its own fields. */
/** How the document runtime stands, as the settings show it. */
export interface RuntimeView {
  /** `stale`: its Python is there and some libraries are not, which are installed by themselves. */
  status: { state: "ready"; python: string; version: string } | { state: "stale"; python: string; version: string } | { state: "missing"; outdated: boolean };
  python_version: string;
  download_mb: number;
  libreoffice: string | null;
  supported: boolean;
}

/** A step of setting up the document runtime. */
export type RuntimeProgress =
  | { step: "downloading"; done: number; total: number | null }
  | { step: "unpacking" }
  | { step: "installing" }
  | { step: "checking" }
  | { step: "done" };

export interface ToolMeta {
  path?: string;
  /** The absolute path of the file an edit wrote. */
  file?: string;
  lines?: number;
  shown?: number;
  pattern?: string;
  glob?: string;
  /** How many files or directory entries `find_files` returned. */
  entries?: number;
  matches?: number;
  /** How many files `search` found in; or, for `deliver_files`, the absolute paths of the files handed over. */
  files?: number | string[];
  truncated?: boolean;
  /** For `document_runtime`: its interpreter, and LibreOffice's command if there is one; `state` says whether it is set up. */
  python?: string;
  libreoffice?: string | null;
  /** For `generate_image`: who painted it, in what size, from what prompt. */
  painter?: string;
  size?: string;
  prompt?: string;
  created?: boolean;
  added?: number;
  removed?: number;
  hunk?: DiffHunk | null;
  command?: string;
  exit_code?: number | null;
  /** Whether the command ran in the sandbox: able to write only under the workspace and the temporary directories. */
  sandboxed?: boolean;
  stopped?: string | null;
  duration_ms?: number;
  output?: string;
  /** What `web_search` looked for, and how many results came back; with no GLM key, the provider that searched
   *  (`engine`), its model, and what the search's own request cost. */
  query?: string;
  results?: number;
  engine?: string;
  model?: string;
  tokens?: Tokens;
  /** For `code_nav`: what was asked, and of a file's diagnostics how many are errors; `server` is the language server that answered. */
  operation?: string;
  errors?: number;
  /** The address `fetch_url` ended at, its title, and how many characters of text it holds. */
  url?: string;
  title?: string | null;
  characters?: number;
  /** The skill `load_skill` read, or `install_skill` installed: where to, and for whom. */
  skill?: string;
  directory?: string;
  scope?: "project" | "user";
  /** The MCP server `add_mcp_server` added, how it stands, and the tools it offers; for `document_runtime`, whether it is set up. */
  server?: string;
  /** …; for `language_server`, whether the server is on the machine (`system`), downloaded for the app, or `missing`. */
  state?: "ready" | "failed" | "connecting" | "missing" | "system" | "downloaded";
  tools?: string[];
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
  /** For `delete_file` and `move_file`, when they can be undone: each file they changed, as undoing needs it. */
  changes?: { file: string; path: string; before: string | null; after: string | null }[];
  /** `delete_file` took the file away; `move_file` moved it, to `to`. */
  deleted?: boolean;
  moved?: boolean;
  to?: string;
  /** For a picture `read_file` showed the model: its type, its size in bytes, and its width and height when known. */
  picture?: string;
  bytes?: number;
  width?: number | null;
  height?: number | null;
  /** The subagents a `delegate` call set off, and why each one's turn ended; `session` is null for one that could not start. */
  agents?: { session: string | null; name: string; reason: string }[];
}

/** A language's server for `code_nav`, as the settings page lists it. */
export interface LanguageServerView {
  key: string;
  name: string;
  program: string;
  /** On the `PATH`, downloaded for the app, or nowhere. */
  state: "system" | "downloaded" | "missing";
  location: string | null;
  /** The version the app downloads and about how many megabytes; null where it cannot download it. */
  version: string | null;
  download_mb: number | null;
  /** How the user installs it themselves. */
  install: string;
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
  /** Id of the model provider the session runs on now, and the model. */
  provider: string;
  model: string;
  /** How hard the model works for this session, when set for it; null while it follows the settings' default. */
  effort: string | null;
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
  /** The image model `generate_image` asks; empty for the first whose provider has a key. */
  painter: string;
  theme: "system" | "light" | "dark";
  /** Addresses that replace a provider's default, by provider id. */
  base_urls: Record<string, string>;
  /** The names the user gave providers, by provider id; every provider added beside the first of its kind is here. */
  provider_names: Record<string, string>;
  /** Whether the commands a 分身 runs are confined to writing under its workspace, where the system can do that. */
  sandbox: boolean;
  /** Directories confined commands may also write under. */
  sandbox_paths: string[];
  /** Context windows the user set by hand, in tokens, by `<provider>/<model>`. */
  context_windows: Record<string, number>;
  /** Compact at four fifths of each model's own context window, whatever `context_limit` says. */
  context_auto: boolean;
  /** In 放手施为, have the 护法, a model, judge the commands the rules cannot pass before 本尊 is asked. */
  guardian: boolean;
  /** The model the 护法 is, by provider id; a provider not listed uses the session's own model. */
  guardian_models: Record<string, string>;
  /** In 放手施为, the seconds a request waits before it is declined by itself; 0 waits for good. */
  approval_timeout: number;
}

export interface KeyStatus {
  source: "saved" | "env" | "none";
  preview: string | null;
  /** The saved key is in a plain-text file, not in the system's credential manager. */
  plaintext: boolean;
}

/** A model provider as the core describes it. Pairs are `[id, display name]` and `[address, what it is]`. */
/** A kind of provider, which providers are added as. */
export interface ProviderKind {
  id: string;
  name: string;
  protocol: "anthropic" | "responses" | { chat: string };
  base_url: string;
  other_urls: [string, string][];
  key_env: string | null;
  models: [string, string][];
}

/** A provider to choose from: the first of a kind (its id the kind's), or another the user added (`<kind>-<n>`). */
export interface Provider {
  id: string;
  /** The id of its kind. */
  kind: string;
  /** The name the user gave it, or its kind's. */
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
  kinds: ProviderKind[];
  efforts: string[];
  home: string;
  /** Command approvals given for good, by workspace path. */
  grants: Record<string, string[]>;
  /** The engines a web search can ask, `[id, what it is]`, and whether a GLM key is saved for searching to work. */
  search_engines: [string, string][];
  search_ready: boolean;
  /** The image models there are, `[id, name, provider]`, and the one in force; null while no provider for one has a key. */
  painters: [string, string, string][];
  painter_ready: string | null;
  /**
   * The context window of the model new 分身 use, and how it is known: the provider said so when asked for its
   * models, its documentation said so when this build was made, or nobody did and a size small enough to be safe is assumed.
   */
  context_window: { tokens: number; source: "set" | "provider" | "documented" | "unknown" };
  /** What confines commands on this system (`Ok`), or why nothing can (`Err`). */
  sandbox_support: { Ok?: string; Err?: string };
  /** The user's own skills, and the directory they are kept in. */
  skills: Skill[];
  skills_directory: string;
  /** The text of the MCP server list, and how each server it names stands. */
  /** The folder kept for conversations (闲谈): a session there is a chat, not work on a project. */
  chat_folder: string;
  mcp_config: string;
  mcp_servers: McpServer[];
}

/** A skill (秘籍) as the 秘籍 page lists it: one that comes with the app, on or off, or one of the user's own. */
export interface ListedSkill {
  name: string;
  description: string;
  /** Where it is, or would be once switched on. */
  directory: string;
  builtin: boolean;
  /** In the user's skills directory, and so offered to every session. */
  installed: boolean;
  /** One of the app's the user has changed: switching it off would lose that. */
  edited: boolean;
}

export interface SkillsView {
  skills: ListedSkill[];
  /** The user's skills directory. */
  folder: string;
}

/** An MCP server from the user's list and whether it is connected. */
export interface McpServer {
  name: string;
  state: "connecting" | "ready" | "failed";
  error: string | null;
  /** The names of the tools it offers. */
  tools: string[];
}

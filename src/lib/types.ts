// Shapes shared with the Rust side. Field names match the JSON the commands and events carry.

export type Decision = "allow_once" | "allow_session" | "deny";
export type Status = "idle" | "running" | "waiting_approval";

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string }
  | { type: "tool_use"; id: string; name: string; input: Record<string, unknown> }
  | { type: "fallback"; from: { model: string }; to: { model: string } };

export type SessionEvent =
  | { type: "session/created"; data: { id: string; workspace: string; model: string; format: number } }
  | { type: "turn/start"; data: { turn: number } }
  | { type: "user/message"; data: { text: string; steering: boolean } }
  | { type: "assistant/message"; data: { content: ContentBlock[]; stop_reason: string | null; model: string } }
  | { type: "approval/requested"; data: { tool_use_id: string; tool: string; reason: string; detail: string } }
  | { type: "approval/resolved"; data: { tool_use_id: string; decision: Decision } }
  | { type: "tool/result"; data: { tool_use_id: string; tool: string; content: string; is_error: boolean; meta: ToolMeta | null } }
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
}

export interface SessionItem {
  id: string;
  title: string;
  workspace: string;
  model: string;
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

export type UiEvent =
  | { kind: "log"; session_id: string; envelope: Envelope }
  | { kind: "stream"; session_id: string; delta: StreamDelta }
  | { kind: "status"; session_id: string; status: Status };

export interface Settings {
  model: string;
  effort: string;
  theme: "system" | "light" | "dark";
}

export interface SettingsView {
  settings: Settings;
  key: { source: "saved" | "env" | "none"; preview: string | null };
  models: { id: string; name: string }[];
  efforts: string[];
  home: string;
}

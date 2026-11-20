import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { Decision, Image, Mode, Picked, Schedule, SessionItem, SessionView, Settings, SettingsView, UiEvent } from "./types";

export const api = {
  listSessions: () => invoke<SessionItem[]>("list_sessions"),
  /** Start a session in a folder; with `isolated`, in a Git worktree made for it, apart from the folder's own files. */
  createSession: (workspace: string, isolated = false) => invoke<SessionView>("create_session", { workspace, isolated }),
  /** Commit what a worktree session's checkout holds and merge it into the folder's branch. Rejects on a conflict, changing nothing. */
  landSession: (id: string) => invoke<{ commits: number; into: string }>("land_session", { id }),
  /**
   * Start a session that goes on from a point in another's conversation: its log begins as a copy of that one's up to
   * where turn `before` starts, or of all of it without `before`. The files are not copied: it works on the same ones.
   */
  forkSession: (id: string, before?: number) => invoke<SessionView>("fork_session", { id, before: before ?? null }),
  /** The 功课: tasks set to start by themselves. Each change resolves with the list after it. */
  listSchedules: () => invoke<Schedule[]>("list_schedules"),
  /** Add a schedule, or change the one with its id. Rejects when it could never run. */
  saveSchedule: (schedule: Schedule) => invoke<Schedule[]>("save_schedule", { schedule }),
  deleteSchedule: (id: string) => invoke<Schedule[]>("delete_schedule", { id }),
  /** Start a schedule's task now. How the start went is in the list: the schedule's `last`. */
  runSchedule: (id: string) => invoke<Schedule[]>("run_schedule", { id }),
  /** Called when a schedule came due and the list changed by itself. */
  onSchedulesChanged: (handler: () => void) => listen("schedules-changed", handler),
  openSession: (id: string) => invoke<SessionView>("open_session", { id }),
  renameSession: (id: string, title: string) => invoke<SessionItem>("rename_session", { id, title }),
  /** Remove the session and its file snapshots for good. Rejects while its agent is working. */
  deleteSession: (id: string) => invoke<void>("delete_session", { id }),
  /** Ids of the sessions whose title, folder, or conversation holds the text. */
  searchSessions: (query: string) => invoke<string[]>("search_sessions", { query }),
  /** Resolves true when the message was queued behind a running turn. */
  /** `files` are paths of files to attach: each is read as it is now, and goes with the message. */
  sendMessage: (id: string, text: string, images: Image[] = [], files: string[] = []) => invoke<boolean>("send_message", { id, text, images, files }),
  /** What each of these paths is, to attach: a file, a picture, or neither and why. Nothing is sent. */
  attachFiles: (paths: string[]) => invoke<Picked[]>("attach_files", { paths }),
  /** Ask for files with the system dialog. Resolves with none when the user closes it. */
  pickFiles: async (): Promise<string[]> => {
    const picked = await open({ multiple: true, title: "选要附给分身的文件" });
    return Array.isArray(picked) ? picked : typeof picked === "string" ? [picked] : [];
  },
  /** Told as files are dragged over the window (`over` true), leave it, or are dropped on it (with their paths). */
  onFilesDragged: (handler: (drag: { over: boolean; dropped: string[] }) => void) =>
    getCurrentWebview().onDragDropEvent((event) => {
      const { payload } = event;
      handler({ over: payload.type === "enter" || payload.type === "over", dropped: payload.type === "drop" ? payload.paths : [] });
    }),
  resolveApproval: (id: string, toolUseId: string, decision: Decision) =>
    invoke<boolean>("resolve_approval", { id, toolUseId, decision }),
  setMode: (id: string, mode: Mode) => invoke<SessionItem>("set_mode", { id, mode }),
  /** Move the session to another model of its provider. Rejects while its agent is working. */
  setModel: (id: string, provider: string, model: string) => invoke<SessionItem>("set_model", { id, provider, model }),
  setEffort: (id: string, effort: string) => invoke<SessionItem>("set_effort", { id, effort }),
  startChat: () => invoke<SessionView>("start_chat"),
  /** The models a provider serves with the saved key, `[id, display name]`, asked from the provider itself. */
  listModels: (provider: string) => invoke<[string, string][]>("list_models", { provider }),
  /** Undo the agent's file edits from `turn` on. Rejects while a turn is running. */
  undoTurn: (id: string, turn: number) => invoke<{ restored: string[]; skipped: string[] }>("undo_turn", { id, turn }),
  cancelTurn: (id: string) => invoke<void>("cancel_turn", { id }),
  /** Have the model summarize the conversation now, between turns. Resolves with how many tokens it held before. */
  compactSession: (id: string) => invoke<number>("compact_session", { id }),
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (settings: Settings) => invoke<SettingsView>("save_settings", { settings }),
  /** Check and save the MCP server list, then connect to what it names. Rejects when the text is not a valid list. */
  saveMcpConfig: (config: string) => invoke<SettingsView>("save_mcp_config", { config }),
  /** Take back a command approval that was given for good. */
  revokeGrant: (workspace: string, grant: string) => invoke<SettingsView>("revoke_grant", { workspace, grant }),
  saveApiKey: (provider: string, key: string) => invoke<SettingsView>("save_api_key", { provider, key }),
  deleteApiKey: (provider: string) => invoke<SettingsView>("delete_api_key", { provider }),
  /** Ask for a folder with the system dialog. Resolves null when the user closes it. */
  pickWorkspace: async (): Promise<string | null> => {
    // The end-to-end test names the folder up front: it cannot drive the system dialog.
    const preset = await invoke<string | null>("preset_workspace");
    if (preset) return preset;
    const picked = await open({ directory: true, multiple: false, title: "选一个文件夹作洞府" });
    return typeof picked === "string" ? picked : null;
  },
  /** Open an http(s) or mailto link in the system's default app, never in the app window. */
  openExternal: (url: string) => openUrl(url),
  onSessionEvent: (handler: (event: UiEvent) => void) => listen<UiEvent>("session-event", (event) => handler(event.payload)),
};

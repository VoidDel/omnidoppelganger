import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  Decision,
  DiffStage,
  FileView,
  FoundFile,
  GitBranch,
  GitCommitItem,
  GitCommitResult,
  GitDiffView,
  GitFile,
  GitStatus,
  Image,
  LanguageServerView,
  Mode,
  Picked,
  PtyExitPayload,
  PtyInfo,
  PtyOutputPayload,
  Reply,
  RestoredArtifact,
  RuntimeProgress,
  RuntimeView,
  Schedule,
  SessionItem,
  SessionView,
  Settings,
  SettingsView,
  SkillsView,
  TreeEntry,
  UiEvent,
} from "./types";

export const api = {
  listSessions: () => invoke<SessionItem[]>("list_sessions"),
  /** The side conversation (旁聊) beside a session, if it has one. */
  asideSession: (id: string) => invoke<SessionView | null>("aside_session", { id }),
  /** The side conversation beside a session, made now when it has none: read-only, briefed on the session before each message. */
  startAside: (id: string) => invoke<SessionView>("start_aside", { id }),
  /** Take away a session's side conversation, to start it over. Rejects while it is answering. */
  resetAside: (id: string) => invoke<void>("reset_aside", { id }),
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
  /** Answer the questions an `ask_user` call waits on, one reply each in their order; `null` leaves it to the 分身. */
  answerQuestions: (id: string, toolUseId: string, answers: Reply[] | null) => invoke<boolean>("answer_questions", { id, toolUseId, answers }),
  setMode: (id: string, mode: Mode) => invoke<SessionItem>("set_mode", { id, mode }),
  /** Move the session to another model of its provider. Rejects while its agent is working. */
  setModel: (id: string, provider: string, model: string) => invoke<SessionItem>("set_model", { id, provider, model }),
  setEffort: (id: string, effort: string) => invoke<SessionItem>("set_effort", { id, effort }),
  startChat: () => invoke<SessionView>("start_chat"),
  listSkills: () => invoke<SkillsView>("list_skills"),
  switchSkill: (name: string, on: boolean, force = false) => invoke<SkillsView>("switch_skill", { name, on, force }),
  removeSkill: (name: string) => invoke<SkillsView>("remove_skill", { name }),
  /** Put a skill in from a folder on this computer or a Git address; `path` is the folder in it that holds the skill. */
  installSkill: (source: string, path: string | null, replace = false) => invoke<SkillsView>("install_skill", { source, path, replace }),
  writeSkill: (name: string, description: string, body: string, replace = false) => invoke<SkillsView>("write_skill", { name, description, body, replace }),
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
  /** Put a provider of `kind` in the list under `name`; resolves with its id, which is new when the first of the kind is already set up. */
  addProvider: (kind: string, name: string) => invoke<{ id: string; view: SettingsView }>("add_provider", { kind, name }),
  /** Take a provider out of the list, with its key, address and name. */
  removeProvider: (id: string) => invoke<SettingsView>("remove_provider", { id }),
  /** Ask for a folder with the system dialog. Resolves null when the user closes it. */
  pickWorkspace: async (): Promise<string | null> => {
    // The end-to-end test names the folder up front: it cannot drive the system dialog.
    const preset = await invoke<string | null>("preset_workspace");
    if (preset) return preset;
    const picked = await open({ directory: true, multiple: false, title: "选一个文件夹作洞府" });
    return typeof picked === "string" ? picked : null;
  },
  /** Open a file attached to a message in the session with the system's program for it, or with `reveal` show it in its folder; with `snapshot`, opens that exact historical version. */
  openAttachment: (id: string, path: string, reveal: boolean, snapshot?: string, version?: number) =>
    invoke<string>("open_attachment", { id, path, reveal, snapshot: snapshot ?? null, version: version ?? null }),
  /** Restore a deliverable in the workspace back to a snapshotted version (backing up unrecorded live edits first). */
  restoreArtifact: (id: string, path: string, snapshot: string, version: number) => invoke<RestoredArtifact>("restore_artifact", { id, path, snapshot, version }),
  /** Save a historical deliverable version as a sibling copy in the workspace (e.g. `report.v1.docx`). */
  saveArtifactCopy: (id: string, path: string, snapshot: string, version: number) => invoke<string>("save_artifact_copy", { id, path, snapshot, version }),
  /** Workspace file explorer and read-only viewer commands, strictly confined to the session's workspace. */
  listWorkspaceDir: (id: string, path = "", showHidden = true, showIgnored = true) =>
    invoke<TreeEntry[]>("list_workspace_dir", { id, path, showHidden, showIgnored }),
  searchWorkspaceFiles: (id: string, query: string, limit = 100) =>
    invoke<FoundFile[]>("search_workspace_files", { id, query, limit }),
  readWorkspaceFile: (id: string, path: string) => invoke<FileView>("read_workspace_file", { id, path }),
  createWorkspaceEntry: (id: string, path: string, isDir: boolean) =>
    invoke<string>("create_workspace_entry", { id, path, isDir }),
  renameWorkspaceEntry: (id: string, from: string, to: string) =>
    invoke<string>("rename_workspace_entry", { id, from, to }),
  deleteWorkspaceEntry: (id: string, path: string) => invoke<void>("delete_workspace_entry", { id, path }),
  openWorkspaceEntry: (id: string, path: string, reveal: boolean) =>
    invoke<string>("open_workspace_entry", { id, path, reveal }),
  /** Git management commands for the session's workspace repository. */
  gitStatus: (id: string) => invoke<GitStatus>("git_status", { id }),
  gitDiff: (id: string, path: string, stage: DiffStage, commit?: string) =>
    invoke<GitDiffView>("git_diff", { id, path, stage, commit: commit ?? null }),
  gitStage: (id: string, paths: string[] = []) => invoke<GitStatus>("git_stage", { id, paths }),
  gitUnstage: (id: string, paths: string[] = []) => invoke<GitStatus>("git_unstage", { id, paths }),
  gitDiscard: (id: string, paths: string[]) => invoke<GitStatus>("git_discard", { id, paths }),
  gitCommit: (id: string, message: string, stageAll = false) =>
    invoke<GitCommitResult>("git_commit", { id, message, stageAll }),
  gitBranches: (id: string) => invoke<GitBranch[]>("git_branches", { id }),
  gitCheckout: (id: string, branch: string, create = false) =>
    invoke<SessionItem>("git_checkout", { id, branch, create }),
  gitLog: (id: string, limit = 30) => invoke<GitCommitItem[]>("git_log", { id, limit }),
  gitCommitFiles: (id: string, commit: string) => invoke<GitFile[]>("git_commit_files", { id, commit }),
  gitSync: (id: string, action: "fetch" | "pull" | "push") => invoke<string>("git_sync", { id, action }),
  /** Interactive side-panel PTY shell commands and event listeners. */
  ptySpawn: (id: string, cols = 80, rows = 24) => invoke<PtyInfo>("pty_spawn", { id, cols, rows }),
  ptyList: (id: string) => invoke<PtyInfo[]>("pty_list", { id }),
  ptyWrite: (ptyId: string, data: string) => invoke<void>("pty_write", { ptyId, data }),
  ptyResize: (ptyId: string, cols: number, rows: number) => invoke<void>("pty_resize", { ptyId, cols, rows }),
  ptyClear: (ptyId: string) => invoke<void>("pty_clear", { ptyId }),
  ptyClose: (ptyId: string) => invoke<void>("pty_close", { ptyId }),
  onPtyOutput: (handler: (payload: PtyOutputPayload) => void) =>
    listen<PtyOutputPayload>("pty-output", (event) => handler(event.payload)),
  onPtyExit: (handler: (payload: PtyExitPayload) => void) =>
    listen<PtyExitPayload>("pty-exit", (event) => handler(event.payload)),
  watchWorkspace: (id: string | null) => invoke<void>("watch_workspace", { id }),
  onWorkspaceChanged: (handler: (workspace: string) => void) =>
    listen<string>("workspace-changed", (event) => handler(event.payload)),
  /** A picture the session's 分身 wrote, either from a recorded `snapshot` or as it is now on disk. */
  picture: (id: string, path: string, snapshot?: string) => invoke<Image>("picture", { id, path, snapshot: snapshot ?? null }),
  /** Ask for a folder with the system dialog, titled `title`. Resolves null when the user closes it. */
  pickFolder: async (title: string): Promise<string | null> => {
    const picked = await open({ directory: true, multiple: false, title });
    return typeof picked === "string" ? picked : null;
  },
  /** Open an http(s) or mailto link in the system's default app, never in the app window. */
  openExternal: (url: string) => openUrl(url),
  onSessionEvent: (handler: (event: UiEvent) => void) => listen<UiEvent>("session-event", (event) => handler(event.payload)),
  /** The document runtime: how it stands, setting it up (how far it has got comes as events), and taking it away. */
  /** The language servers `code_nav` uses: where each is, downloading one the app can, and taking a download away. */
  languageServers: () => invoke<LanguageServerView[]>("language_servers"),
  installLanguageServer: (key: string) => invoke<LanguageServerView[]>("install_language_server", { key }),
  removeLanguageServer: (key: string) => invoke<LanguageServerView[]>("remove_language_server", { key }),
  runtimeStatus: () => invoke<RuntimeView>("runtime_status"),
  installRuntime: () => invoke<RuntimeView>("install_runtime"),
  removeRuntime: () => invoke<RuntimeView>("remove_runtime"),
  onRuntimeProgress: (handler: (step: RuntimeProgress) => void) => listen<RuntimeProgress>("runtime-progress", (event) => handler(event.payload)),
};

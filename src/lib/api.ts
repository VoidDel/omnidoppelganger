import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { Decision, Image, Mode, SessionItem, SessionView, Settings, SettingsView, UiEvent } from "./types";

export const api = {
  listSessions: () => invoke<SessionItem[]>("list_sessions"),
  createSession: (workspace: string) => invoke<SessionView>("create_session", { workspace }),
  openSession: (id: string) => invoke<SessionView>("open_session", { id }),
  renameSession: (id: string, title: string) => invoke<SessionItem>("rename_session", { id, title }),
  /** Remove the session and its file snapshots for good. Rejects while its agent is working. */
  deleteSession: (id: string) => invoke<void>("delete_session", { id }),
  /** Ids of the sessions whose title, folder, or conversation holds the text. */
  searchSessions: (query: string) => invoke<string[]>("search_sessions", { query }),
  /** Resolves true when the message was queued behind a running turn. */
  sendMessage: (id: string, text: string, images: Image[] = []) => invoke<boolean>("send_message", { id, text, images }),
  resolveApproval: (id: string, toolUseId: string, decision: Decision) =>
    invoke<boolean>("resolve_approval", { id, toolUseId, decision }),
  setMode: (id: string, mode: Mode) => invoke<SessionItem>("set_mode", { id, mode }),
  /** Undo the agent's file edits from `turn` on. Rejects while a turn is running. */
  undoTurn: (id: string, turn: number) => invoke<{ restored: string[]; skipped: string[] }>("undo_turn", { id, turn }),
  cancelTurn: (id: string) => invoke<void>("cancel_turn", { id }),
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (settings: Settings) => invoke<SettingsView>("save_settings", { settings }),
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

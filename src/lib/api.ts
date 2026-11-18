import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { Decision, SessionItem, SessionView, Settings, SettingsView, UiEvent } from "./types";

export const api = {
  listSessions: () => invoke<SessionItem[]>("list_sessions"),
  createSession: (workspace: string) => invoke<SessionView>("create_session", { workspace }),
  openSession: (id: string) => invoke<SessionView>("open_session", { id }),
  /** Resolves true when the message was queued behind a running turn. */
  sendMessage: (id: string, text: string) => invoke<boolean>("send_message", { id, text }),
  resolveApproval: (id: string, toolUseId: string, decision: Decision) =>
    invoke<boolean>("resolve_approval", { id, toolUseId, decision }),
  cancelTurn: (id: string) => invoke<void>("cancel_turn", { id }),
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (settings: Settings) => invoke<SettingsView>("save_settings", { settings }),
  saveApiKey: (key: string) => invoke<SettingsView>("save_api_key", { key }),
  deleteApiKey: () => invoke<SettingsView>("delete_api_key"),
  /** Ask for a folder with the system dialog. Resolves null when the user closes it. */
  pickWorkspace: async (): Promise<string | null> => {
    const picked = await open({ directory: true, multiple: false, title: "选一个工作区文件夹" });
    return typeof picked === "string" ? picked : null;
  },
  onSessionEvent: (handler: (event: UiEvent) => void) => listen<UiEvent>("session-event", (event) => handler(event.payload)),
};

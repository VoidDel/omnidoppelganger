import { api } from "./api";

/**
 * Send a desktop notification only when the app window is unfocused/hidden so background
 * turns (completion or approval waits) surface cleanly without interrupting active work.
 */
export function notifyBackground(title: string, body: string): void {
  if (typeof document !== "undefined" && !document.hidden && document.hasFocus()) {
    return;
  }
  api.notifyDesktop(title, body).catch(() => {
    if (typeof Notification !== "undefined" && Notification.permission === "granted") {
      try {
        new Notification(title, { body });
      } catch {
        // Ignore browser notification errors in unsupported environments.
      }
    }
  });
}

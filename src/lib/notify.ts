import { api } from "./api";

export interface NotifyOptions {
  sessionId?: string | null;
  page?: string | null;
  onActivate?: () => void;
}

/**
 * Send a desktop notification only when the app window is unfocused/hidden so background
 * turns (completion or approval waits) surface cleanly without interrupting active work.
 */
export function notifyBackground(title: string, body: string, options?: NotifyOptions): void {
  if (typeof document !== "undefined" && !document.hidden && document.hasFocus()) {
    return;
  }
  api.notifyDesktop(title, body, options?.sessionId, options?.page).catch(() => {
    if (typeof Notification === "undefined") return;
    const showWebNotification = () => {
      try {
        const n = new Notification(title, { body });
        n.onclick = () => {
          window.focus();
          options?.onActivate?.();
          n.close();
        };
      } catch {
        // Ignore browser notification errors in unsupported environments.
      }
    };
    if (Notification.permission === "granted") {
      showWebNotification();
    } else if (Notification.permission === "default") {
      void Notification.requestPermission().then((perm) => {
        if (perm === "granted") showWebNotification();
      });
    }
  });
}

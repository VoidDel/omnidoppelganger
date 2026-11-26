import { type Dispatch, type SetStateAction, useEffect, useRef, useState } from "react";
import { SIDEBAR_WIDTH, fitSidebar } from "../components/Sidebar";
import { PANEL_WIDTH, fitPanel } from "../components/SidePanel";
import type { Settings } from "../lib/types";

const PANEL_KEY = "odg.panel";
const SIDEBAR_KEY = "odg.sidebar";

export function useSidebar() {
  const [sidebar, setSidebar] = useState<{ width: number; hidden: boolean }>(() => {
    try {
      const kept = JSON.parse(localStorage.getItem(SIDEBAR_KEY) ?? "{}");
      return {
        width: typeof kept.width === "number" ? fitSidebar(kept.width) : SIDEBAR_WIDTH.initial,
        hidden: kept.hidden === true,
      };
    } catch {
      return { width: SIDEBAR_WIDTH.initial, hidden: false };
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_KEY, JSON.stringify(sidebar));
    } catch {
      // Ignore storage errors
    }
  }, [sidebar]);
  return [sidebar, setSidebar] as const;
}

export function useCompactLayout() {
  const [compact, setCompact] = useState(
    () => typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(max-width: 1100px)").matches,
  );
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const media = window.matchMedia("(max-width: 1100px)");
    const sync = () => setCompact(media.matches);
    sync();
    media.addEventListener("change", sync);
    return () => media.removeEventListener("change", sync);
  }, []);
  return compact;
}

export function usePanel() {
  const [panel, setPanel] = useState<{ width: number; hidden: boolean }>(() => {
    try {
      const kept = JSON.parse(localStorage.getItem(PANEL_KEY) ?? "{}");
      return {
        width: typeof kept.width === "number" ? fitPanel(kept.width) : PANEL_WIDTH.initial,
        hidden: kept.hidden === true,
      };
    } catch {
      return { width: PANEL_WIDTH.initial, hidden: false };
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(PANEL_KEY, JSON.stringify(panel));
    } catch {
      // Storage may be unavailable
    }
  }, [panel]);
  return [panel, setPanel] as const;
}

const pressed = { x: 0, y: 0 };
if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", (event) => Object.assign(pressed, { x: event.clientX, y: event.clientY }), true);
  if ("__TAURI_INTERNALS__" in window && /Mac/.test(navigator.platform || navigator.userAgent)) {
    document.documentElement.dataset.macosOverlay = "true";
  }
}

function setTheme(next: "light" | "dark", spread: boolean) {
  const root = document.documentElement;
  if (root.dataset.theme === next) return;
  const still = typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const swap = () => {
    root.dataset.theme = next;
  };
  if (!spread || still || !("startViewTransition" in document)) return swap();
  const x = pressed.x || window.innerWidth - 40;
  const y = pressed.y || 40;
  const reach = Math.hypot(Math.max(x, window.innerWidth - x), Math.max(y, window.innerHeight - y));
  root.style.setProperty("--spread-x", `${x}px`);
  root.style.setProperty("--spread-y", `${y}px`);
  root.style.setProperty("--spread-r", `${reach}px`);
  (document as Document & { startViewTransition: (update: () => void) => unknown }).startViewTransition(swap);
}

export function useTheme(theme: Settings["theme"] | undefined) {
  const read = useRef(false);
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const system = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = (spread: boolean) => setTheme(theme === "dark" || (theme !== "light" && system.matches) ? "dark" : "light", spread);
    apply(read.current);
    if (theme !== undefined) read.current = true;
    const follow = () => apply(true);
    system.addEventListener("change", follow);
    return () => system.removeEventListener("change", follow);
  }, [theme]);
}

export interface UseGlobalShortcutsOptions {
  inChat: boolean;
  isProjectSession: boolean;
  hasActive: boolean;
  compactLayout: boolean;
  drawerOpen: boolean;
  setDrawerOpen: Dispatch<SetStateAction<boolean>>;
  setJumping: Dispatch<SetStateAction<"all" | "files" | null>>;
  setSidebar: Dispatch<SetStateAction<{ width: number; hidden: boolean }>>;
  summonTerminal: () => void;
}

export function useGlobalShortcuts({
  inChat,
  isProjectSession,
  hasActive,
  compactLayout,
  drawerOpen,
  setDrawerOpen,
  setJumping,
  setSidebar,
  summonTerminal,
}: UseGlobalShortcutsOptions) {
  const shortcutContext = useRef({ inChat, isProjectSession, hasActive, compactLayout, drawerOpen });
  shortcutContext.current = { inChat, isProjectSession, hasActive, compactLayout, drawerOpen };

  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (
        event.key === "Escape" &&
        !event.defaultPrevented &&
        shortcutContext.current.compactLayout &&
        shortcutContext.current.drawerOpen &&
        !document.querySelector("dialog[open], .slash-menu, [role='menu'], .confirm-bubble, .combo-list") &&
        !(event.target instanceof HTMLElement && event.target.closest(".xterm"))
      ) {
        event.preventDefault();
        setDrawerOpen(false);
        return;
      }
      const mod = event.ctrlKey || event.metaKey;
      const lower = event.key.toLowerCase();
      if (mod && !event.altKey && lower === "k") {
        event.preventDefault();
        if (document.querySelector("dialog[open]:not(.jump)")) return;
        setJumping((cur) => (cur === "all" ? null : "all"));
        return;
      }
      if (mod && !event.altKey && lower === "p" && shortcutContext.current.isProjectSession) {
        event.preventDefault();
        if (document.querySelector("dialog[open]:not(.jump)")) return;
        setJumping((cur) => (cur === "files" ? null : "files"));
        return;
      }
      if (mod && !event.altKey && !event.shiftKey && lower === "b") {
        event.preventDefault();
        setSidebar((cur) => ({ ...cur, hidden: !cur.hidden }));
        return;
      }
      if (
        shortcutContext.current.hasActive &&
        ((mod && !event.altKey && lower === "j") || (event.ctrlKey && !event.altKey && event.key === "`"))
      ) {
        event.preventDefault();
        summonTerminal();
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [setDrawerOpen, setJumping, setSidebar, summonTerminal]);
}

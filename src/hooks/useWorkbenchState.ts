import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { SidebarTab } from "../components/Sidebar";
import type { WorkbenchTab } from "../components/Workbench";
import type { DiffStage } from "../lib/types";

export function diffTabId(
  stage: DiffStage,
  commit: string | undefined,
  repo: string | null | undefined,
  path: string,
): string {
  return `diff:${repo ?? ""}:${stage}:${commit ?? ""}:${path}`;
}

export function renameWorkbenchTabs(
  tabs: WorkbenchTab[],
  activeTabId: string | null,
  fromRel: string,
  toRel: string,
): { tabs: WorkbenchTab[]; activeTabId: string | null } {
  let nextActiveId = activeTabId;
  const nextTabs = tabs.map((t) => {
    if (t.path === fromRel) {
      const name = toRel.split("/").pop() || toRel;
      const nextId = t.kind === "file" ? `file:${toRel}` : diffTabId(t.stage, t.commit, t.repo, toRel);
      if (nextActiveId === t.id) nextActiveId = nextId;
      return { ...t, id: nextId, path: toRel, name };
    }
    if (t.path.startsWith(`${fromRel}/`)) {
      const updated = `${toRel}${t.path.slice(fromRel.length)}`;
      const nextId = t.kind === "file" ? `file:${updated}` : diffTabId(t.stage, t.commit, t.repo, updated);
      if (nextActiveId === t.id) nextActiveId = nextId;
      return { ...t, id: nextId, path: updated };
    }
    return t;
  });
  return { tabs: nextTabs, activeTabId: nextActiveId };
}

export interface UseWorkbenchStateOptions {
  inChat: boolean;
  isProjectSession: boolean;
  activeWorkspace: string;
  replaying: boolean;
  selectedGitRepo: string | null;
  leftTab: SidebarTab;
  setLeftTab: (tab: SidebarTab) => void;
}

export function useWorkbenchState({
  inChat,
  isProjectSession,
  activeWorkspace,
  replaying,
  selectedGitRepo,
  leftTab,
  setLeftTab,
}: UseWorkbenchStateOptions) {
  const [wbTabs, setWbTabs] = useState<WorkbenchTab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [inWorkbench, setInWorkbench] = useState(false);

  const selectedRepoRef = useRef(selectedGitRepo);
  selectedRepoRef.current = selectedGitRepo;

  const lastWorkspaceRef = useRef(activeWorkspace);
  useEffect(() => {
    if (lastWorkspaceRef.current !== activeWorkspace) {
      lastWorkspaceRef.current = activeWorkspace;
      setWbTabs([]);
      setActiveTabId(null);
      setInWorkbench(false);
    }
    if (!isProjectSession && leftTab !== "sessions") {
      setLeftTab("sessions");
    }
  }, [activeWorkspace, isProjectSession, leftTab, setLeftTab]);

  const openWorkbenchFile = useCallback((relPath: string, _pin = false, line?: number) => {
    const clean = relPath.replace(/\\/g, "/").replace(/^\/+/, "");
    if (!clean) return;
    const name = clean.split("/").pop() || clean;
    const id = `file:${clean}`;
    setWbTabs((prev) => {
      const existing = prev.find((t) => t.id === id);
      if (existing && existing.kind === "file") {
        return prev.map((t) =>
          t.id === id && t.kind === "file" ? { ...t, line: line ?? t.line, jumpSeq: (t.jumpSeq ?? 0) + 1 } : t,
        );
      }
      const nextTab: WorkbenchTab = { id, kind: "file", path: clean, name, line, jumpSeq: line ? 1 : 0 };
      return [...prev, nextTab];
    });
    setActiveTabId(id);
    setInWorkbench(true);
  }, []);

  const openWorkbenchDiff = useCallback(
    (relPath: string, stage: DiffStage, _pin = false, commit?: string, repoOverride?: string | null) => {
      const clean = relPath.replace(/\\/g, "/").replace(/^\/+/, "");
      if (!clean) return;
      const name = clean.split("/").pop() || clean;
      const repo = repoOverride !== undefined ? repoOverride : selectedRepoRef.current;
      const id = diffTabId(stage, commit, repo, clean);
      setWbTabs((prev) => {
        const existing = prev.find((t) => t.id === id);
        if (existing) return prev;
        const nextTab: WorkbenchTab = { id, kind: "diff", path: clean, name, stage, commit, repo: repo ?? null };
        return [...prev, nextTab];
      });
      setActiveTabId(id);
      setInWorkbench(true);
    },
    [],
  );

  const moveWorkbenchTab = useCallback((fromId: string, toId: string) => {
    setWbTabs((prev) => {
      const fromIdx = prev.findIndex((t) => t.id === fromId);
      const toIdx = prev.findIndex((t) => t.id === toId);
      if (fromIdx === -1 || toIdx === -1 || fromIdx === toIdx) return prev;
      const next = [...prev];
      const [moved] = next.splice(fromIdx, 1);
      next.splice(toIdx, 0, moved);
      return next;
    });
  }, []);

  const closeWorkbenchTab = useCallback((id: string) => {
    setWbTabs((prev) => {
      const idx = prev.findIndex((t) => t.id === id);
      if (idx === -1) return prev;
      const next = prev.filter((t) => t.id !== id);
      setActiveTabId((cur) => {
        if (cur !== id) return cur;
        return next[Math.min(idx, next.length - 1)]?.id ?? null;
      });
      if (next.length === 0) setInWorkbench(false);
      return next;
    });
  }, []);

  const closeAllWorkbenchTabs = useCallback(() => {
    setWbTabs([]);
    setActiveTabId(null);
    setInWorkbench(false);
  }, []);

  const handleDeletedWorkspacePath = useCallback((relPath: string) => {
    setWbTabs((prev) => {
      const next = prev.filter((t) => t.path !== relPath && !t.path.startsWith(`${relPath}/`));
      if (next.length === 0) setInWorkbench(false);
      setActiveTabId((cur) => (next.some((t) => t.id === cur) ? cur : next[next.length - 1]?.id ?? null));
      return next;
    });
  }, []);

  const handleRenamedWorkspacePath = useCallback((fromRel: string, toRel: string) => {
    setWbTabs((prev) => {
      let updatedActive: string | null | undefined;
      const nextTabs = prev.map((t) => {
        if (t.path === fromRel) {
          const name = toRel.split("/").pop() || toRel;
          const nextId = t.kind === "file" ? `file:${toRel}` : diffTabId(t.stage, t.commit, t.repo, toRel);
          setActiveTabId((cur) => {
            if (cur === t.id) updatedActive = nextId;
            return updatedActive ?? cur;
          });
          return { ...t, id: nextId, path: toRel, name };
        }
        if (t.path.startsWith(`${fromRel}/`)) {
          const updated = `${toRel}${t.path.slice(fromRel.length)}`;
          const nextId = t.kind === "file" ? `file:${updated}` : diffTabId(t.stage, t.commit, t.repo, updated);
          setActiveTabId((cur) => {
            if (cur === t.id) updatedActive = nextId;
            return updatedActive ?? cur;
          });
          return { ...t, id: nextId, path: updated };
        }
        return t;
      });
      return nextTabs;
    });
  }, []);

  const showWorkbench = inChat && isProjectSession && inWorkbench && wbTabs.length > 0 && !replaying;
  const activeWbFile = useMemo(() => {
    if (!showWorkbench) return null;
    const cur = wbTabs.find((t) => t.id === activeTabId) ?? wbTabs[0];
    return cur?.kind === "file" ? cur.path : null;
  }, [showWorkbench, wbTabs, activeTabId]);

  return {
    wbTabs,
    activeTabId,
    setActiveTabId,
    inWorkbench,
    setInWorkbench,
    showWorkbench,
    activeWbFile,
    openWorkbenchFile,
    openWorkbenchDiff,
    moveWorkbenchTab,
    closeWorkbenchTab,
    closeAllWorkbenchTabs,
    handleDeletedWorkspacePath,
    handleRenamedWorkspacePath,
  };
}

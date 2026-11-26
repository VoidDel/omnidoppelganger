import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { GitBadge } from "../components/FileTree";
import { api } from "../lib/api";
import type { Envelope, GitStatus } from "../lib/types";

function normalizeWorkspacePath(value: string): string {
  return value.replace(/[\\/]+$/, "");
}

export interface UseWorkspaceGitSyncOptions {
  inChat: boolean;
  isProjectSession: boolean;
  activeId: string | null;
  activeWorkspace: string;
  events: Envelope[];
  onExternalBranchChange: () => void;
}

export function useWorkspaceGitSync({
  inChat,
  isProjectSession,
  activeId,
  activeWorkspace,
  events,
  onExternalBranchChange,
}: UseWorkspaceGitSyncOptions) {
  const [manualTick, setManualTick] = useState(0);
  const [buildTick, setBuildTick] = useState(0);
  const [gitState, setGitState] = useState<GitStatus | null>(null);
  const [selectedGitRepo, setSelectedGitRepo] = useState<string | null>(null);

  const activeWorkspaceRef = useRef(activeWorkspace);
  activeWorkspaceRef.current = activeWorkspace;

  const lastWorkspaceRef = useRef(activeWorkspace);
  useEffect(() => {
    if (lastWorkspaceRef.current !== activeWorkspace) {
      lastWorkspaceRef.current = activeWorkspace;
      setGitState(null);
    }
  }, [activeWorkspace]);

  const gitRefreshTick = useMemo(() => {
    let count = manualTick;
    for (const env of events) {
      if (
        env.type === "files/made" ||
        env.type === "files/restored" ||
        env.type === "artifact/restored" ||
        (env.type === "tool/result" &&
          !env.data.is_error &&
          ["edit_file", "write_file", "delete_file", "move_file", "run_command"].includes(env.data.tool))
      ) {
        count++;
      }
    }
    return count;
  }, [events, manualTick]);

  const fsRefreshTick = gitRefreshTick + buildTick;
  const bumpRefresh = useCallback(() => setManualTick((n) => n + 1), []);

  // Watch the active project workspace on disk so external edits, build outputs, and PTY commands auto-refresh UI.
  useEffect(() => {
    const targetId = inChat && isProjectSession && activeId ? activeId : null;
    api.watchWorkspace(targetId).catch(() => undefined);
  }, [inChat, isProjectSession, activeId, activeWorkspace]);

  useEffect(() => {
    const stop = api.onWorkspaceChanged((payload) => {
      const currentWs = activeWorkspaceRef.current;
      if (!currentWs || normalizeWorkspacePath(payload.workspace) !== normalizeWorkspacePath(currentWs)) {
        return;
      }
      if (payload.git) bumpRefresh();
      else setBuildTick((n) => n + 1);
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, [bumpRefresh]);

  useEffect(() => {
    let lastFocusAt = 0;
    const onFocus = () => {
      const now = Date.now();
      if (now - lastFocusAt >= 1500) {
        lastFocusAt = now;
        bumpRefresh();
      }
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [bumpRefresh]);

  const gitKey = useRef<string | null>(null);
  const gitWorkspaceKey = useRef<string | null>(null);
  useEffect(() => {
    const wsKey = `${activeId ?? ""}\u0000${activeWorkspace ?? ""}`;
    if (gitWorkspaceKey.current !== wsKey) {
      gitWorkspaceKey.current = wsKey;
      setSelectedGitRepo(null);
    }
  }, [activeId, activeWorkspace]);

  useEffect(() => {
    if (!isProjectSession || !activeId) {
      gitKey.current = null;
      setGitState(null);
      return;
    }
    const repoArg = selectedGitRepo ?? undefined;
    const key = `${activeId}\u0000${activeWorkspace}\u0000${repoArg ?? ""}`;
    const fresh = gitKey.current !== key;
    gitKey.current = key;
    const timer = setTimeout(
      () =>
        api.gitStatus(activeId, repoArg).then(
          (st) => {
            if (gitKey.current !== key) return;
            setGitState(st);
            if (selectedGitRepo === null && st.active_repo) {
              setSelectedGitRepo(st.active_repo);
            }
          },
          () => gitKey.current === key && setGitState(null),
        ),
      fresh ? 0 : 300,
    );
    return () => clearTimeout(timer);
  }, [isProjectSession, activeId, activeWorkspace, selectedGitRepo, gitRefreshTick]);

  const seenBranch = useRef<string | null>(null);
  useEffect(() => {
    const branch = gitState?.is_repo ? gitState.branch : null;
    if (branch && seenBranch.current && branch !== seenBranch.current) onExternalBranchChange();
    seenBranch.current = branch;
  }, [gitState?.is_repo, gitState?.branch, onExternalBranchChange]);

  const gitBadges = useMemo<Record<string, GitBadge>>(() => {
    if (!gitState?.is_repo) return {};
    const prefix = gitState.active_repo ? `${gitState.active_repo}/` : "";
    const map: Record<string, GitBadge> = {};
    for (const f of gitState.staged) {
      map[`${prefix}${f.path}`] = { label: f.status, tone: f.status === "A" ? "added" : f.status === "D" ? "deleted" : "modified" };
    }
    for (const f of gitState.unstaged) {
      map[`${prefix}${f.path}`] = { label: f.status, tone: f.status === "D" ? "deleted" : "modified" };
    }
    for (const f of gitState.untracked) {
      map[`${prefix}${f.path}`] = { label: "U", tone: "added" };
    }
    for (const f of gitState.conflicts) {
      map[`${prefix}${f.path}`] = { label: "!", tone: "conflict" };
    }
    return map;
  }, [gitState]);

  const gitChangeCount = gitState?.is_repo
    ? gitState.repos.length > 0
      ? gitState.repos.reduce((sum, r) => sum + r.dirty, 0)
      : gitState.conflicts.length + gitState.staged.length + gitState.unstaged.length + gitState.untracked.length
    : 0;

  return {
    gitState,
    setGitState,
    selectedGitRepo,
    setSelectedGitRepo,
    gitRefreshTick,
    fsRefreshTick,
    bumpRefresh,
    gitBadges,
    gitChangeCount,
  };
}

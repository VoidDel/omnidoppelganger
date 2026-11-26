import { Suspense, lazy } from "react";
import { api } from "../lib/api";
import type { State } from "../lib/sessionState";
import type { DiffStage, GitStatus, SessionItem, Status } from "../lib/types";
import type { ComposerQuoteInput } from "./Composer";
import type { GitBadge } from "./FileTree";
import { Guard } from "./Guard";
import { Sidebar, type SidebarTab } from "./Sidebar";

const FileTree = lazy(() => import("./FileTree").then((m) => ({ default: m.FileTree })));
const GitPanel = lazy(() => import("./GitPanel").then((m) => ({ default: m.GitPanel })));

export const PANEL_STRIP_WIDTH = 56;

export function normalizeQuote(input: string | ComposerQuoteInput): ComposerQuoteInput {
  if (typeof input !== "string") return input;
  const trimmed = input.trim();
  const lines = trimmed.split(/\r?\n/);
  const label = trimmed.startsWith("后台任务")
    ? "后台任务日志"
    : trimmed.includes("终端")
      ? "终端输出"
      : trimmed.startsWith("请看 `")
        ? trimmed.slice(4, trimmed.indexOf("`", 4) > 4 ? trimmed.indexOf("`", 4) : 24)
        : "引用内容";
  return { label, meta: lines.length > 1 ? `${lines.length} 行` : `${trimmed.length} 字`, content: trimmed };
}

export function joinWs(workspace: string, rel: string): string {
  const sep = workspace.includes("\\") ? "\\" : "/";
  return `${workspace.replace(/[\\/]+$/, "")}${sep}${rel.replace(/\//g, sep)}`;
}

export interface AppLeftSidebarProps {
  sessions: SessionItem[];
  active: SessionItem | undefined;
  activeId: string | null;
  isProjectSession: boolean;
  canAside: boolean;
  page: State["page"];
  tab: SidebarTab;
  onTab: (tab: SidebarTab) => void;
  width: number;
  onResize: (width: number) => void;
  status: Status;
  activeWbFile: string | null;
  fsRefreshTick: number;
  gitRefreshTick: number;
  gitChangeCount: number;
  gitBadges: Record<string, GitBadge>;
  gitState: GitStatus | null;
  onSelectGitRepo: (repo: string | null) => void;
  onSetGitState: (next: GitStatus) => void;
  onBumpRefresh: () => void;
  onOpenFile: (relPath: string, pin?: boolean) => void;
  onOpenDiff: (relPath: string, stage: DiffStage, pin?: boolean, commit?: string) => void;
  onAttachMain: (absPath: string) => void;
  onAttachAside: (absPath: string) => void;
  onDeletedPath: (rel: string) => void;
  onRenamedPath: (fromRel: string, toRel: string) => void;
  onAskReview: () => void;
  onHome: () => void;
  onMcp: () => void;
  onSkills: () => void;
  onSchedules: () => void;
  onSettings: () => void;
  onChat: () => void;
  onOpen: (id: string) => void;
  onNew: () => void;
  onNewIn: (workspace: string, isolated?: boolean) => void;
  onForkSession: (id: string) => void;
  onDeleteSession: (id: string) => void;
  onExportSession: (id: string, title: string) => void;
  onCleanScheduled: (ids: string[]) => void;
  onSessionUpdated: (item: SessionItem) => void;
  onNote: (note: string) => void;
  onError: (err: unknown) => void;
}

export function AppLeftSidebar(props: AppLeftSidebarProps) {
  const { active, isProjectSession, canAside } = props;
  return (
    <Guard name="分身列表" className="sidebar">
      <Sidebar
        sessions={props.sessions}
        activeId={props.activeId}
        page={props.page}
        tab={props.tab}
        onTab={props.onTab}
        width={props.width}
        onResize={props.onResize}
        gitCount={props.gitChangeCount}
        filesPane={
          isProjectSession && active ? (
            <Suspense fallback={<div className="viewer-placeholder muted">正在加载文件树…</div>}>
              <FileTree
                sessionId={active.id}
                workspace={active.workspace}
                activePath={props.activeWbFile}
                refreshTick={props.fsRefreshTick}
                gitBadges={props.gitBadges}
                onOpenFile={(relPath, pin) => props.onOpenFile(relPath, pin)}
                onAttachFile={props.onAttachMain}
                onAttachToAside={canAside ? props.onAttachAside : undefined}
                onDeletedPath={(rel) => (props.onDeletedPath(rel), props.onBumpRefresh())}
                onRenamedPath={(fromRel, toRel) => (props.onRenamedPath(fromRel, toRel), props.onBumpRefresh())}
                onError={props.onError}
              />
            </Suspense>
          ) : undefined
        }
        gitPane={
          isProjectSession && active ? (
            <Suspense fallback={<div className="viewer-placeholder muted">正在加载 Git 面板…</div>}>
              <GitPanel
                sessionId={active.id}
                workspace={active.workspace}
                isWorktree={!!active.worktree}
                status={props.gitState}
                onSelectRepo={props.onSelectGitRepo}
                refreshTick={props.gitRefreshTick}
                working={props.status !== "idle"}
                onStatusChanged={(next) => (next ? props.onSetGitState(next) : props.onBumpRefresh())}
                onFilesChanged={props.onBumpRefresh}
                onBranchSwitched={props.onSessionUpdated}
                onOpenDiff={props.onOpenDiff}
                onOpenFile={(relPath, pin) => props.onOpenFile(relPath, pin)}
                onAskReview={props.onAskReview}
                onNote={props.onNote}
                onError={props.onError}
              />
            </Suspense>
          ) : undefined
        }
        onHome={props.onHome}
        onMcp={props.onMcp}
        onSkills={props.onSkills}
        onSchedules={props.onSchedules}
        onChat={props.onChat}
        onOpen={props.onOpen}
        onNew={props.onNew}
        onNewIn={props.onNewIn}
        onSettings={props.onSettings}
        onSearch={api.searchSessions}
        onRenameSession={(id, title) => api.renameSession(id, title).then(props.onSessionUpdated, props.onError)}
        onForkSession={props.onForkSession}
        onDeleteSession={props.onDeleteSession}
        onExportSession={props.onExportSession}
        onCleanScheduled={props.onCleanScheduled}
        onRevealWorkspace={(id) => void api.openWorkspaceEntry(id, "", true).catch(props.onError)}
      />
    </Guard>
  );
}

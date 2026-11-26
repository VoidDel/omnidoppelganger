import { Suspense } from "react";
import { formatMessage, getCurrentLocale, type Locale, useI18n } from "../i18n";
import { api } from "../lib/api";
import type { State } from "../lib/sessionState";
import type { DiffStage, GitStatus, SessionItem, Status } from "../lib/types";
import type { ComposerQuoteInput, QuotePayload, StructuredQuote } from "./Composer";
import type { GitBadge } from "./FileTree";
import { Guard, lazyWithRetry } from "./Guard";
import { Sidebar, type SidebarTab } from "./Sidebar";

const FileTree = lazyWithRetry(() => import("./FileTree").then((m) => ({ default: m.FileTree })));
const GitPanel = lazyWithRetry(() => import("./GitPanel").then((m) => ({ default: m.GitPanel })));

export const PANEL_STRIP_WIDTH = 56;

function isStructuredQuote(input: Exclude<QuotePayload, string>): input is StructuredQuote {
  return "kind" in input && typeof input.kind === "string";
}

function baseFileName(filePath: string): string {
  const parts = filePath.split(/[\\/]/);
  return parts[parts.length - 1] || filePath;
}

/**
 * Convert a structured quote payload (or pre-built `ComposerQuoteInput` / legacy string) into a localized
 * `ComposerQuoteInput`, completely free of UI prefix / regex sniffing.
 */
export function normalizeQuote(input: QuotePayload, locale: Locale = getCurrentLocale()): ComposerQuoteInput {
  if (typeof input === "string") {
    const trimmed = input.trim();
    const lines = trimmed.split(/\r?\n/);
    const meta =
      lines.length > 1
        ? formatMessage(locale, "quote.linesMeta", { count: lines.length })
        : formatMessage(locale, "quote.charsMeta", { count: trimmed.length });
    return { label: formatMessage(locale, "quote.defaultLabel"), meta, content: trimmed };
  }

  if (!isStructuredQuote(input)) {
    return input;
  }

  const trimmed = input.content.trim();
  const lineCount = trimmed ? trimmed.split(/\r?\n/).length : 0;

  switch (input.kind) {
    case "job": {
      const label = input.jobId
        ? formatMessage(locale, "quote.jobLabel", { id: input.jobId })
        : formatMessage(locale, "quote.jobFallbackLabel");
      const meta =
        lineCount > 1
          ? formatMessage(locale, "quote.linesMeta", { count: lineCount })
          : formatMessage(locale, "quote.charsMeta", { count: trimmed.length });
      const content = formatMessage(locale, "quote.jobContent", {
        id: input.jobId ?? "?",
        command: input.command ?? "",
        content: trimmed,
      });
      return { label, meta, content };
    }
    case "terminal": {
      const meta =
        lineCount > 1
          ? formatMessage(locale, "quote.linesMeta", { count: lineCount })
          : formatMessage(locale, "quote.charsMeta", { count: trimmed.length });
      return {
        label: formatMessage(locale, "quote.terminalLabel"),
        meta,
        content: formatMessage(locale, "quote.terminalContent", { content: trimmed }),
      };
    }
    case "code": {
      const filePath = input.path ?? "";
      const fileName = baseFileName(filePath) || formatMessage(locale, "quote.defaultLabel");
      if (input.lineRange) {
        const [start, end] = input.lineRange;
        const shortRange = start === end ? `L${start}` : `L${start}-${end}`;
        const rangeText =
          start === end
            ? formatMessage(locale, "quote.codeSingleLine", { line: start })
            : formatMessage(locale, "quote.codeLineRange", { start, end });
        return {
          label: `${fileName}:${shortRange}`,
          meta: formatMessage(locale, "quote.linesMeta", { count: Math.max(1, end - start + 1) }),
          content: formatMessage(locale, "quote.codeContentRange", {
            path: filePath,
            range: rangeText,
            lang: input.language ?? "",
            content: input.content,
          }),
        };
      }
      const quoted = trimmed
        .split(/\r?\n/)
        .map((line) => `> ${line}`)
        .join("\n");
      return {
        label: fileName,
        meta:
          lineCount > 1
            ? formatMessage(locale, "quote.linesMeta", { count: lineCount })
            : formatMessage(locale, "quote.charsMeta", { count: trimmed.length }),
        content: filePath
          ? formatMessage(locale, "quote.codeContentPlain", { path: filePath, content: quoted })
          : quoted,
      };
    }
    case "diff": {
      const filePath = input.path ?? "";
      const fileName = baseFileName(filePath) || filePath;
      const stageText =
        input.stage === "staged"
          ? formatMessage(locale, "quote.diffStaged")
          : input.stage === "commit"
            ? formatMessage(locale, "quote.diffCommit", { commit: input.commit?.slice(0, 7) ?? "" })
            : formatMessage(locale, "quote.diffUnstaged");
      const meta =
        input.added !== undefined && input.removed !== undefined
          ? `+${input.added} −${input.removed}`
          : formatMessage(locale, "quote.linesMeta", { count: Math.max(1, lineCount) });
      return {
        label: `${fileName} · ${stageText}`,
        meta,
        content: formatMessage(locale, "quote.diffContent", {
          path: filePath,
          stage: stageText,
          content: trimmed,
        }),
      };
    }
  }
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
  inert?: boolean;
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
  const { t } = useI18n();
  return (
    <Guard
      name={t("guard.sidebar")}
      className="sidebar"
      resetKey={`${props.tab}:${props.activeId ?? ""}:${props.page}`}
    >
      <Sidebar
        inert={props.inert}
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
            <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
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
            <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
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

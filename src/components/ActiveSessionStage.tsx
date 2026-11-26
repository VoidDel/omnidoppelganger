import { type ComponentProps, type Dispatch, type ReactNode, type SetStateAction, Suspense, lazy } from "react";
import type { useSessionController } from "../hooks/useSessionController";
import type { useWorkbenchState } from "../hooks/useWorkbenchState";
import { api } from "../lib/api";
import type { Row, Usage } from "../lib/fold";
import { turnAt } from "../lib/replay";
import type { SlashCommand } from "../lib/slash";
import type { Envelope, Mode, Provider, SessionItem, SettingsView, Status } from "../lib/types";
import { AgentJobsPane } from "./AgentJobsPane";
import { joinWs } from "./AppPanes";
import { AsidePane } from "./AsidePane";
import { Composer, type ComposerQuoteInput, type ComposerSeed } from "./Composer";
import { ChatEmptyStage } from "./EmptyStage";
import { Guard } from "./Guard";
import { ReplayBar } from "./Replay";
import { SessionHeader } from "./SessionHeader";
import { type PanelTab, SidePanel } from "./SidePanel";
import { Transcript } from "./Transcript";
import { Workbench, WorkbenchTabsBar } from "./Workbench";

const TerminalPane = lazy(() => import("./TerminalPane"));
const DiffViewer = lazy(() => import("./DiffViewer").then((m) => ({ default: m.DiffViewer })));

export interface ActiveSessionStageProps {
  active: SessionItem;
  sessions: SessionItem[];
  events: Envelope[];
  queued: string[];
  status: Status;
  rows: Row[];
  stops: number[];
  spent: Usage;
  costUsd: number | null;
  planReady: boolean;
  isChatSession: boolean;
  isProjectSession: boolean;
  canAside: boolean;
  settings: SettingsView | null;
  providers: Provider[];
  replay: ReturnType<typeof useSessionController>["replay"];
  setReplay: Dispatch<SetStateAction<ReturnType<typeof useSessionController>["replay"]>>;
  wb: ReturnType<typeof useWorkbenchState>;
  fsRefreshTick: number;
  gitRefreshTick: number;
  sidebarHidden: boolean;
  onShowSidebar: () => void;
  panelHidden: boolean;
  onTogglePanel: () => void;
  banners: ReactNode;
  drafts: ComponentProps<typeof Composer>["drafts"];
  seed: ComposerSeed | null;
  onClearSeed: () => void;
  slashCommands: SlashCommand[];
  listedCheck: (provider: string) => boolean;
  keyedCheck: (provider: string) => boolean;
  onSaveLimit: (limit: number | "auto") => void;
  onOpenSession: (id: string) => void;
  onForkFrom: (turn?: number, text?: string) => void;
  onExportSession: (id: string, title: string) => void;
  onDeleteSession: (id: string) => void;
  onRefreshSessions: () => void;
  onSessionUpdated: (item: SessionItem) => void;
  onNote: (note: string) => void;
  onError: (err: unknown) => void;
  onDecide: ComponentProps<typeof Transcript>["onDecide"];
  onAnswer: ComponentProps<typeof Transcript>["onAnswer"];
  onOpenFile: ComponentProps<typeof Transcript>["onOpenFile"];
  onPicture: ComponentProps<typeof Transcript>["onPicture"];
  versionOf: ComponentProps<typeof Transcript>["versionOf"];
  onQuoteIntoMain: (input: string | ComposerQuoteInput) => void;
  onQuoteIntoAside: (input: string | ComposerQuoteInput) => void;
  onAttachIntoMain: (absPath: string) => void;
  onAttachIntoAside: (absPath: string) => void;
  onSwitchMode: (mode: Mode) => Promise<void>;
  onCarryOut: ComponentProps<typeof Composer>["onCarryOut"];
  onSend: ComponentProps<typeof Composer>["onSend"];
}

export function ActiveSessionStage(props: ActiveSessionStageProps) {
  const { active, replay, wb, canAside, isProjectSession } = props;
  return (
    <>
      <Guard name="标题栏" className="bar" resetKey={active.id}>
        <SessionHeader
          session={active}
          parent={props.sessions.find((s) => s.id === active.parent)}
          onOpenParent={() => active.parent && props.onOpenSession(active.parent)}
          source={props.sessions.find((s) => s.id === active.fork?.session)}
          onOpenSource={() => active.fork && props.onOpenSession(active.fork.session)}
          onFork={() => props.onForkFrom()}
          onExport={props.rows.length > 0 ? () => props.onExportSession(active.id, active.title) : undefined}
          replaying={!!replay}
          onReplay={() => props.setReplay(replay || props.stops.length === 0 ? null : { at: 0, playing: true, speed: 1 })}
          onLand={() =>
            api.landSession(active.id).then((landed) => {
              props.onNote(
                landed.commits
                  ? `已并回：${landed.commits} 个提交合并进了洞府的 ${landed.into} 分支。`
                  : "这个分身的工作树里没有新的改动，没什么可并回的。",
              );
              props.onRefreshSessions();
            }, props.onError)
          }
          sidebarHidden={props.sidebarHidden}
          onToggleSidebar={props.onShowSidebar}
          panelHidden={props.panelHidden}
          onTogglePanel={props.onTogglePanel}
          onRename={(title) => api.renameSession(active.id, title).then(props.onSessionUpdated, props.onError)}
          onDelete={() => api.deleteSession(active.id).then(() => props.onDeleteSession(active.id), props.onError)}
        />
      </Guard>
      {props.banners}
      {isProjectSession && wb.wbTabs.length > 0 && !replay && (
        <WorkbenchTabsBar
          tabs={wb.wbTabs}
          inWorkbench={wb.showWorkbench}
          activeTabId={wb.activeTabId}
          chatStatus={props.status}
          onSelectChat={() => wb.setInWorkbench(false)}
          onSelectTab={(id) => (wb.setActiveTabId(id), wb.setInWorkbench(true))}
          onMoveTab={wb.moveWorkbenchTab}
          onCloseTab={wb.closeWorkbenchTab}
          onCloseAllTabs={wb.closeAllWorkbenchTabs}
        />
      )}
      {wb.showWorkbench ? (
        <Guard name="工作台" className="fill" resetKey={active.id}>
          <Workbench
            sessionId={active.id}
            tabs={wb.wbTabs}
            activeTabId={wb.activeTabId}
            refreshTick={props.fsRefreshTick}
            onQuoteToMain={props.onQuoteIntoMain}
            onQuoteToAside={canAside ? props.onQuoteIntoAside : undefined}
            onAttachPath={(rel) => props.onAttachIntoMain(joinWs(active.workspace, rel))}
            onAttachToAside={canAside ? (rel) => props.onAttachIntoAside(joinWs(active.workspace, rel)) : undefined}
            onError={props.onError}
            renderDiff={(tab) => (
              <Suspense fallback={<div className="viewer-placeholder muted">正在加载对比…</div>}>
                <DiffViewer
                  key={tab.id}
                  sessionId={active.id}
                  path={tab.path}
                  stage={tab.stage}
                  commit={tab.commit}
                  repo={tab.repo}
                  refreshTick={props.gitRefreshTick}
                  onOpenSource={(relPath, line) => wb.openWorkbenchFile(relPath, false, line)}
                  onQuoteToMain={props.onQuoteIntoMain}
                  onQuoteToAside={canAside ? props.onQuoteIntoAside : undefined}
                />
              </Suspense>
            )}
          />
        </Guard>
      ) : (
        <>
          {props.rows.length === 0 && props.queued.length === 0 ? (
            <ChatEmptyStage
              active={active}
              isChatSession={props.isChatSession}
              chatFolder={props.settings?.chat_folder}
              providers={props.providers}
            />
          ) : (
            <Guard name="对话" className="fill" resetKey={active.id}>
              <Transcript
                key={active.id}
                sessionId={active.id}
                rows={props.rows}
                queued={replay ? [] : props.queued}
                status={replay ? "idle" : props.status}
                mode={active.mode}
                origin={active.parent ? "agent" : active.schedule ? "schedule" : "user"}
                sessions={props.sessions}
                copied={active.fork?.turn ?? 0}
                frozen={!!replay}
                onOpen={props.onOpenSession}
                onDecide={props.onDecide}
                onAnswer={props.onAnswer}
                onUndo={(turn) => api.undoTurn(active.id, turn).catch(props.onError)}
                onFork={active.parent ? undefined : props.onForkFrom}
                onOpenFile={props.onOpenFile}
                onPicture={props.onPicture}
                versionOf={props.versionOf}
                onDeliverNow={replay ? undefined : () => api.deliverQueued(active.id).catch(props.onError)}
                onQuoteToMain={replay ? undefined : props.onQuoteIntoMain}
                onQuoteToAside={canAside ? props.onQuoteIntoAside : undefined}
              />
            </Guard>
          )}
          {replay && (
            <ReplayBar
              at={replay.at}
              count={props.stops.length}
              playing={replay.playing}
              speed={replay.speed}
              turn={turnAt(props.events, props.stops[replay.at] ?? 0)}
              time={new Date(props.events[props.stops[replay.at] ?? 0]?.ts ?? Date.now()).toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
                second: "2-digit",
              })}
              onSeek={(at) => props.setReplay((now) => now && { ...now, at })}
              onPlaying={(p) => props.setReplay((now) => now && { ...now, playing: p })}
              onSpeed={(s) => props.setReplay((now) => now && { ...now, speed: s })}
              onExit={() => props.setReplay(null)}
            />
          )}
          <div style={{ display: replay ? "none" : "contents" }}>
            <Guard name="输入框" className="under" resetKey={active.id}>
              <Composer
                key={active.id}
                sessionKey={active.id}
                workspace={active.workspace}
                drafts={props.drafts}
                status={props.status}
                providers={props.providers}
                listed={props.listedCheck}
                keyed={props.keyedCheck}
                providerId={active.provider}
                model={active.model}
                effort={active.effort ?? props.settings?.settings.effort ?? "high"}
                effortSet={active.effort !== null}
                efforts={props.settings?.efforts ?? ["low", "medium", "high", "xhigh", "max"]}
                onEffort={(effort) => api.setEffort(active.id, effort).then(props.onSessionUpdated, props.onError)}
                listModels={api.listModels}
                onModel={(p, m) => api.setModel(active.id, p, m).then(props.onSessionUpdated, props.onError)}
                vision={active.vision}
                mode={active.mode}
                usage={props.spent}
                costUsd={props.costUsd}
                contextLimit={active.context_limit}
                contextCeiling={active.context_ceiling}
                contextSetting={props.settings?.settings.context_limit ?? active.context_limit}
                contextAuto={!!props.settings?.settings.context_auto}
                onLimit={props.onSaveLimit}
                planReady={props.planReady}
                onMode={(mode) => props.onSwitchMode(mode).catch(props.onError)}
                onCarryOut={props.onCarryOut}
                onSend={props.onSend}
                onStop={() => api.cancelTurn(active.id).catch(props.onError)}
                onCompact={(focus) => api.compactSession(active.id, focus).catch(props.onError)}
                trustable={!!active.worktree}
                seed={props.seed}
                onSeedConsumed={props.onClearSeed}
                commands={props.slashCommands}
              />
            </Guard>
          </div>
        </>
      )}
    </>
  );
}

export interface AppRightPanelProps {
  active: SessionItem | undefined;
  activeId: string | null;
  sessions: SessionItem[];
  status: Status;
  replaying: boolean;
  canAside: boolean;
  isProjectSession: boolean;
  showWorkbench: boolean;
  compactLayout: boolean;
  strip: boolean;
  panelWidth: number;
  sidebarWidth: number;
  files: ComponentProps<typeof SidePanel>["files"];
  runs: ComponentProps<typeof SidePanel>["runs"];
  plan: ComponentProps<typeof SidePanel>["todos"];
  made: ComponentProps<typeof SidePanel>["products"];
  summonTab: { id: number; tab: PanelTab } | null;
  drafts: ComponentProps<typeof Composer>["drafts"];
  providers: Provider[];
  defaultEffort: string;
  efforts: string[];
  contextSetting: number;
  contextAuto: boolean;
  asideSeed: ComposerSeed | null;
  onClearAsideSeed: () => void;
  listedCheck: (provider: string) => boolean;
  keyedCheck: (provider: string) => boolean;
  onSaveLimit: (limit: number | "auto", onDone: () => void) => void;
  onRefreshSessions: () => void;
  onAsideId: (asideId: string | null) => void;
  onForwardFromAside: (text: string) => void;
  onQuoteIntoChat: (input: string | ComposerQuoteInput) => void;
  onResize: (width: number) => void;
  onWiden: () => void;
  onCloseDrawer: () => void;
  onInspectFile?: (relPath: string) => void;
  onOpenFile: ComponentProps<typeof SidePanel>["onOpenFile"];
  onRestoreArtifact: ComponentProps<typeof SidePanel>["onRestoreArtifact"];
  onSaveArtifactCopy: ComponentProps<typeof SidePanel>["onSaveArtifactCopy"];
  onError: (err: unknown) => void;
}

export function AppRightPanel(props: AppRightPanelProps) {
  const { active, canAside, replaying } = props;
  return (
    <>
      {props.compactLayout && <div className="panel-scrim" aria-hidden="true" onClick={props.onCloseDrawer} />}
      <Guard name="侧栏" className="panel" resetKey={props.activeId}>
        <SidePanel
          files={props.files}
          runs={props.runs}
          todos={props.plan}
          products={props.made}
          workspace={active?.workspace}
          width={props.panelWidth}
          sidebarWidth={props.sidebarWidth}
          onResize={props.onResize}
          narrow={props.strip}
          onWiden={props.onWiden}
          working={props.status === "running"}
          frozen={replaying}
          renderAside={
            canAside && active
              ? (visible) => (
                  <AsidePane
                    key={active.id}
                    main={active}
                    visible={visible}
                    sessions={props.sessions}
                    onAsideId={props.onAsideId}
                    onForward={props.onForwardFromAside}
                    onError={props.onError}
                    renderComposer={({ aside, status: asideStatus, usage: asideUsage, onItem, onSend: sendAside, onStop: stopAside, onCompact: compactAside }) => (
                      <Composer
                        key={aside.id}
                        sessionKey={aside.id}
                        workspace={aside.workspace}
                        drafts={props.drafts}
                        status={asideStatus}
                        providers={props.providers}
                        listed={props.listedCheck}
                        keyed={props.keyedCheck}
                        providerId={aside.provider}
                        model={aside.model}
                        effort={aside.effort ?? props.defaultEffort}
                        effortSet={aside.effort !== null}
                        efforts={props.efforts}
                        onEffort={(effort) => api.setEffort(aside.id, effort).then(onItem, props.onError)}
                        listModels={api.listModels}
                        onModel={(p, m) => api.setModel(aside.id, p, m).then(onItem, props.onError)}
                        vision={aside.vision}
                        mode={aside.mode}
                        usage={asideUsage}
                        contextLimit={aside.context_limit}
                        contextCeiling={aside.context_ceiling}
                        contextSetting={props.contextSetting || aside.context_limit}
                        contextAuto={props.contextAuto}
                        onLimit={(limit) =>
                          props.onSaveLimit(limit, () => {
                            props.onRefreshSessions();
                            api.asideSession(active.id).then(
                              (v) => v && onItem((({ events: _, abandoned_brief: __, ...it }) => it)(v)),
                              () => undefined,
                            );
                          })
                        }
                        planReady={false}
                        onMode={(mode) => api.setMode(aside.id, mode).then(onItem, props.onError)}
                        onCarryOut={() => undefined}
                        onSend={sendAside}
                        onStop={stopAside}
                        onCompact={compactAside}
                        trustable={false}
                        seed={props.asideSeed}
                        onSeedConsumed={props.onClearAsideSeed}
                        commands={[]}
                        aside
                        acceptDrops={visible && props.showWorkbench}
                        placeholder={
                          props.status !== "idle"
                            ? "主分身运功中，在此旁聊发问（只读，不打断它）…"
                            : "在此旁聊发问（只读洞府，自动知晓主分身进展）…"
                        }
                      />
                    )}
                  />
                )
              : undefined
          }
          jobsPane={
            active && !replaying ? (
              <AgentJobsPane sessionId={active.id} runs={props.runs} onSendToChat={props.onQuoteIntoChat} onError={props.onError} />
            ) : undefined
          }
          terminalPane={
            active && !replaying ? (
              <Suspense fallback={<div className="viewer-placeholder muted">正在加载用户终端…</div>}>
                <TerminalPane sessionId={active.id} onSendToChat={props.onQuoteIntoChat} onError={props.onError} />
              </Suspense>
            ) : undefined
          }
          summonTab={props.summonTab}
          onInspectFile={props.onInspectFile}
          onOpenFile={props.onOpenFile}
          onRestoreArtifact={props.onRestoreArtifact}
          onSaveArtifactCopy={props.onSaveArtifactCopy}
          onCloseDrawer={props.compactLayout ? props.onCloseDrawer : undefined}
        />
      </Guard>
    </>
  );
}

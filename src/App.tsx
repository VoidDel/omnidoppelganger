import { type CSSProperties, Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import { AppLeftSidebar, PANEL_STRIP_WIDTH, normalizeQuote } from "./components/AppPanes";
import type { ComposerQuoteInput } from "./components/Composer";
import { HomeEmptyStage, SecondaryPageStage, buildJumpActs } from "./components/EmptyStage";
import { Jump } from "./components/Jump";
import type { PanelTab } from "./components/SidePanel";
import type { SidebarTab } from "./components/Sidebar";
import { StatusBanners } from "./components/StatusBanners";
import { useCompactLayout, useGlobalShortcuts, usePanel, useSidebar, useTheme } from "./hooks/useGlobalShortcuts";
import { useSessionController } from "./hooks/useSessionController";
import { REVIEW_TASK, useSlashCommands } from "./hooks/useSlashCommands";
import { useWorkbenchState } from "./hooks/useWorkbenchState";
import { useWorkspaceGitSync } from "./hooks/useWorkspaceGitSync";
import { api } from "./lib/api";
import { forwarded } from "./lib/aside";
import { isProviderEnabled, isProviderKeyed } from "./lib/providers";

const ActiveSessionStage = lazy(() =>
  import("./components/ActiveSessionStage").then((m) => ({ default: m.ActiveSessionStage })),
);
const AppRightPanel = lazy(() =>
  import("./components/ActiveSessionStage").then((m) => ({ default: m.AppRightPanel })),
);

export default function App() {
  const ctl = useSessionController();
  const { state, dispatch, fail, refresh, refreshSettings, saved, scheduled, active, activeId, replay, setReplay } = ctl;
  const [jumping, setJumping] = useState<"all" | "files" | null>(null);
  useTheme(state.settings?.settings.theme);

  const providers = state.settings?.providers ?? [];
  const defaultProvider = providers.find((provider) => provider.id === state.settings?.settings.provider);
  const missingKey = !!defaultProvider && !isProviderKeyed(defaultProvider.id, providers, state.settings?.keys);
  const inChat = state.page === "chat";
  const compactLayout = useCompactLayout();
  const [sidebar, setSidebar] = useSidebar();
  const [leftTab, setLeftTab] = useState<SidebarTab>("sessions");
  const [panel, setPanel] = usePanel();
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [peek, setPeek] = useState(false);
  const [summonPanelTab, setSummonPanelTab] = useState<{ id: number; tab: PanelTab } | null>(null);
  const prevActiveIdRef = useRef(activeId);
  useEffect(() => {
    if (prevActiveIdRef.current !== activeId) {
      prevActiveIdRef.current = activeId;
      setPeek(false);
      setDrawerOpen(false);
    }
  });

  const isChatSession = !!active && ctl.chatting(active);
  const isProjectSession = !!active && !isChatSession;
  const activeWorkspace = active?.workspace ?? "";

  const git = useWorkspaceGitSync({
    inChat,
    isProjectSession,
    activeId,
    activeWorkspace,
    events: state.events,
    onExternalBranchChange: refresh,
  });

  const wb = useWorkbenchState({
    inChat,
    isProjectSession,
    activeWorkspace,
    replaying: !!replay,
    selectedGitRepo: git.selectedGitRepo,
    leftTab,
    setLeftTab,
  });

  const canAside = !!active && !active.parent && !replay;
  const strip = !compactLayout && (panel.hidden || (ctl.made.length === 0 && ctl.plan.length === 0 && !peek));
  const panelHidden = compactLayout ? !drawerOpen : strip;
  const showPanel = inChat && !!active && (!compactLayout || drawerOpen);
  const showWorkbenchRef = useRef(wb.showWorkbench);
  showWorkbenchRef.current = wb.showWorkbench;

  const summonPanel = useCallback(
    (tab: PanelTab) => {
      dispatch({ type: "page", page: "chat" });
      setPanel((cur) => ({ ...cur, hidden: false }));
      setDrawerOpen(true);
      setPeek(true);
      setSummonPanelTab({ id: Date.now(), tab });
    },
    [dispatch, setPanel],
  );
  const summonAside = useCallback(() => summonPanel("chat"), [summonPanel]);
  const summonTerminal = useCallback(() => summonPanel("terminal"), [summonPanel]);

  const quoteIntoMain = useCallback(
    (input: string | ComposerQuoteInput) => {
      if (wb.showWorkbench) wb.setInWorkbench(false);
      ctl.setSeed({ id: Date.now(), quote: normalizeQuote(input) });
    },
    [wb.showWorkbench, wb.setInWorkbench, ctl.setSeed],
  );
  const quoteIntoAside = useCallback(
    (input: string | ComposerQuoteInput) => {
      if (!canAside) return;
      ctl.setAsideSeed({ id: Date.now(), quote: normalizeQuote(input) });
      summonAside();
    },
    [canAside, summonAside, ctl.setAsideSeed],
  );
  const quoteIntoChat = useCallback(
    (input: string | ComposerQuoteInput) => (wb.showWorkbench && canAside ? quoteIntoAside(input) : quoteIntoMain(input)),
    [wb.showWorkbench, canAside, quoteIntoAside, quoteIntoMain],
  );
  const attachIntoMain = useCallback(
    (absPath: string) => {
      if (wb.showWorkbench) wb.setInWorkbench(false);
      ctl.setSeed({ id: Date.now(), attachPaths: [absPath] });
    },
    [wb.showWorkbench, wb.setInWorkbench, ctl.setSeed],
  );
  const attachIntoAside = useCallback(
    (absPath: string) => {
      if (!canAside) return;
      ctl.setAsideSeed({ id: Date.now(), attachPaths: [absPath] });
      summonAside();
    },
    [canAside, summonAside, ctl.setAsideSeed],
  );

  const handleForwardFromAside = useCallback(
    (text: string) => {
      ctl.send(forwarded(text));
      if (ctl.stateRef.current.status !== "idle") {
        dispatch({ type: "note", note: "已作为传音转给主分身，它做完眼下这一步就会收到。" });
      } else if (showWorkbenchRef.current) {
        dispatch({ type: "note", note: "已转给主分身。" });
      }
    },
    [ctl.send, ctl.stateRef, dispatch],
  );

  useGlobalShortcuts({
    inChat,
    isProjectSession,
    hasActive: !!active,
    compactLayout,
    drawerOpen,
    setDrawerOpen,
    setJumping,
    setSidebar,
    summonTerminal,
  });

  const toSchedules = useCallback(() => {
    api.listSchedules().then((schedules) => dispatch({ type: "schedules", schedules }), fail);
    dispatch({ type: "page", page: "schedules" });
  }, [dispatch, fail]);
  const toSettings = useCallback(() => (refreshSettings(), dispatch({ type: "page", page: "settings" })), [refreshSettings, dispatch]);
  const toMcp = useCallback(() => (refreshSettings(), dispatch({ type: "page", page: "mcp" })), [refreshSettings, dispatch]);
  const toSkills = useCallback(() => dispatch({ type: "page", page: "skills" }), [dispatch]);
  const showSidebarTab = useCallback(
    (tab: SidebarTab) => (dispatch({ type: "page", page: "chat" }), setSidebar((cur) => ({ ...cur, hidden: false })), setLeftTab(tab)),
    [dispatch, setSidebar],
  );

  const slashCommands = useSlashCommands({
    active,
    isChatSession,
    status: state.status,
    events: state.events,
    settingsSkills: state.settings?.skills,
    hasRows: ctl.rows.length > 0,
    hasStops: ctl.stops.length > 0,
    fsRefreshTick: git.fsRefreshTick,
    createIn: ctl.createIn,
    summonAside,
    summonTerminal,
    forkFrom: ctl.forkFrom,
    exportSession: ctl.exportSession,
    send: ctl.send,
    showSidebarTab,
    startReplay: () => setReplay({ at: 0, playing: true, speed: 1 }),
    toMcp,
    toSkills,
    toSchedules,
    toSettings,
    onSessionUpdated: (item) => dispatch({ type: "session", item }),
    onNote: (note) => dispatch({ type: "note", note }),
    onError: fail,
  });

  const listedCheck = useCallback(
    (provider: string) => !!state.settings && !!providers.find((known) => known.id === provider && isProviderEnabled(state.settings!, known)),
    [state.settings, providers],
  );
  const keyedCheck = useCallback(
    (provider: string) => isProviderKeyed(provider, providers, state.settings?.keys),
    [providers, state.settings?.keys],
  );

  const saveLimit = (limit: number | "auto", onDone: () => void) => {
    if (!state.settings) return;
    const next = limit === "auto" ? { ...state.settings.settings, context_auto: true } : { ...state.settings.settings, context_limit: limit, context_auto: false };
    saved(api.saveSettings(next)).then(onDone);
  };

  const banners = (
    <StatusBanners
      error={state.error}
      note={state.note}
      active={active}
      replaying={!!replay}
      onDismissError={() => dispatch({ type: "error", error: null })}
      onDismissNote={() => dispatch({ type: "note", note: null })}
      onTrusted={(item, note) => (dispatch({ type: "session", item }), dispatch({ type: "note", note }))}
      onError={fail}
    />
  );

  return (
    <div
      className={["app", showPanel && "with-panel", sidebar.hidden && "sidebar-hidden"].filter(Boolean).join(" ")}
      style={{ "--sidebar-width": `${sidebar.width}px`, "--panel-width": `${strip ? PANEL_STRIP_WIDTH : panel.width}px` } as CSSProperties}
    >
      {!sidebar.hidden && (
        <AppLeftSidebar
          sessions={state.sessions}
          active={active}
          activeId={activeId}
          isProjectSession={isProjectSession}
          canAside={canAside}
          page={state.page}
          tab={leftTab}
          onTab={setLeftTab}
          width={sidebar.width}
          onResize={(width) => setSidebar({ ...sidebar, width })}
          status={state.status}
          activeWbFile={wb.activeWbFile}
          fsRefreshTick={git.fsRefreshTick}
          gitRefreshTick={git.gitRefreshTick}
          gitChangeCount={git.gitChangeCount}
          gitBadges={git.gitBadges}
          gitState={git.gitState}
          onSelectGitRepo={git.setSelectedGitRepo}
          onSetGitState={git.setGitState}
          onBumpRefresh={git.bumpRefresh}
          onOpenFile={wb.openWorkbenchFile}
          onOpenDiff={wb.openWorkbenchDiff}
          onAttachMain={attachIntoMain}
          onAttachAside={attachIntoAside}
          onDeletedPath={wb.handleDeletedWorkspacePath}
          onRenamedPath={wb.handleRenamedWorkspacePath}
          onAskReview={() => (ctl.send(REVIEW_TASK), wb.showWorkbench && wb.setInWorkbench(false))}
          onHome={ctl.goHome}
          onMcp={toMcp}
          onSkills={toSkills}
          onSchedules={toSchedules}
          onSettings={toSettings}
          onChat={ctl.chat}
          onOpen={ctl.open}
          onNew={ctl.create}
          onNewIn={ctl.createIn}
          onForkSession={ctl.forkSessionById}
          onDeleteSession={(id) => api.deleteSession(id).then(() => ctl.dispatchDeleted(id), fail)}
          onExportSession={ctl.exportSession}
          onCleanScheduled={(ids) => void ctl.cleanScheduledSessions(ids)}
          onSessionUpdated={(item) => dispatch({ type: "session", item })}
          onNote={(note) => dispatch({ type: "note", note })}
          onError={fail}
        />
      )}
      <main className="main">
        <div className="stage-drag" data-tauri-drag-region />
        {!(inChat && active) && banners}
        {!inChat ? (
          <SecondaryPageStage
            page={state.page}
            settings={state.settings}
            schedules={state.schedules}
            sessions={state.sessions}
            active={active}
            saved={saved}
            scheduled={scheduled}
            refreshSettings={refreshSettings}
            onSettingsLoaded={(settings) => dispatch({ type: "settings", settings })}
            onSchedulesLoaded={(schedules) => dispatch({ type: "schedules", schedules })}
            onOpenSession={ctl.open}
            onCleanSessions={(ids) => void ctl.cleanScheduledSessions(ids)}
            onError={fail}
          />
        ) : active ? (
          <Suspense fallback={null}>
            <ActiveSessionStage
              active={active}
              sessions={state.sessions}
              events={state.events}
              queued={state.queued}
              status={state.status}
              rows={ctl.rows}
              stops={ctl.stops}
              spent={ctl.spent}
              costUsd={ctl.costUsd}
              planReady={ctl.planReady}
              isChatSession={isChatSession}
              isProjectSession={isProjectSession}
              canAside={canAside}
              settings={state.settings}
              providers={providers}
              replay={replay}
              setReplay={setReplay}
              wb={wb}
              fsRefreshTick={git.fsRefreshTick}
              gitRefreshTick={git.gitRefreshTick}
              sidebarHidden={sidebar.hidden}
              onShowSidebar={() => setSidebar((cur) => ({ ...cur, hidden: false }))}
              panelHidden={panelHidden}
              onTogglePanel={() => {
                if (compactLayout) setDrawerOpen((o) => (!o && setPeek(true), !o));
                else if (strip) (setPeek(true), setPanel({ ...panel, hidden: false }));
                else setPanel({ ...panel, hidden: true });
              }}
              banners={banners}
              drafts={ctl.draftsRef.current}
              seed={ctl.seed}
              onClearSeed={() => ctl.setSeed(null)}
              slashCommands={slashCommands}
              listedCheck={listedCheck}
              keyedCheck={keyedCheck}
              onSaveLimit={(limit) => saveLimit(limit, refresh)}
              onOpenSession={ctl.open}
              onForkFrom={ctl.forkFrom}
              onExportSession={ctl.exportSession}
              onDeleteSession={ctl.dispatchDeleted}
              onRefreshSessions={refresh}
              onSessionUpdated={(item) => dispatch({ type: "session", item })}
              onNote={(note) => dispatch({ type: "note", note })}
              onError={fail}
              onDecide={ctl.decide}
              onAnswer={ctl.answer}
              onOpenFile={ctl.openFile}
              onPicture={ctl.pictureOf}
              versionOf={ctl.versionOf}
              onQuoteIntoMain={quoteIntoMain}
              onQuoteIntoAside={quoteIntoAside}
              onAttachIntoMain={attachIntoMain}
              onAttachIntoAside={attachIntoAside}
              onSwitchMode={ctl.switchMode}
              onCarryOut={ctl.carryOut}
              onSend={ctl.send}
            />
          </Suspense>
        ) : (
          <HomeEmptyStage
            missingKey={missingKey}
            defaultProviderName={defaultProvider?.name}
            sessions={state.sessions}
            schedules={state.schedules}
            onCreate={ctl.create}
            onChat={ctl.chat}
            onOpenSettings={() => dispatch({ type: "page", page: "settings" })}
            onOpenSession={ctl.open}
            onOpenSchedules={toSchedules}
          />
        )}
      </main>
      {jumping && (
        <Jump
          mode={jumping}
          sessions={state.sessions}
          acts={buildJumpActs({ isProjectSession, active, goHome: ctl.goHome, create: ctl.create, chat: ctl.chat, openLeftTab: showSidebarTab, summonAside, summonTerminal, toSkills, toMcp, toSchedules, toSettings })}
          onOpen={ctl.open}
          onSearch={api.searchSessions}
          onSearchFiles={isProjectSession && activeId ? (q) => api.searchWorkspaceFiles(activeId, q, 60) : undefined}
          onOpenFile={isProjectSession ? (relPath, line) => (dispatch({ type: "page", page: "chat" }), wb.openWorkbenchFile(relPath, false, line)) : undefined}
          onClose={() => setJumping(null)}
        />
      )}
      {showPanel && (
        <Suspense fallback={null}>
          <AppRightPanel
            active={active}
            activeId={activeId}
            sessions={state.sessions}
            status={state.status}
            replaying={!!replay}
            canAside={canAside}
            isProjectSession={isProjectSession}
            showWorkbench={wb.showWorkbench}
            compactLayout={compactLayout}
            strip={strip}
            panelWidth={panel.width}
            sidebarWidth={sidebar.hidden ? 0 : sidebar.width}
            files={ctl.files}
            runs={ctl.runs}
            plan={ctl.plan}
            made={ctl.made}
            summonTab={summonPanelTab}
            drafts={ctl.draftsRef.current}
            providers={providers}
            defaultEffort={state.settings?.settings.effort ?? "high"}
            efforts={state.settings?.efforts ?? ["low", "medium", "high", "xhigh", "max"]}
            contextSetting={state.settings?.settings.context_limit ?? 0}
            contextAuto={!!state.settings?.settings.context_auto}
            asideSeed={ctl.asideSeed}
            onClearAsideSeed={() => ctl.setAsideSeed(null)}
            listedCheck={listedCheck}
            keyedCheck={keyedCheck}
            onSaveLimit={saveLimit}
            onRefreshSessions={refresh}
            onAsideId={(aid) => active && aid && ctl.asideIdsRef.current.set(active.id, aid)}
            onForwardFromAside={handleForwardFromAside}
            onQuoteIntoChat={quoteIntoChat}
            onResize={(width) => setPanel({ ...panel, width })}
            onWiden={() => (setPeek(true), setPanel((cur) => (cur.hidden ? { ...cur, hidden: false } : cur)))}
            onCloseDrawer={() => setDrawerOpen(false)}
            onInspectFile={isProjectSession ? (relPath) => wb.openWorkbenchFile(relPath, false) : undefined}
            onOpenFile={ctl.openFile}
            onRestoreArtifact={ctl.restoreArtifact}
            onSaveArtifactCopy={ctl.saveArtifactCopy}
            onError={fail}
          />
        </Suspense>
      )}
    </div>
  );
}

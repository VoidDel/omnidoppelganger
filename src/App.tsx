import { type CSSProperties, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { AppLeftSidebar, PANEL_STRIP_WIDTH, WorkspaceFilesPane, WorkspaceGitPane, normalizeQuote } from "./components/AppPanes";
import type { QuotePayload } from "./components/Composer";
import { HomeEmptyStage, SecondaryPageStage, buildJumpActs } from "./components/EmptyStage";
import { Guard, lazyWithRetry } from "./components/Guard";
import { Jump } from "./components/Jump";
import type { PanelTab } from "./components/SidePanel";
import type { SidebarTab } from "./components/Sidebar";
import type { SettingsTab } from "./components/Settings";
import { StatusBanners } from "./components/StatusBanners";
import { useCompactLayout, useGlobalShortcuts, usePanel, useSidebar, useTheme } from "./hooks/useGlobalShortcuts";
import { useSessionController } from "./hooks/useSessionController";
import { reviewTaskPrompt, useSlashCommands } from "./hooks/useSlashCommands";
import { useWorkbenchState } from "./hooks/useWorkbenchState";
import { useWorkspaceGitSync } from "./hooks/useWorkspaceGitSync";
import { formatMessage, I18nProvider, resolveLocale } from "./i18n";
import { api } from "./lib/api";
import { forwarded } from "./lib/aside";
import { hydratePriceCatalog } from "./lib/pricing";
import { isProviderEnabled, isProviderKeyed } from "./lib/providers";

const ActiveSessionStage = lazyWithRetry(() =>
  import("./components/ActiveSessionStage").then((m) => ({ default: m.ActiveSessionStage })),
);
const AppRightPanel = lazyWithRetry(() =>
  import("./components/ActiveSessionStage").then((m) => ({ default: m.AppRightPanel })),
);

export default function App() {
  const ctl = useSessionController();
  const { state, dispatch, fail, refresh, refreshSettings, saved, scheduled, active, activeId, replay, setReplay } = ctl;
  const [jumping, setJumping] = useState<"all" | "files" | null>(null);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("model");
  const locale = resolveLocale(state.settings?.settings.locale);
  useTheme(state.settings?.settings.theme);

  useEffect(() => {
    api.getPriceCatalog().then(hydratePriceCatalog).catch(() => undefined);
  }, []);

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
  const drawerInert = compactLayout && drawerOpen && showPanel;
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
    (input: QuotePayload) => {
      if (wb.showWorkbench) wb.setInWorkbench(false);
      ctl.setSeed({ id: Date.now(), quote: normalizeQuote(input, locale) });
    },
    [wb.showWorkbench, wb.setInWorkbench, ctl.setSeed, locale],
  );
  const quoteIntoAside = useCallback(
    (input: QuotePayload) => {
      if (!canAside) return;
      ctl.setAsideSeed({ id: Date.now(), quote: normalizeQuote(input, locale) });
      summonAside();
    },
    [canAside, summonAside, ctl.setAsideSeed, locale],
  );
  const quoteIntoChat = useCallback(
    (input: QuotePayload) => (wb.showWorkbench && canAside ? quoteIntoAside(input) : quoteIntoMain(input)),
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
  const attachImageIntoMain = useCallback(
    (image: import("./lib/types").Image) => {
      if (wb.showWorkbench) wb.setInWorkbench(false);
      ctl.setSeed({ id: Date.now(), attachImages: [image] });
    },
    [wb.showWorkbench, wb.setInWorkbench, ctl.setSeed],
  );
  const attachImageIntoAside = useCallback(
    (image: import("./lib/types").Image) => {
      if (!canAside) return;
      ctl.setAsideSeed({ id: Date.now(), attachImages: [image] });
      summonAside();
    },
    [canAside, summonAside, ctl.setAsideSeed],
  );

  const handleForwardFromAside = useCallback(
    (text: string) => {
      ctl.send(forwarded(text, locale));
      if (ctl.stateRef.current.status !== "idle") {
        dispatch({ type: "note", note: formatMessage(locale, "aside.forwardedQueued") });
      } else if (showWorkbenchRef.current) {
        dispatch({ type: "note", note: formatMessage(locale, "aside.forwardedIdle") });
      }
    },
    [ctl.send, ctl.stateRef, dispatch, locale],
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
  const toMemory = useCallback(() => (setSettingsTab("memory"), refreshSettings(), dispatch({ type: "page", page: "settings" })), [refreshSettings, dispatch]);
  const toMcp = useCallback(() => (setSettingsTab("mcp"), refreshSettings(), dispatch({ type: "page", page: "settings" })), [refreshSettings, dispatch]);
  const toSkills = useCallback(() => (setSettingsTab("skills"), refreshSettings(), dispatch({ type: "page", page: "settings" })), [refreshSettings, dispatch]);
  const showSidebarTab = useCallback(
    (tab: SidebarTab) => {
      if (tab === "files") {
        summonPanel("workspace_files");
        return;
      }
      if (tab === "git") {
        summonPanel("git");
        return;
      }
      dispatch({ type: "page", page: "chat" });
      setSidebar((cur) => ({ ...cur, hidden: false }));
      setLeftTab(tab);
    },
    [dispatch, setSidebar, summonPanel],
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
    toMemory,
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
    <I18nProvider locale={locale}>
      <div
        className={["app", showPanel && "with-panel", sidebar.hidden && "sidebar-hidden"].filter(Boolean).join(" ")}
        style={{ "--sidebar-width": `${sidebar.width}px`, "--panel-width": `${strip ? PANEL_STRIP_WIDTH : panel.width}px` } as CSSProperties}
      >
        {!sidebar.hidden && (
          <AppLeftSidebar
            inert={drawerInert}
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
            onAskReview={() => (ctl.send(reviewTaskPrompt(locale)), wb.showWorkbench && wb.setInWorkbench(false))}
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
        <main className="main" inert={drawerInert || undefined}>
          <div className="stage-drag" data-tauri-drag-region />
          {!(inChat && active) && banners}
          {!inChat ? (
            <SecondaryPageStage
              page={state.page}
              settingsTab={settingsTab}
              onSettingsTabChange={setSettingsTab}
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
                onAttachImageIntoMain={attachImageIntoMain}
                onAttachImageIntoAside={attachImageIntoAside}
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
          <Guard name={formatMessage(locale, "guard.jump")} resetKey={jumping} onDismiss={() => setJumping(null)}>
            <Jump
              mode={jumping}
              sessions={state.sessions}
              acts={buildJumpActs({ isProjectSession, active, goHome: ctl.goHome, create: ctl.create, chat: ctl.chat, openLeftTab: showSidebarTab, summonAside, summonTerminal, toSkills, toMcp, toSchedules, toSettings }, locale)}
              onOpen={ctl.open}
              onSearch={api.searchSessions}
              onSearchFiles={isProjectSession && activeId ? (q) => api.searchWorkspaceFiles(activeId, q, 60) : undefined}
              onOpenFile={isProjectSession ? (relPath, line) => (dispatch({ type: "page", page: "chat" }), wb.openWorkbenchFile(relPath, false, line)) : undefined}
              onClose={() => setJumping(null)}
            />
          </Guard>
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
              workspaceFilesPane={
                isProjectSession && active ? (
                  <WorkspaceFilesPane
                    active={active}
                    activeWbFile={wb.activeWbFile}
                    fsRefreshTick={git.fsRefreshTick}
                    gitBadges={git.gitBadges}
                    canAside={canAside}
                    onOpenFile={wb.openWorkbenchFile}
                    onAttachMain={attachIntoMain}
                    onAttachAside={attachIntoAside}
                    onDeletedPath={wb.handleDeletedWorkspacePath}
                    onRenamedPath={wb.handleRenamedWorkspacePath}
                    onBumpRefresh={git.bumpRefresh}
                    onError={fail}
                  />
                ) : undefined
              }
              gitPane={
                isProjectSession && active ? (
                  <WorkspaceGitPane
                    active={active}
                    status={state.status}
                    gitState={git.gitState}
                    gitRefreshTick={git.gitRefreshTick}
                    onSelectGitRepo={git.setSelectedGitRepo}
                    onSetGitState={git.setGitState}
                    onBumpRefresh={git.bumpRefresh}
                    onSessionUpdated={(item) => dispatch({ type: "session", item })}
                    onOpenDiff={wb.openWorkbenchDiff}
                    onOpenFile={wb.openWorkbenchFile}
                    onAskReview={() => (ctl.send(reviewTaskPrompt(locale)), wb.showWorkbench && wb.setInWorkbench(false))}
                    onNote={(note) => dispatch({ type: "note", note })}
                    onError={fail}
                  />
                ) : undefined
              }
              gitCount={git.gitChangeCount}
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
    </I18nProvider>
  );
}

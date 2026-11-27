import { Suspense } from "react";
import { formatMessage, getCurrentLocale, type Locale, useI18n } from "../i18n";
import { api } from "../lib/api";
import { modeHintFor, modeOf } from "../lib/modes";
import { cliOf } from "../lib/providers";
import type { State } from "../lib/sessionState";
import type { Provider, Schedule, SessionItem, SettingsView } from "../lib/types";
import { Logo, Mountains } from "./Backdrop";
import { Combo } from "./Combo";
import { ConfirmBubble } from "./ConfirmBubble";
import { Desk } from "./Desk";
import { FormDialog } from "./FormDialog";
import { Guard, lazyWithRetry } from "./Guard";
import { Info } from "./Hint";
import type { SidebarTab } from "./Sidebar";
import type { SettingsTab } from "./Settings";
import { CHAT_ICON } from "./Sidebar";
import { sameDiffView, sameFileView } from "../lib/refresh";

export { Combo, ConfirmBubble, FormDialog, Info, sameDiffView, sameFileView };

const SettingsPage = lazyWithRetry(() => import("./Settings").then((m) => ({ default: m.SettingsPage })));
const McpPage = lazyWithRetry(() => import("./Mcp").then((m) => ({ default: m.McpPage })));
const SkillsPage = lazyWithRetry(() => import("./Skills").then((m) => ({ default: m.SkillsPage })));
const SchedulesPage = lazyWithRetry(() => import("./Schedules").then((m) => ({ default: m.SchedulesPage })));

export interface SecondaryPageStageProps {
  page: State["page"];
  settingsTab?: SettingsTab;
  onSettingsTabChange?: (tab: SettingsTab) => void;
  settings: SettingsView | null;
  schedules: Schedule[];
  sessions: SessionItem[];
  active: SessionItem | undefined;
  saved: (req: Promise<SettingsView>) => Promise<unknown>;
  scheduled: (req: Promise<Schedule[]>) => Promise<string | null>;
  refreshSettings: () => void;
  onSettingsLoaded: (settings: SettingsView) => void;
  onSchedulesLoaded: (schedules: Schedule[]) => void;
  onOpenSession: (id: string) => void;
  onCleanSessions: (ids: string[]) => void;
  onError: (error: unknown) => void;
}

export function SecondaryPageStage({
  page,
  settingsTab,
  onSettingsTabChange,
  settings,
  schedules,
  sessions,
  active,
  saved,
  scheduled,
  refreshSettings,
  onSettingsLoaded,
  onSchedulesLoaded,
  onOpenSession,
  onCleanSessions,
  onError,
}: SecondaryPageStageProps) {
  const { t } = useI18n();
  if (page === "settings" && settings) {
    return (
      <Guard name={t("guard.settings")} className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
          <SettingsPage
            view={settings}
            onSave={(s) => saved(api.saveSettings(s))}
            onSaveKey={(provider, key) => saved(api.saveApiKey(provider, key))}
            onAddProvider={(kind, name) =>
              api.addProvider(kind, name).then(
                (added) => (onSettingsLoaded(added.view), added),
                (e) => (onError(e), null),
              )
            }
            onRemoveProvider={(provider) => saved(api.removeProvider(provider))}
            onRevoke={(workspace, grant) => saved(api.revokeGrant(workspace, grant))}
            onSaveMcp={(cfg) => api.saveMcpConfig(cfg).then((s) => (onSettingsLoaded(s), null), (e) => String(e))}
            initialTab={settingsTab}
            onTabChange={onSettingsTabChange}
            onRefresh={refreshSettings}
            listModels={api.listModels}
          />
        </Suspense>
      </Guard>
    );
  }
  if (page === "mcp" && settings) {
    return (
      <Guard name={t("guard.mcp")} className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
          <McpPage
            config={settings.mcp_config}
            servers={settings.mcp_servers}
            home={settings.home}
            settingsView={settings}
            onRefresh={refreshSettings}
            onSave={(cfg) => api.saveMcpConfig(cfg).then((s) => (onSettingsLoaded(s), null), (e) => String(e))}
          />
        </Suspense>
      </Guard>
    );
  }
  if (page === "skills") {
    return (
      <Guard name={t("guard.skills")} className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
          <SkillsPage onChanged={refreshSettings} />
        </Suspense>
      </Guard>
    );
  }
  if (page === "schedules") {
    return (
      <Guard name={t("guard.schedules")} className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
          <SchedulesPage
            schedules={schedules}
            sessions={sessions}
            mode={settings?.settings.mode ?? "edit"}
            workspace={active ? (active.worktree?.origin ?? active.workspace) : (sessions.find((s) => !s.parent)?.workspace ?? "")}
            onOpen={onOpenSession}
            onPick={api.pickWorkspace}
            onSave={(schedule) => scheduled(api.saveSchedule(schedule))}
            onDelete={(id) => scheduled(api.deleteSchedule(id))}
            onRun={(id) =>
              api.runSchedule(id).then(
                (list) => {
                  onSchedulesLoaded(list);
                  const lastRun = list.find((s) => s.id === id)?.last;
                  if (lastRun?.session && !lastRun.error) onOpenSession(lastRun.session);
                  return lastRun?.error ?? null;
                },
                (e) => String(e),
              )
            }
            onCleanSessions={onCleanSessions}
          />
        </Suspense>
      </Guard>
    );
  }
  return null;
}

export interface HomeEmptyStageProps {
  missingKey: boolean;
  defaultProviderName?: string;
  sessions: SessionItem[];
  schedules: Schedule[];
  onCreate: () => void;
  onChat: () => void;
  onOpenSettings: () => void;
  onOpenSession: (id: string) => void;
  onOpenSchedules: () => void;
}

export function HomeEmptyStage({
  missingKey,
  defaultProviderName,
  sessions,
  schedules,
  onCreate,
  onChat,
  onOpenSettings,
  onOpenSession,
  onOpenSchedules,
}: HomeEmptyStageProps) {
  const { t } = useI18n();
  return (
    <>
      <Mountains />
      <div className="empty">
        <Logo size={64} alive />
        <h1>{t("home.title")}</h1>
        <p>{t("home.lead")}</p>
        {missingKey && <p className="muted">{t("home.missingKey", { provider: defaultProviderName ?? "" })}</p>}
        <div className="actions">
          <button type="button" className="button primary" onClick={onCreate}>
            {t("home.pickWorkspace")}
          </button>
          <button type="button" className="button outline" onClick={onChat}>
            {CHAT_ICON}
            {t("home.startChat")}
          </button>
          {missingKey && (
            <button type="button" className="button outline" onClick={onOpenSettings}>
              {t("home.fillKey")}
            </button>
          )}
        </div>
        <Desk sessions={sessions} schedules={schedules} onOpen={onOpenSession} onSchedules={onOpenSchedules} />
      </div>
    </>
  );
}

export interface ChatEmptyStageProps {
  active: SessionItem;
  isChatSession: boolean;
  chatFolder?: string;
  providers: Provider[];
}

export function ChatEmptyStage({ active, isChatSession, chatFolder, providers }: ChatEmptyStageProps) {
  const { locale, t } = useI18n();
  if (isChatSession) {
    return (
      <div className="empty">
        <h1>{t("chatEmpty.chatTitle")}</h1>
        <p>
          {t("chatEmpty.chatBodyPrefix")}
          <span className="mono">{chatFolder}</span>
          {t("chatEmpty.chatBodySuffix")}
        </p>
      </div>
    );
  }
  const activeCli = cliOf(providers.find((known) => known.id === active.provider));
  return (
    <div className="empty">
      <h1>{t("chatEmpty.projectTitle")}</h1>
      <p>
        {t("chatEmpty.projectMode", {
          mode: modeOf(active.mode, locale).name,
          hint: modeHintFor(active.mode, activeCli, locale),
        })}
        {active.mode === "bypass"
          ? t("chatEmpty.bypassNote")
          : activeCli
            ? t("chatEmpty.cliNote")
            : t("chatEmpty.safetyNote")}{" "}
        {t("chatEmpty.shiftTabHint")}
      </p>
    </div>
  );
}

export interface JumpActsOptions {
  isProjectSession: boolean;
  active: SessionItem | undefined;
  goHome: () => void;
  create: () => Promise<void>;
  chat: () => void;
  openLeftTab: (tab: SidebarTab) => void;
  summonAside: () => void;
  summonTerminal: () => void;
  toSkills: () => void;
  toMcp: () => void;
  toSchedules: () => void;
  toSettings: () => void;
}

export function buildJumpActs(
  {
    isProjectSession,
    active,
    goHome,
    create,
    chat,
    openLeftTab,
    summonAside,
    summonTerminal,
    toSkills,
    toMcp,
    toSchedules,
    toSettings,
  }: JumpActsOptions,
  locale: Locale = getCurrentLocale(),
) {
  const tr = (key: Parameters<typeof formatMessage>[1]) => formatMessage(locale, key);
  const enMode = locale === "en";
  return [
    { id: "home", name: tr("jump.actHome"), hint: tr("jump.actHomeHint"), seal: enMode ? "HM" : "山", run: goHome },
    { id: "new", name: tr("jump.actNew"), hint: tr("jump.actNewHint"), seal: enMode ? "WS" : "辟", run: () => void create() },
    { id: "chat", name: tr("jump.actChat"), hint: tr("jump.actChatHint"), seal: enMode ? "CH" : "谈", run: chat },
    ...(isProjectSession
      ? [
          { id: "files", name: tr("jump.actFiles"), hint: tr("jump.actFilesHint"), seal: enMode ? "FL" : "卷", run: () => openLeftTab("files") },
          { id: "git", name: tr("jump.actGit"), hint: tr("jump.actGitHint"), seal: enMode ? "GT" : "版", run: () => openLeftTab("git") },
        ]
      : []),
    ...(active && !active.parent
      ? [{ id: "aside", name: tr("jump.actAside"), hint: tr("jump.actAsideHint"), seal: enMode ? "AS" : "旁", run: summonAside }]
      : []),
    ...(active
      ? [{ id: "terminal", name: tr("jump.actTerminal"), hint: tr("jump.actTerminalHint"), seal: enMode ? "SH" : "端", run: summonTerminal }]
      : []),
    { id: "skills", name: tr("jump.actSkills"), hint: tr("jump.actSkillsHint"), seal: enMode ? "SK" : "籍", run: toSkills },
    { id: "mcp", name: tr("jump.actMcp"), hint: tr("jump.actMcpHint"), seal: enMode ? "MC" : "器", run: toMcp },
    { id: "schedules", name: tr("jump.actSchedules"), hint: tr("jump.actSchedulesHint"), seal: enMode ? "CR" : "课", run: toSchedules },
    { id: "settings", name: tr("jump.actSettings"), hint: tr("jump.actSettingsHint"), seal: enMode ? "ST" : "设", run: toSettings },
  ];
}

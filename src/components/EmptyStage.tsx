import { Suspense, lazy } from "react";
import { api } from "../lib/api";
import { modeHintFor, modeOf } from "../lib/modes";
import { cliOf } from "../lib/providers";
import type { State } from "../lib/sessionState";
import type { Provider, Schedule, SessionItem, SettingsView } from "../lib/types";
import { Logo, Mountains } from "./Backdrop";
import { Desk } from "./Desk";
import { Guard } from "./Guard";
import type { SidebarTab } from "./Sidebar";
import { CHAT_ICON } from "./Sidebar";

const SettingsPage = lazy(() => import("./Settings").then((m) => ({ default: m.SettingsPage })));
const McpPage = lazy(() => import("./Mcp").then((m) => ({ default: m.McpPage })));
const SkillsPage = lazy(() => import("./Skills").then((m) => ({ default: m.SkillsPage })));
const SchedulesPage = lazy(() => import("./Schedules").then((m) => ({ default: m.SchedulesPage })));

export interface SecondaryPageStageProps {
  page: State["page"];
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
  if (page === "settings" && settings) {
    return (
      <Guard name="设置" className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">正在加载设置…</div>}>
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
            onRefresh={refreshSettings}
            listModels={api.listModels}
          />
        </Suspense>
      </Guard>
    );
  }
  if (page === "mcp" && settings) {
    return (
      <Guard name="法器" className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">正在加载法器…</div>}>
          <McpPage
            config={settings.mcp_config}
            servers={settings.mcp_servers}
            home={settings.home}
            onRefresh={refreshSettings}
            onSave={(cfg) => api.saveMcpConfig(cfg).then((s) => (onSettingsLoaded(s), null), (e) => String(e))}
          />
        </Suspense>
      </Guard>
    );
  }
  if (page === "skills") {
    return (
      <Guard name="秘籍" className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">正在加载秘籍…</div>}>
          <SkillsPage onChanged={refreshSettings} />
        </Suspense>
      </Guard>
    );
  }
  if (page === "schedules") {
    return (
      <Guard name="功课" className="fill" resetKey={page}>
        <Suspense fallback={<div className="viewer-placeholder muted">正在加载功课…</div>}>
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
  return (
    <>
      <Mountains />
      <div className="empty">
        <Logo size={64} alive />
        <h1>选一处洞府，化出分身来干活</h1>
        <p>白纸是本尊的法旨，墨迹是分身的手笔，见黄符朱砂处候本尊定夺。</p>
        {missingKey && <p className="muted">还没有 {defaultProviderName} 的密钥，分身出不了关，先到设置里填上。</p>}
        <div className="actions">
          <button type="button" className="button primary" onClick={onCreate}>
            选文件夹作洞府，化出分身
          </button>
          <button type="button" className="button outline" onClick={onChat}>
            {CHAT_ICON}
            不选文件夹，直接闲谈
          </button>
          {missingKey && (
            <button type="button" className="button outline" onClick={onOpenSettings}>
              去填密钥
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
  if (isChatSession) {
    return (
      <div className="empty">
        <h1>有话直说</h1>
        <p>
          这是闲谈，不绑哪个项目：问什么答什么，要查的它会上网查，附上的文件它会读。它写出的文件放在{" "}
          <span className="mono">{chatFolder}</span>。要它在某个项目里干活，就用「开辟洞府」选那个文件夹。
        </p>
      </div>
    );
  }
  const activeCli = cliOf(providers.find((known) => known.id === active.provider));
  return (
    <div className="empty">
      <h1>分身已入驻这处洞府</h1>
      <p>
        吩咐一件事吧。眼下的禁制是「{modeOf(active.mode).name}」：{modeHintFor(active.mode, activeCli)}。
        {active.mode === "bypass"
          ? "它不会停下来请示任何事，碰洞府之外的东西、改 .git 也一样。"
          : activeCli
            ? "当前心法由本机 CLI 无头运行，不会弹出逐条审批卡片，权限直接按对应档位托管。"
            : "碰这个文件夹之外的东西、改 .git 里的文件，无论哪种禁制都要先请本尊定夺。"}
        输入框里按 Shift+Tab 换禁制。
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

export function buildJumpActs({
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
}: JumpActsOptions) {
  return [
    { id: "home", name: "首页", hint: "回到山前：候旨的、运功的、将至的功课", seal: "山", run: goHome },
    { id: "new", name: "开辟洞府", hint: "选一个文件夹，化出分身", seal: "辟", run: () => void create() },
    { id: "chat", name: "闲谈", hint: "不选文件夹，直接和分身说话", seal: "谈", run: chat },
    ...(isProjectSession
      ? [
          { id: "files", name: "文件树", hint: "在左栏查看与管理洞府里的文件", seal: "卷", run: () => openLeftTab("files") },
          { id: "git", name: "Git 管理", hint: "在左栏暂存、提交、切换分支、拉取推送", seal: "版", run: () => openLeftTab("git") },
        ]
      : []),
    ...(active && !active.parent
      ? [{ id: "aside", name: "旁聊", hint: "不打断主分身，在右侧栏另开只读对话问进展、查代码", seal: "旁", run: summonAside }]
      : []),
    ...(active
      ? [{ id: "terminal", name: "运行终端", hint: "打开右侧交互式本地终端 (Ctrl/Cmd+J)", seal: "端", run: summonTerminal }]
      : []),
    { id: "skills", name: "秘籍", hint: "随程序带来的做事说明，打开才收入；还有自己的", seal: "籍", run: toSkills },
    { id: "mcp", name: "法器", hint: "给分身添上本事的 MCP 服务：查文档、搜网页、操控浏览器", seal: "器", run: toMcp },
    { id: "schedules", name: "功课", hint: "到了时辰自行化出分身去办的事", seal: "课", run: toSchedules },
    { id: "settings", name: "设置", hint: "心法、禁制、神通、外观", seal: "设", run: toSettings },
  ];
}

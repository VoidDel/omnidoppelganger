import { useEffect, useMemo, useRef, useState } from "react";
import type { SidebarTab } from "../components/Sidebar";
import { formatMessage, getCurrentLocale, type Locale, useI18n } from "../i18n";
import { api } from "../lib/api";
import type { SlashCommand } from "../lib/slash";
import type { Envelope, PromptTemplateInfo, SessionItem, Skill, Status } from "../lib/types";

export function initTaskPrompt(locale: Locale = getCurrentLocale()): string {
  return formatMessage(locale, "prompt.init");
}

export function reviewTaskPrompt(locale: Locale = getCurrentLocale()): string {
  return formatMessage(locale, "prompt.review");
}

/** Default `zh-CN` "/init" prompt constant preserved for compatibility. */
export const INIT_TASK = initTaskPrompt("zh-CN");

/** Default `zh-CN` "/review" prompt constant preserved for compatibility. */
export const REVIEW_TASK = reviewTaskPrompt("zh-CN");

export interface UseSlashCommandsOptions {
  active: SessionItem | undefined;
  isChatSession: boolean;
  status: Status;
  events: Envelope[];
  settingsSkills: Skill[] | undefined;
  hasRows: boolean;
  hasStops: boolean;
  fsRefreshTick: number;
  createIn: (workspace: string, isolated?: boolean) => void;
  summonAside: () => void;
  summonTerminal: () => void;
  summonUsage?: () => void;
  forkFrom: (turn?: number, text?: string) => void;
  exportSession: (id: string, title: string) => void;
  send: (text: string) => void;
  showSidebarTab: (tab: SidebarTab) => void;
  startReplay: () => void;
  toMcp: () => void;
  toSkills: () => void;
  toSchedules: () => void;
  toSettings: () => void;
  onSessionUpdated: (item: SessionItem) => void;
  onNote: (note: string) => void;
  onError: (error: unknown) => void;
}

export function useSlashCommands({
  active,
  isChatSession,
  status,
  events,
  settingsSkills,
  hasRows,
  hasStops,
  fsRefreshTick,
  createIn,
  summonAside,
  summonTerminal,
  summonUsage,
  forkFrom,
  exportSession,
  send,
  showSidebarTab,
  startReplay,
  toMcp,
  toSkills,
  toSchedules,
  toSettings,
  onSessionUpdated,
  onNote,
  onError,
}: UseSlashCommandsOptions): SlashCommand[] {
  const { locale } = useI18n();
  const [promptTemplates, setPromptTemplates] = useState<PromptTemplateInfo[]>([]);
  useEffect(() => {
    let cancelled = false;
    api.listPromptTemplates(active?.id ?? null).then(
      (items) => {
        if (!cancelled) setPromptTemplates(items);
      },
      () => {
        if (!cancelled) setPromptTemplates([]);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [active?.id, active?.workspace, active?.project_trusted, fsRefreshTick]);

  const listedSkills = useMemo(() => {
    for (let at = events.length - 1; at >= 0; at--) {
      const event = events[at];
      if (event.type === "skills/listed") return event.data.skills;
    }
    return null;
  }, [events]);

  const actionsRef = useRef({
    createIn,
    summonAside,
    summonTerminal,
    summonUsage,
    forkFrom,
    exportSession,
    send,
    showSidebarTab,
    startReplay,
    toMcp,
    toSkills,
    toSchedules,
    toSettings,
    onSessionUpdated,
    onNote,
    onError,
  });
  actionsRef.current = {
    createIn,
    summonAside,
    summonTerminal,
    summonUsage,
    forkFrom,
    exportSession,
    send,
    showSidebarTab,
    startReplay,
    toMcp,
    toSkills,
    toSchedules,
    toSettings,
    onSessionUpdated,
    onNote,
    onError,
  };

  const activeId = active?.id ?? null;
  const activeTitle = active?.title ?? "";
  const activeWorkspace = active?.workspace ?? "";
  const activeOrigin = active?.worktree?.origin ?? activeWorkspace;
  const activeParent = active?.parent ?? null;
  const projectTrusted = Boolean(active?.project_trusted);
  const busy = status !== "idle";

  return useMemo<SlashCommand[]>(() => {
    if (!activeId) return [];
    const tr = (key: Parameters<typeof formatMessage>[1], params?: Record<string, string | number>) =>
      formatMessage(locale, key, params);
    const working = busy ? tr("slash.working") : undefined;
    const builtIn: SlashCommand[] = [
      {
        name: "new",
        label: tr("slash.new.label"),
        aliases: ["新分身", "化出", "新建会话"],
        hint: tr("slash.new.hint"),
        run: () => actionsRef.current.createIn(activeOrigin, false),
      },
      {
        name: "aside",
        label: tr("slash.aside.label"),
        aliases: ["旁聊", "侧聊", "侧边问答"],
        hint: tr("slash.aside.hint"),
        unavailable: activeParent ? tr("slash.aside.unavail") : undefined,
        run: () => actionsRef.current.summonAside(),
      },
      {
        name: "fork",
        label: tr("slash.fork.label"),
        aliases: ["分叉"],
        hint: tr("slash.fork.hint"),
        unavailable: activeParent ? tr("slash.fork.unavail") : undefined,
        run: () => actionsRef.current.forkFrom(),
      },
      {
        name: "export",
        label: tr("slash.export.label"),
        aliases: ["导出", "导出会话"],
        hint: tr("slash.export.hint"),
        unavailable: !hasRows ? tr("slash.export.unavail") : undefined,
        run: () => actionsRef.current.exportSession(activeId, activeTitle),
      },
      {
        name: "usage",
        label: tr("slash.usage.label"),
        aliases: ["用量", "额度", "灵石"],
        hint: tr("slash.usage.hint"),
        run: () => (actionsRef.current.summonUsage ? actionsRef.current.summonUsage() : actionsRef.current.toSettings()),
      },
      ...(!isChatSession
        ? [
            {
              name: "trust",
              label: projectTrusted ? tr("slash.trust.revoke") : tr("slash.trust.grant"),
              aliases: ["信任", "授信"],
              hint: projectTrusted ? tr("slash.trust.revokeHint") : tr("slash.trust.grantHint"),
              run: () => {
                const next = !projectTrusted;
                api.setWorkspaceTrust(activeId, next).then(
                  (item) => {
                    actionsRef.current.onSessionUpdated(item);
                    actionsRef.current.onNote(next ? tr("slash.trust.trustedNote") : tr("slash.trust.revokedNote"));
                  },
                  (err) => actionsRef.current.onError(err),
                );
              },
            },
          ]
        : []),
      {
        name: "files",
        label: tr("slash.files.label"),
        aliases: ["文件", "目录"],
        hint: tr("slash.files.hint"),
        run: () => actionsRef.current.showSidebarTab("files"),
      },
      {
        name: "git",
        label: tr("slash.git.label"),
        aliases: ["版本", "仓库"],
        hint: tr("slash.git.hint"),
        run: () => actionsRef.current.showSidebarTab("git"),
      },
      {
        name: "terminal",
        label: tr("slash.terminal.label"),
        aliases: ["终端", "命令行"],
        hint: tr("slash.terminal.hint"),
        run: () => actionsRef.current.summonTerminal(),
      },
      {
        name: "rename",
        label: tr("slash.rename.label"),
        aliases: ["改名", "重命名"],
        hint: tr("slash.rename.hint"),
        arg: tr("slash.rename.arg"),
        run: (title) => {
          api.renameSession(activeId, title).then(
            (item) => actionsRef.current.onSessionUpdated(item),
            (err) => actionsRef.current.onError(err),
          );
        },
      },
      {
        name: "replay",
        label: tr("slash.replay.label"),
        aliases: ["回溯", "回放"],
        hint: tr("slash.replay.hint"),
        unavailable: !hasStops ? tr("slash.replay.unavail") : undefined,
        run: () => actionsRef.current.startReplay(),
      },
      {
        name: "init",
        label: tr("slash.init.label"),
        aliases: ["立规矩", "规矩"],
        hint: tr("slash.init.hint"),
        unavailable: working,
        run: () => actionsRef.current.send(initTaskPrompt(locale)),
      },
      {
        name: "review",
        label: tr("slash.review.label"),
        aliases: ["审查"],
        hint: tr("slash.review.hint"),
        unavailable: working,
        run: () => actionsRef.current.send(reviewTaskPrompt(locale)),
      },
      { name: "mcp", label: tr("slash.mcp.label"), aliases: ["法器"], hint: tr("slash.mcp.hint"), run: () => actionsRef.current.toMcp() },
      { name: "skills", label: tr("slash.skills.label"), aliases: ["秘籍", "技能"], hint: tr("slash.skills.hint"), run: () => actionsRef.current.toSkills() },
      { name: "schedule", label: tr("slash.schedule.label"), aliases: ["功课", "定时任务"], hint: tr("slash.schedule.hint"), run: () => actionsRef.current.toSchedules() },
      { name: "settings", label: tr("slash.settings.label"), aliases: ["设置"], hint: tr("slash.settings.hint"), run: () => actionsRef.current.toSettings() },
    ];

    const taken = new Set([
      ...builtIn.map((command) => command.name.toLowerCase()),
      "compact",
      "model",
      "effort",
      "mode",
      "usage",
    ]);

    const templates: SlashCommand[] = promptTemplates.map((tpl) => {
      const rawName = tpl.name.trim().replace(/\s+/g, "-");
      const cmdName = taken.has(rawName.toLowerCase()) ? `tpl-${rawName}` : rawName;
      taken.add(cmdName.toLowerCase());
      return {
        name: cmdName,
        label: tpl.scope === "project" ? tr("slash.template.project") : tr("slash.template.user"),
        aliases: [tpl.name],
        hint: tpl.description,
        arg: tpl.argument_hint ?? undefined,
        unavailable: working,
        run: (arg) => {
          api.expandPromptTemplate(activeId, tpl.name, arg).then(
            (expanded) => {
              if (expanded.trim()) actionsRef.current.send(expanded);
            },
            (err) => actionsRef.current.onError(err),
          );
        },
      };
    });

    const skills: SlashCommand[] = (listedSkills ?? settingsSkills ?? []).map((skill) => {
      const name = skill.name.trim().replace(/\s+/g, "-");
      const cmdName = taken.has(name.toLowerCase()) ? `skill-${name}` : name;
      taken.add(cmdName.toLowerCase());
      return {
        name: cmdName,
        label: tr("slash.skills.label"),
        aliases: [skill.name],
        hint: skill.description,
        arg: tr("slash.skill.arg"),
        unavailable: working,
        run: (task) =>
          actionsRef.current.send(
            task
              ? tr("slash.skill.runWithTask", { name: skill.name, task })
              : tr("slash.skill.runNoTask", { name: skill.name }),
          ),
      };
    });

    const fitting = isChatSession
      ? builtIn.filter((command) => !["init", "review", "files", "git"].includes(command.name))
      : builtIn;
    return [...fitting, ...templates, ...skills];
  }, [
    activeId,
    activeTitle,
    activeOrigin,
    activeParent,
    projectTrusted,
    isChatSession,
    busy,
    hasRows,
    hasStops,
    promptTemplates,
    listedSkills,
    settingsSkills,
    locale,
  ]);
}

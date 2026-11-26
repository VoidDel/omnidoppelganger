import { useEffect, useMemo, useRef, useState } from "react";
import type { SidebarTab } from "../components/Sidebar";
import { api } from "../lib/api";
import type { SlashCommand } from "../lib/slash";
import type { Envelope, PromptTemplateInfo, SessionItem, Skill, Status } from "../lib/types";

/** What "/init" hands the 分身: learn the project, and write down what a 分身 needs to know of it. */
export const INIT_TASK =
  "读懂这个项目，在洞府根目录写一份 AGENTS.md，给以后在这里干活的分身看：项目是做什么的，目录怎么分，怎么构建、测试、运行，代码有哪些约定，哪里容易出错。只写读代码读不出来、或者要读很久才明白的；已经有 AGENTS.md 的话，补上缺的、改掉过时的，别的照旧。";

/** What "/review" hands the 分身: look over what has changed and not been committed, and change nothing. */
export const REVIEW_TASK =
  "审查洞府里还没提交的改动（git diff，连同暂存的）：找出 bug、漏掉的边界情况、和项目约定不符的地方，以及缺了的测试。按严重程度列出，每条写明文件和行号、会出什么问题、怎么改。只看不改。";

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
    const working = busy ? "分身运功时用不了，等它收功，或先收回它" : undefined;
    const builtIn: SlashCommand[] = [
      {
        name: "new",
        label: "新分身",
        aliases: ["新分身", "化出"],
        hint: "在这处洞府再化出一个分身",
        run: () => actionsRef.current.createIn(activeOrigin, false),
      },
      {
        name: "aside",
        label: "旁聊",
        aliases: ["旁聊", "侧聊"],
        hint: "在右侧栏打开只读旁聊：不打断主分身，随时问进展、查代码",
        unavailable: activeParent ? "子分身的会话没有独立旁聊" : undefined,
        run: () => actionsRef.current.summonAside(),
      },
      {
        name: "fork",
        label: "分叉",
        aliases: ["分叉"],
        hint: "分叉：另起一个分身，带着到现在的全部对话",
        unavailable: activeParent ? "子分身的会话不能分叉" : undefined,
        run: () => actionsRef.current.forkFrom(),
      },
      {
        name: "export",
        label: "导出",
        aliases: ["导出", "导出会话"],
        hint: "将当前会话导出为 Markdown 文件（.md）",
        unavailable: !hasRows ? "还没有对话记录可导出" : undefined,
        run: () => actionsRef.current.exportSession(activeId, activeTitle),
      },
      ...(!isChatSession
        ? [
            {
              name: "trust",
              label: projectTrusted ? "收回洞府授信" : "信任洞府",
              aliases: ["信任", "授信"],
              hint: projectTrusted
                ? "收回对此洞府的授信，下一轮起不再自动读取其中的 AGENTS.md 与 .odg 配置"
                : "信任此洞府，允许分身读取其中的 AGENTS.md、.odg/skills 与项目模板",
              run: () => {
                const next = !projectTrusted;
                api.setWorkspaceTrust(activeId, next).then(
                  (item) => {
                    actionsRef.current.onSessionUpdated(item);
                    actionsRef.current.onNote(
                      next
                        ? "已信任此洞府，项目规矩与秘籍将在下一轮起加载。"
                        : "已收回此洞府授信，下一轮起不再加载项目规矩与秘籍。",
                    );
                  },
                  (err) => actionsRef.current.onError(err),
                );
              },
            },
          ]
        : []),
      {
        name: "files",
        label: "文件",
        aliases: ["文件", "目录"],
        hint: "在左侧栏打开洞府文件树管理",
        run: () => actionsRef.current.showSidebarTab("files"),
      },
      {
        name: "git",
        label: "Git",
        aliases: ["版本", "仓库"],
        hint: "在左侧栏打开 Git 变更、提交与分支管理",
        run: () => actionsRef.current.showSidebarTab("git"),
      },
      {
        name: "terminal",
        label: "终端",
        aliases: ["终端", "命令行"],
        hint: "打开右侧交互式运行终端 (Ctrl/Cmd+J)",
        run: () => actionsRef.current.summonTerminal(),
      },
      {
        name: "rename",
        label: "改名",
        aliases: ["改名"],
        hint: "给这个分身改名；不写名字就用第一句话作名",
        arg: "<新名字>",
        run: (title) => {
          api.renameSession(activeId, title).then(
            (item) => actionsRef.current.onSessionUpdated(item),
            (err) => actionsRef.current.onError(err),
          );
        },
      },
      {
        name: "replay",
        label: "回溯",
        aliases: ["回溯"],
        hint: "回溯：从头一步步看这个分身做过什么",
        unavailable: !hasStops ? "这个分身还没做过什么，没有可回溯的" : undefined,
        run: () => actionsRef.current.startReplay(),
      },
      {
        name: "init",
        label: "立规矩",
        aliases: ["立规矩", "规矩"],
        hint: "让分身读懂这个项目，写一份 AGENTS.md",
        unavailable: working,
        run: () => actionsRef.current.send(INIT_TASK),
      },
      {
        name: "review",
        label: "审查",
        aliases: ["审查"],
        hint: "让分身审查还没提交的改动，只看不改",
        unavailable: working,
        run: () => actionsRef.current.send(REVIEW_TASK),
      },
      { name: "mcp", label: "法器", aliases: ["法器"], hint: "打开法器页：MCP 服务", run: () => actionsRef.current.toMcp() },
      { name: "skills", label: "秘籍", aliases: ["秘籍"], hint: "打开秘籍页：收入或收起秘籍", run: () => actionsRef.current.toSkills() },
      { name: "schedule", label: "功课", aliases: ["功课"], hint: "打开功课页：到点自行去办的事", run: () => actionsRef.current.toSchedules() },
      { name: "settings", label: "设置", aliases: ["设置"], hint: "打开设置", run: () => actionsRef.current.toSettings() },
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
        label: tpl.scope === "project" ? "项目模板" : "模板",
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
        label: "秘籍",
        aliases: [skill.name],
        hint: skill.description,
        arg: "<要办的事>",
        unavailable: working,
        run: (task) =>
          actionsRef.current.send(
            task ? `按秘籍「${skill.name}」（skill）办：${task}` : `按秘籍「${skill.name}」（skill）办。`,
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
  ]);
}

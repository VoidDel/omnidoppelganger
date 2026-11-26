import { render, renderHook } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { isModelFree, normalizeListedModel } from "../components/Pickers";
import { initTaskPrompt, reviewTaskPrompt, useSlashCommands } from "../hooks/useSlashCommands";
import { forwarded } from "../lib/aside";
import { localizeBackendError, localizeSkillError, parseBackendError } from "../lib/errorCode";
import { makeMockSessionItem } from "../test/mockApi";
import { en } from "./en";
import { formatMessage, I18nProvider, setCurrentLocale, useI18n } from "./index";
import { zhCN } from "./zh-CN";
import { zhPlain } from "./zh-plain";

function extractPlaceholders(template: string): string[] {
  const matches = [...template.matchAll(/\{([a-zA-Z0-9_]+)\}/g)].map((m) => m[1]);
  return [...new Set(matches)].sort();
}

describe("Compile-time strong i18n dictionaries & decoupling contracts (P2-13)", () => {
  it("keeps 100% identical key sets across zh-CN, zh-plain, and en dictionaries", () => {
    const zhKeys = Object.keys(zhCN).sort();
    const plainKeys = Object.keys(zhPlain).sort();
    const enKeys = Object.keys(en).sort();

    expect(plainKeys).toEqual(zhKeys);
    expect(enKeys).toEqual(zhKeys);
    expect(zhKeys.length).toBeGreaterThan(150);
  });

  it("keeps 100% identical {placeholder} parameter sets across all 3 dictionaries for every key", () => {
    for (const key of Object.keys(zhCN) as (keyof typeof zhCN)[]) {
      const baseVars = extractPlaceholders(zhCN[key]);
      const plainVars = extractPlaceholders(zhPlain[key]);
      const enVars = extractPlaceholders(en[key]);
      expect({ key, vars: plainVars }).toEqual({ key, vars: baseVars });
      expect({ key, vars: enVars }).toEqual({ key, vars: baseVars });
    }
  });

  it("updates document.documentElement.lang via I18nProvider and localizes built-in model prompts", () => {
    function Probe() {
      const { locale, t } = useI18n();
      return createElement("div", { "data-testid": "probe" }, `${locale}:${t("nav.settings")}`);
    }

    const { rerender, getByTestId } = render(
      createElement(I18nProvider, { locale: "en", children: createElement(Probe) }),
    );
    expect(document.documentElement.lang).toBe("en");
    expect(getByTestId("probe").textContent).toBe("en:Settings");

    // Built-in prompts sent to models in English mode contain zero CJK characters
    expect(initTaskPrompt("en")).not.toMatch(/[\u4e00-\u9fff]/);
    expect(reviewTaskPrompt("en")).not.toMatch(/[\u4e00-\u9fff]/);
    expect(forwarded("Check build", "en")).not.toMatch(/[\u4e00-\u9fff]/);
    expect(formatMessage("en", "prompt.carryOut")).toBe("Carry out this plan.");

    rerender(createElement(I18nProvider, { locale: "zh-plain", children: createElement(Probe) }));
    expect(document.documentElement.lang).toBe("zh-CN");
    expect(getByTestId("probe").textContent).toBe("zh-plain:设置");

    setCurrentLocale("zh-CN");
  });

  it("exposes complete slash commands (/usage, /files, /git, /terminal, /init) with Chinese aliases and localized hints", () => {
    const active = makeMockSessionItem();

    const { result } = renderHook(
      () =>
        useSlashCommands({
          active,
          isChatSession: false,
          status: "idle",
          events: [],
          settingsSkills: [],
          hasRows: true,
          hasStops: true,
          fsRefreshTick: 0,
          createIn: vi.fn(),
          summonAside: vi.fn(),
          summonTerminal: vi.fn(),
          forkFrom: vi.fn(),
          exportSession: vi.fn(),
          send: vi.fn(),
          showSidebarTab: vi.fn(),
          startReplay: vi.fn(),
          toMcp: vi.fn(),
          toSkills: vi.fn(),
          toSchedules: vi.fn(),
          toSettings: vi.fn(),
          onSessionUpdated: vi.fn(),
          onNote: vi.fn(),
          onError: vi.fn(),
        }),
      {
        wrapper: ({ children }: { children: ReactNode }) => createElement(I18nProvider, { locale: "en", children }),
      },
    );

    const byName = new Map(result.current.map((cmd) => [cmd.name, cmd]));
    expect(byName.get("usage")?.aliases).toEqual(["用量", "额度", "灵石"]);
    expect(byName.get("files")?.aliases).toEqual(["文件", "目录"]);
    expect(byName.get("git")?.aliases).toEqual(["版本", "仓库"]);
    expect(byName.get("terminal")?.aliases).toEqual(["终端", "命令行"]);
    expect(byName.get("init")?.aliases).toEqual(["立规矩", "规矩"]);
    expect(byName.get("usage")?.label).toBe("Usage");
    expect(byName.get("init")?.hint).not.toMatch(/[\u4e00-\u9fff]/);
    setCurrentLocale("zh-CN");
  });

  it("parses structured backend error codes ([ERR_*] and JSON) and reads explicit ListedModel.free boolean", () => {
    expect(parseBackendError("[ERR_SKILL_EXISTS] Skill foo is already installed")).toMatchObject({
      code: "ERR_SKILL_EXISTS",
    });
    expect(localizeSkillError("[ERR_SKILL_EXISTS] duplicate", "en")).toEqual({
      text: "A skill with this name is already installed.",
      replace: true,
    });
    expect(
      localizeSkillError("[ERR_SKILL_SUBFOLDERS] Skills in the folders below it: pdf, docx. Pass path.", "zh-CN"),
    ).toEqual({
      text: "这里顶上没有 SKILL.md，下面的文件夹里有这几部，选一部收入：",
      choices: ["pdf", "docx"],
    });
    expect(localizeSkillError("[ERR_SKILL_CLONE_TIMEOUT] timed out", "en")).toEqual({
      text: "Cloning took longer than three minutes and was stopped.",
    });
    expect(localizeSkillError(JSON.stringify({ code: "ERR_SKILL_PATH_ESCAPE", message: "escaped" }), "en")).toEqual({
      text: "Skill subfolder path escapes the repository root and was rejected.",
    });
    expect(localizeBackendError("[ERR_WORKTREE_BUSY] another session sharing this worktree is working", "en")).toBe(
      "Another session sharing this worktree is still running. Stop it or wait until it finishes before landing.",
    );
    expect(localizeBackendError("[ERR_WORKTREE_BUSY] busy", "zh-CN")).toBe(
      "共享此独立工作树的分身尚在运功中，请先收回或等它收功后再合入。",
    );

    const structuredFree = normalizeListedModel({ id: "glm-4.5-flash", name: "GLM-4.5 Flash", free: true });
    const structuredPaid = normalizeListedModel({ id: "glm-5", name: "GLM-5 · 免费", free: false });
    expect(isModelFree(structuredFree)).toBe(true);
    // Explicit `free: false` overrides any display label substring!
    expect(isModelFree(structuredPaid)).toBe(false);
  });
});

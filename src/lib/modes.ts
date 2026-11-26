import { formatMessage, getCurrentLocale, type Locale } from "../i18n";
import type { CliBackend, Mode } from "./types";

const MODE_ORDER: Mode[] = ["manual", "edit", "plan", "auto"];

/** The four restrictions (禁制) a 分身 works under, in the order Shift+Tab cycles them (default zh-CN). */
export const MODES: { value: Mode; name: string; hint: string }[] = [
  { value: "manual", name: "步步请示", hint: "改文件、跑命令，每一步都先请本尊定夺" },
  { value: "edit", name: "府内自便", hint: "洞府之内的文件读写自便，跑命令和碰洞府之外的仍要请示" },
  { value: "plan", name: "先谋后动", hint: "只看不改，摸清之后呈上计划，本尊点头再动手" },
  { value: "auto", name: "放手施为", hint: "只读、构建、测试类命令自行放行，有风险的才停下请示" },
];

/**
 * The fifth, which asks about nothing. It is switched on by hand in one session with a worktree of its own, after
 * saying what it means: never a default, never a schedule's, never reached by Shift+Tab.
 */
export const BYPASS = { value: "bypass" as Mode, name: "全权托付", hint: "什么都不请示：任何命令、洞府之外、.git、上网、身外法器，一概自行放行" };

export function modesFor(locale: Locale = getCurrentLocale()): { value: Mode; name: string; hint: string }[] {
  return MODE_ORDER.map((value) => ({
    value,
    name: formatMessage(locale, `mode.${value}.name` as const),
    hint: formatMessage(locale, `mode.${value}.hint` as const),
  }));
}

export function bypassFor(locale: Locale = getCurrentLocale()): { value: Mode; name: string; hint: string } {
  return {
    value: "bypass",
    name: formatMessage(locale, "mode.bypass.name"),
    hint: formatMessage(locale, "mode.bypass.hint"),
  };
}

export const modeOf = (value: Mode | undefined, locale: Locale = getCurrentLocale()) => {
  if (value === "bypass") return bypassFor(locale);
  const list = modesFor(locale);
  return list.find((mode) => mode.value === value) ?? list[1];
};

/** The mode after `value`, for Shift+Tab: one of the four, so it never lands on 全权托付; from that, the first. */
export const nextMode = (value: Mode): Mode => MODES[(MODES.findIndex((mode) => mode.value === value) + 1) % MODES.length].value;

/**
 * Describe what `mode` actually enforces when a session is backed by a headless external CLI (`cli !== null`),
 * where per-step interactive approval cards do not exist.
 */
export function modeHintFor(mode: Mode | undefined, cli: CliBackend | null = null, locale: Locale = getCurrentLocale()): string {
  const resolved = modeOf(mode, locale);
  if (!cli) return resolved.hint;
  switch (resolved.value) {
    case "manual":
      return formatMessage(locale, "mode.cli.manual");
    case "plan":
      return formatMessage(locale, "mode.cli.plan");
    case "edit":
      return cli === "codex"
        ? formatMessage(locale, "mode.cli.editCodex")
        : formatMessage(locale, "mode.cli.editOther");
    case "auto":
      return formatMessage(locale, "mode.cli.auto");
    case "bypass":
      return formatMessage(locale, "mode.cli.bypass");
  }
}

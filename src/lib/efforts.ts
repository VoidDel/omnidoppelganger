// The agent's realm (境界): how hard the model works on each step, named for the stages of cultivation, lowest
// to highest, one for each of the API's five effort levels. Shared by the settings, where the default is set, and
// the composer, where one 分身 is set apart from it.

import { formatMessage, getCurrentLocale, type Locale, type MessageKey } from "../i18n";
import { cliOf } from "./providers";
import type { Provider } from "./types";

export const EFFORT_NAMES: Record<string, string> = { low: "练气", medium: "筑基", high: "金丹", xhigh: "元婴", max: "化神" };

/** What each realm is for, in a few words. */
export const EFFORT_HINTS: Record<string, string> = {
  low: "推演最浅，最快最省，适合查找、小改",
  medium: "稍作推演，日常的活",
  high: "推演得深，多数编码任务用这一档",
  xhigh: "推演更深，难题、大改动",
  max: "推演到底，最慢最费灵石",
};

const KNOWN_EFFORTS = new Set(["low", "medium", "high", "xhigh", "max"]);

export function effortName(level: string, locale: Locale = getCurrentLocale()): string {
  if (!KNOWN_EFFORTS.has(level)) return level;
  return formatMessage(locale, `effort.${level}.name` as MessageKey);
}

export function effortHint(level: string, locale: Locale = getCurrentLocale()): string | undefined {
  if (!KNOWN_EFFORTS.has(level)) return undefined;
  return formatMessage(locale, `effort.${level}.hint` as MessageKey);
}

/** How a provider's effort setting lands, for a note under the realms. */
export function effortNote(provider: Provider | undefined): string {
  if (!provider || provider.protocol === "anthropic" || provider.protocol === "gemini" || provider.protocol === "responses") return "";
  const cli = cliOf(provider);
  if (cli) {
    switch (cli) {
      case "antigravity":
        return "Antigravity 会按当前心法支持的思考档位自动对应模型版本（超出上限时自动落在最高支持档）。";
      case "claude_code":
        return "";
      case "codex":
        return "Codex CLI 最高到元婴（xhigh），化神按元婴算。";
      case "cursor":
      case "grok":
      case "opencode":
        return "这个 CLI 暂不单独传境界参数，按所选心法的默认思考深度运行。";
    }
  }
  if (!("chat" in provider.protocol)) return "";
  switch (provider.protocol.chat) {
    case "deep_seek":
      return "DeepSeek 只分低、高、极三档：筑基和元婴按金丹算。";
    case "kimi":
    case "glm":
      return `${provider.name} 只分低、高、极三档：筑基按金丹算，元婴按化神算。`;
    default:
      return "这个服务不一定认境界，所以不发送，按服务自己的默认推演。";
  }
}

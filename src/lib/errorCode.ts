import { formatMessage, getCurrentLocale, type Locale } from "../i18n";

export interface ParsedBackendError {
  code: string | null;
  params: Record<string, string>;
  message: string;
}

/**
 * Lightweight parser for structured backend IPC errors.
 * Recognizes:
 *   1. Structured error objects / JSON strings `{ code: "ERR_...", params?: {...}, message: "..." }`
 *   2. Bracketed error-code prefixes `[ERR_<CODE>] <rest>` (with optional `(choices: a, b)` or detail payload)
 */
export function parseBackendError(raw: unknown): ParsedBackendError {
  if (typeof raw === "object" && raw !== null) {
    const obj = raw as Record<string, unknown>;
    if (typeof obj.code === "string") {
      const params: Record<string, string> = {};
      if (typeof obj.params === "object" && obj.params !== null) {
        for (const [k, v] of Object.entries(obj.params as Record<string, unknown>)) {
          if (v !== undefined && v !== null) params[k] = String(v);
        }
      }
      return {
        code: obj.code,
        params,
        message: typeof obj.message === "string" ? obj.message : JSON.stringify(raw),
      };
    }
  }

  const text = typeof raw === "string" ? raw.trim() : String(raw).trim();
  if (text.startsWith("{") && text.endsWith("}")) {
    try {
      const parsed = JSON.parse(text) as Record<string, unknown>;
      if (typeof parsed.code === "string") {
        return parseBackendError(parsed);
      }
    } catch {
      // Fall through to prefix parsing
    }
  }

  const prefixMatch = /^\[(ERR_[A-Z0-9_]+)\]\s*([\s\S]*)$/.exec(text);
  if (prefixMatch) {
    const code = prefixMatch[1];
    let body = prefixMatch[2].trim();
    const params: Record<string, string> = {};
    // Optional leading key=value metadata segment `[ERR_CODE] choices=a,b | message`
    const metaPipe = /^([a-z_]+=[^|\n]+)\|\s*([\s\S]*)$/.exec(body);
    if (metaPipe) {
      for (const pair of metaPipe[1].split(";")) {
        const eq = pair.indexOf("=");
        if (eq > 0) {
          params[pair.slice(0, eq).trim()] = pair.slice(eq + 1).trim();
        }
      }
      body = metaPipe[2].trim();
    }
    return { code, params, message: body };
  }

  return { code: null, params: {}, message: text };
}

export interface LocalizedSkillError {
  text: string;
  replace?: boolean;
  choices?: string[];
}

/**
 * Render skill installation / validation errors via structured `ERR_*` codes and `i18n` dictionaries,
 * without relying on fragile English sentence regexes.
 */
export function localizeSkillError(raw: unknown, locale: Locale = getCurrentLocale()): LocalizedSkillError {
  const parsed = parseBackendError(raw);
  const { code, params, message } = parsed;

  switch (code) {
    case "ERR_SKILL_EXISTS":
    case "ERR_SKILL_ALREADY_INSTALLED":
      return { text: formatMessage(locale, "err.skillExists"), replace: true };
    case "ERR_SKILL_SUBDIRS":
    case "ERR_SKILL_SUBFOLDERS":
    case "ERR_SKILL_MULTIPLE_FOUND": {
      let listRaw = params.choices ?? params.subdirs ?? "";
      if (!listRaw && message) {
        const colonIdx = message.indexOf(":");
        if (colonIdx >= 0) {
          const afterColon = message.slice(colonIdx + 1);
          const dotIdx = afterColon.indexOf(". ");
          listRaw = (dotIdx >= 0 ? afterColon.slice(0, dotIdx) : afterColon).trim();
        } else {
          listRaw = message;
        }
      }
      const choices = listRaw
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      return {
        text: formatMessage(locale, "err.skillSubdirs"),
        ...(choices.length > 0 ? { choices } : {}),
      };
    }
    case "ERR_SKILL_NOT_FOUND":
    case "ERR_NOT_A_SKILL":
      return { text: formatMessage(locale, "err.skillNotFound") };
    case "ERR_SKILL_INVALID_SOURCE":
      return { text: formatMessage(locale, "err.skillInvalidSource") };
    case "ERR_SKILL_PATH_ESCAPE":
      return { text: formatMessage(locale, "err.skillPathEscape") };
    case "ERR_SKILL_INVALID_NAME":
      return { text: formatMessage(locale, "err.skillInvalidName") };
    case "ERR_SKILL_BUILTIN":
      return { text: formatMessage(locale, "err.skillBuiltin") };
    case "ERR_SKILL_TOO_LARGE":
      return { text: formatMessage(locale, "err.skillTooLarge") };
    case "ERR_GIT_MISSING":
    case "ERR_SKILL_GIT_MISSING":
      return { text: formatMessage(locale, "err.gitMissing") };
    case "ERR_GIT_CLONE_FAILED":
    case "ERR_SKILL_CLONE_FAILED":
      return { text: formatMessage(locale, "err.gitCloneFailed", { detail: params.detail || message }) };
    case "ERR_GIT_TIMEOUT":
    case "ERR_SKILL_TIMEOUT":
    case "ERR_SKILL_CLONE_TIMEOUT":
      return { text: formatMessage(locale, "err.gitTimeout") };
    default:
      return { text: message || String(raw) };
  }
}

/**
 * Localize any general backend IPC error string/object for global banners (`[ERR_WORKTREE_BUSY]`, `ERR_SKILL_*`, etc.),
 * falling back cleanly to the human-readable message body without raw `[ERR_*]` noise.
 */
export function localizeBackendError(raw: unknown, locale: Locale = getCurrentLocale()): string {
  const parsed = parseBackendError(raw);
  if (!parsed.code) return parsed.message;
  if (parsed.code === "ERR_WORKTREE_BUSY") {
    return formatMessage(locale, "err.worktreeBusy");
  }
  return localizeSkillError(raw, locale).text;
}

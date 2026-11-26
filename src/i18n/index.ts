import { createContext, createElement, type ReactNode, useCallback, useContext, useEffect, useMemo, useState } from "react";
import { en } from "./en";
import { type MessageKey, type Messages, zhCN } from "./zh-CN";
import { zhPlain } from "./zh-plain";

export type Locale = "zh-CN" | "zh-plain" | "en";
export type { MessageKey, Messages };
export { en, zhCN, zhPlain };

export const DICTIONARIES: Record<Locale, Messages> = {
  "zh-CN": zhCN,
  "zh-plain": zhPlain,
  en,
};

export const LOCALE_OPTIONS: { value: Locale; nameKey: MessageKey; hintKey: MessageKey }[] = [
  { value: "zh-CN", nameKey: "settings.localeZhCN", hintKey: "settings.localeZhCNHint" },
  { value: "zh-plain", nameKey: "settings.localeZhPlain", hintKey: "settings.localeZhPlainHint" },
  { value: "en", nameKey: "settings.localeEn", hintKey: "settings.localeEnHint" },
];

export function resolveLocale(raw?: string | null): Locale {
  if (!raw) return "zh-CN";
  const trimmed = raw.trim();
  if (trimmed === "zh-plain") return "zh-plain";
  if (trimmed === "en" || trimmed.toLowerCase().startsWith("en-")) return "en";
  return "zh-CN";
}

export function htmlLangOf(locale: Locale): "zh-CN" | "en" {
  return locale === "en" ? "en" : "zh-CN";
}

export function intlLocaleOf(locale: Locale): string {
  return locale === "en" ? "en-US" : "zh-CN";
}

let activeLocale: Locale = "zh-CN";

export function getCurrentLocale(): Locale {
  return activeLocale;
}

export function applyDocumentLocale(locale: Locale): void {
  activeLocale = locale;
  if (typeof document !== "undefined" && document.documentElement) {
    document.documentElement.lang = htmlLangOf(locale);
    document.documentElement.dataset.locale = locale;
  }
}

export function setCurrentLocale(raw?: string | null): Locale {
  const next = resolveLocale(raw);
  applyDocumentLocale(next);
  return next;
}

export function getMessages(locale: Locale = activeLocale): Messages {
  return DICTIONARIES[locale] ?? zhCN;
}

export function formatMessage(
  locale: Locale,
  key: MessageKey,
  params?: Record<string, string | number>,
): string {
  const template = (DICTIONARIES[locale] ?? zhCN)[key] ?? zhCN[key];
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (_, name: string) =>
    params[name] !== undefined ? String(params[name]) : `{${name}}`,
  );
}

export function t(key: MessageKey, params?: Record<string, string | number>, locale: Locale = activeLocale): string {
  return formatMessage(locale, key, params);
}

export interface I18nContextValue {
  locale: Locale;
  messages: Messages;
  t: (key: MessageKey, params?: Record<string, string | number>) => string;
  setLocale: (next: Locale) => void;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export interface I18nProviderProps {
  locale?: string | null;
  onLocaleChange?: (next: Locale) => void;
  children: ReactNode;
}

export function I18nProvider({ locale: propLocale, onLocaleChange, children }: I18nProviderProps) {
  const [override, setOverride] = useState<Locale | null>(null);
  const resolved = propLocale !== undefined && propLocale !== null ? resolveLocale(propLocale) : (override ?? activeLocale);

  // Sync synchronously so child renders and non-hook helpers see the active locale immediately.
  activeLocale = resolved;

  useEffect(() => {
    setOverride(null);
  }, [propLocale]);

  useEffect(() => {
    applyDocumentLocale(resolved);
  }, [resolved]);

  const setLocale = useCallback(
    (next: Locale) => {
      setOverride(next);
      applyDocumentLocale(next);
      onLocaleChange?.(next);
    },
    [onLocaleChange],
  );

  const translate = useCallback(
    (key: MessageKey, params?: Record<string, string | number>) => formatMessage(resolved, key, params),
    [resolved],
  );

  const value = useMemo<I18nContextValue>(
    () => ({
      locale: resolved,
      messages: DICTIONARIES[resolved],
      t: translate,
      setLocale,
    }),
    [resolved, translate, setLocale],
  );

  return createElement(I18nContext.Provider, { value }, children);
}

export function useI18n(): I18nContextValue {
  const ctx = useContext(I18nContext);
  if (ctx) return ctx;
  const fallbackLocale = activeLocale;
  return {
    locale: fallbackLocale,
    messages: DICTIONARIES[fallbackLocale],
    t: (key, params) => formatMessage(fallbackLocale, key, params),
    setLocale: (next) => {
      applyDocumentLocale(next);
    },
  };
}

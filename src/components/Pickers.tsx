import { type ReactNode, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { shortCount } from "../lib/fold";
import { useMenuFocus } from "../lib/menuFocus";
import { cooldownRemainingMs, findModelUsage, formatCooldown, todayTokens } from "../lib/modelUsage";
import { bypassFor, modeHintFor, modeOf, modesFor } from "../lib/modes";
import { CLI_INFO } from "../lib/providers";
import type { CliBackend, ListedModel, Mode, ModelUsageEntry, Provider, RawListedModel } from "../lib/types";
import { Info } from "./Hint";
import { Icon } from "./Icon";

const MODE_BI: Record<Mode, string> = {
  manual: "shield-check",
  edit: "pencil-square",
  plan: "journal-check",
  auto: "lightning-charge",
  bypass: "fire",
};

export function normalizeListedModel(entry: RawListedModel): ListedModel {
  if (Array.isArray(entry)) {
    return { id: entry[0], name: entry[1], ...(entry[2] !== undefined ? { free: entry[2] } : {}) };
  }
  return entry;
}

/**
 * Determine whether a listed model is free-tier, prioritizing the structured `model.free` boolean
 * and falling back to safe ID / keyword heuristics only when `model.free === undefined`.
 */
export function isModelFree(model: ListedModel): boolean {
  if (typeof model.free === "boolean") return model.free;
  const lowerId = model.id.toLowerCase();
  return lowerId.endsWith("-free") || lowerId.endsWith("/big-pickle") || /(?:\bfree\b|免费)/i.test(model.name);
}

/** Said before 全权托付 is switched on, and only switched on when 本尊 stamps it. */
function BypassDialog({ onConfirm, onClose }: { onConfirm: () => void; onClose: () => void }) {
  const uid = useId();
  const titleId = `${uid}-bypass-title`;
  const dialog = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    dialog.current?.showModal();
    cancel.current?.focus();
  }, []);
  return (
    <dialog ref={dialog} className="dialog danger" aria-labelledby={titleId} onClose={onClose}>
      <form
        method="dialog"
        onSubmit={(event) => {
          event.preventDefault();
          onConfirm();
        }}
      >
        <h2 id={titleId}>全权托付这个分身？</h2>
        <p>托付之后，分身做什么都不再请示：跑任何命令、改洞府之外的文件、改 .git、上网、动用身外法器，一概自行放行。</p>
        <p>
          <strong>Windows 上没有结界保护。</strong>它能删除、改写这台电脑上本尊能动的任何文件，也能把文件内容发到网上。它在独立工作树里改的东西，并回前不会进洞府；但洞府之外的一切不受保护。
        </p>
        <p className="muted">只对这个分身生效，不会成为新分身的默认，功课也用不了。随时可以在禁制菜单里换回别的。</p>
        <div className="actions">
          <button type="submit" className="button solid">
            确认托付
          </button>
          <button type="button" className="button text" ref={cancel} onClick={onClose}>
            算了
          </button>
        </div>
      </form>
    </dialog>
  );
}

function ModeIcon({ mode }: { mode: Mode }) {
  return <Icon name={MODE_BI[mode]} size={15} />;
}

export interface PopupProps {
  /** What the button shows. */
  label: ReactNode;
  title?: string;
  disabled?: boolean;
  /** What the menu is, for a screen reader. */
  name: string;
  /** The menu's contents, given a function that closes it. */
  children: (close: () => void) => ReactNode;
  /** Opens the menu each time it changes from what it was: for a "/" command that opens it. */
  summon?: number;
}

/** Keeps a `.mode-menu` popup horizontally inside the viewport without flashing. */
function useKeepInView(open: boolean) {
  const menu = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const el = menu.current;
      if (!el) return;
      el.style.left = "0px";
      const rect = el.getBoundingClientRect();
      const margin = 12;
      let dx = 0;
      if (rect.right > window.innerWidth - margin) {
        dx = window.innerWidth - margin - rect.right;
      }
      if (rect.left + dx < margin) {
        dx = margin - rect.left;
      }
      el.style.left = `${Math.round(dx)}px`;
    };
    place();
    window.addEventListener("resize", place);
    return () => window.removeEventListener("resize", place);
  }, [open]);
  return menu;
}

/** A button in the composer row whose menu opens above it, supports arrow navigation, and returns focus on close. */
export function Popup({ label, title, disabled, name, children, summon = 0 }: PopupProps) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menu = useKeepInView(open);
  useMenuFocus(open, menu, () => setOpen(false), triggerRef);

  useEffect(() => {
    if (summon > 0 && !disabled) setOpen(true);
  }, [summon, disabled]);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => !root.current?.contains(event.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);
  return (
    <div
      className="mode-picker"
      ref={root}
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <button
        ref={triggerRef}
        type="button"
        className="select-button"
        aria-haspopup="menu"
        aria-expanded={open}
        title={title}
        disabled={disabled}
        onClick={() => setOpen(!open)}
      >
        {label}
      </button>
      {open && (
        <div className="mode-menu" ref={menu} role="menu" aria-label={name}>
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

export const CHECK = <Icon name="check-lg" size={14} className="check" />;

export interface ModelListProps {
  providers: Provider[];
  listed: (provider: string) => boolean;
  keyed: (provider: string) => boolean;
  providerId: string;
  model: string;
  onPick: (provider: string, model: string) => void;
  onAutoFallback?: (provider: string, model: string) => void;
  listModels: (provider: string) => Promise<RawListedModel[]>;
  modelUsage?: ModelUsageEntry[];
  nowMs?: number;
}

const MODEL_CACHE_KEY = "odg.model-list-cache.v4";
const OPENCODE_TIER_KEY = "odg.opencode-model-tier.v1";

type OpenCodeTierFilter = "free" | "go" | "all";

function openCodeModelTier(entry: ListedModel): "free" | "go" | "paid" {
  if (isModelFree(entry)) return "free";
  const lowerId = entry.id.toLowerCase();
  if (lowerId.startsWith("opencode-go/") || /\bGo\b/.test(entry.name)) return "go";
  return "paid";
}

function loadOpenCodeTier(): OpenCodeTierFilter {
  try {
    const v = localStorage.getItem(OPENCODE_TIER_KEY);
    if (v === "free" || v === "go" || v === "all") return v;
  } catch {
    // ignore
  }
  return "free";
}

function isValidCachedModel(item: unknown): item is RawListedModel {
  if (Array.isArray(item)) {
    return item.length >= 2 && typeof item[0] === "string" && typeof item[1] === "string";
  }
  if (typeof item === "object" && item !== null) {
    const obj = item as Record<string, unknown>;
    return typeof obj.id === "string" && typeof obj.name === "string";
  }
  return false;
}

const modelListCache: Map<string, ListedModel[]> = (() => {
  const map = new Map<string, ListedModel[]>();
  try {
    const raw = localStorage.getItem(MODEL_CACHE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      for (const [k, v] of Object.entries(parsed)) {
        if (Array.isArray(v) && v.every(isValidCachedModel)) {
          map.set(k, v.map(normalizeListedModel));
        }
      }
    }
  } catch {
    // Ignore storage read errors.
  }
  return map;
})();

function saveModelListCache(providerId: string, models: ListedModel[]) {
  modelListCache.set(providerId, models);
  try {
    const obj: Record<string, ListedModel[]> = {};
    for (const [k, v] of modelListCache.entries()) obj[k] = v;
    localStorage.setItem(MODEL_CACHE_KEY, JSON.stringify(obj));
  } catch {
    // Ignore storage quota errors.
  }
}

export function ModelList({
  providers,
  listed,
  keyed,
  providerId,
  model,
  onPick,
  onAutoFallback,
  listModels,
  modelUsage = [],
  nowMs = Date.now(),
}: ModelListProps) {
  const { t } = useI18n();
  const [showing, setShowing] = useState(providerId);
  const [asked, setAsked] = useState<{ state: "asking" | "told" | "failed"; models: ListedModel[]; why?: string }>(() => {
    const cached = modelListCache.get(providerId);
    return cached ? { state: "told", models: cached } : { state: "asking", models: [] };
  });
  const [refreshTick, setRefreshTick] = useState(0);
  const [filter, setFilter] = useState("");
  const [openCodeTier, setOpenCodeTier] = useState<OpenCodeTierFilter>(loadOpenCodeTier);
  const pickOpenCodeTier = (next: OpenCodeTierFilter) => {
    setOpenCodeTier(next);
    try {
      localStorage.setItem(OPENCODE_TIER_KEY, next);
    } catch {
      // ignore
    }
  };
  const forceRefresh = useRef(false);
  useEffect(() => {
    let current = true;
    const force = forceRefresh.current;
    forceRefresh.current = false;
    const cached = modelListCache.get(showing);
    if (cached && !force) {
      setAsked({ state: "told", models: cached });
      return;
    }
    setAsked((prev) => ({ state: "asking", models: cached ?? prev.models }));
    listModels(showing).then(
      (rawModels) => {
        const normalized = rawModels.map(normalizeListedModel);
        saveModelListCache(showing, normalized);
        if (current) setAsked({ state: "told", models: normalized });
      },
      (error) => {
        if (current) setAsked({ state: "failed", models: cached ?? [], why: String(error) });
      },
    );
    return () => {
      current = false;
    };
  }, [showing, listModels, refreshTick]);
  const triggerRefresh = () => {
    forceRefresh.current = true;
    setRefreshTick((n) => n + 1);
  };
  const provider = providers.find((known) => known.id === showing);
  const home = showing === providerId;
  const fellBack = useRef<string | null>(null);
  useEffect(() => {
    if (!home || !onAutoFallback || asked.state !== "told" || asked.models.length === 0) return;
    if (model === "auto" || asked.models.some((item) => item.id === model)) return;
    const key = `${providerId}:${model}`;
    if (fellBack.current === key) return;
    fellBack.current = key;
    onAutoFallback(providerId, asked.models[0].id);
  }, [home, onAutoFallback, asked, providerId, model]);
  const builtinFallback: ListedModel[] = (provider?.models ?? []).map(normalizeListedModel);
  const known = asked.models.length > 0 ? asked.models : asked.state === "failed" ? builtinFallback : [];
  const supportsAuto = ["anthropic", "gemini", "openai", "deepseek", "moonshot", "zhipu", "openrouter"].includes(provider?.kind ?? showing);
  const withAuto: ListedModel[] =
    supportsAuto && !known.some((item) => item.id === "auto")
      ? [{ id: "auto", name: t("picker.autoRoute") }, ...known]
      : known;
  const models: ListedModel[] =
    !home || withAuto.some((item) => item.id === model) ? withAuto : [{ id: model, name: model }, ...withAuto];
  const isOpenCodeShowing = showing === "opencode" || showing.startsWith("opencode-");
  const freeModels = isOpenCodeShowing ? models.filter((m) => openCodeModelTier(m) === "free") : [];
  const goModels = isOpenCodeShowing ? models.filter((m) => openCodeModelTier(m) === "go") : [];
  const hasTierChoice = isOpenCodeShowing && freeModels.length > 0 && (freeModels.length < models.length || goModels.length > 0);
  const activeTier: OpenCodeTierFilter = !hasTierChoice
    ? "all"
    : openCodeTier === "go" && goModels.length === 0
      ? "free"
      : openCodeTier;
  const tiered = !hasTierChoice || activeTier === "all" ? models : activeTier === "free" ? freeModels : goModels;
  const shown = tiered.filter((item) => `${item.id} ${item.name}`.toLowerCase().includes(filter.trim().toLowerCase()));
  const name = (id: string) => providers.find((known) => known.id === id)?.name ?? id;
  const tabs = [providerId, ...providers.map((known) => known.id).filter((id) => id !== providerId && listed(id))];
  return (
    <>
      {tabs.length > 1 && (
        <div className="menu-tabs" role="tablist" aria-label="宗门">
          {tabs.map((id) => {
            const open = id === providerId || keyed(id);
            return (
              <button
                type="button"
                role="tab"
                key={id}
                className="menu-tab"
                aria-selected={id === showing}
                disabled={!open}
                title={open ? (id === providerId ? `${name(id)}：这个分身眼下的宗门` : `看看 ${name(id)} 的心法`) : `${name(id)} 还没有密钥：先到设置的「心法」页填上`}
                onClick={() => {
                  setShowing(id);
                  setFilter("");
                }}
              >
                {name(id)}
              </button>
            );
          })}
        </div>
      )}
      <div className="mode-menu-head">
        <span>
          {name(showing)} · {t("picker.model")}
          <Info>换了之后，这个分身接下来的话由新心法来答，前情照旧带着。列表会缓存，点右侧刷新按钮可重新向宗门拉取。</Info>
        </span>
        <span className="menu-head-right">
          <span>{asked.state === "asking" ? "查询中…" : asked.state === "told" ? `共 ${tiered.length} 门` : "内置列表"}</span>
          <button
            type="button"
            className={`quota-refresh-btn ${asked.state === "asking" ? "spinning" : ""}`}
            disabled={asked.state === "asking"}
            title={asked.state === "asking" ? "正在拉取模型列表…" : "刷新模型列表"}
            aria-label={asked.state === "asking" ? "正在拉取模型列表…" : "刷新模型列表"}
            onClick={triggerRefresh}
          >
            <Icon name="arrow-clockwise" size={13} />
          </button>
        </span>
      </div>
      {asked.state === "failed" && <p className="menu-note">{asked.why}</p>}
      {hasTierChoice && (
        <div className="model-tier-tabs" role="tablist" aria-label="OpenCode 模型分层">
          <button type="button" role="tab" className="model-tier-pill" aria-selected={activeTier === "free"} onClick={() => pickOpenCodeTier("free")}>
            {t("picker.freeCount", { count: freeModels.length })}
          </button>
          {goModels.length > 0 && (
            <button type="button" role="tab" className="model-tier-pill" aria-selected={activeTier === "go"} onClick={() => pickOpenCodeTier("go")}>
              Go {goModels.length}
            </button>
          )}
          <button type="button" role="tab" className="model-tier-pill" aria-selected={activeTier === "all"} onClick={() => pickOpenCodeTier("all")}>
            {t("picker.allCount", { count: models.length })}
          </button>
        </div>
      )}
      {tiered.length > 8 && (
        <div className="menu-filter-row">
          <input className="input menu-filter" type="search" placeholder="筛一筛" aria-label="筛选心法" autoFocus value={filter} onChange={(event) => setFilter(event.target.value)} />
        </div>
      )}
      <div className="menu-scroll">
        {shown.map((entry) => {
          const { id, name: display } = entry;
          const isAuto = id === "auto";
          const free = entry.free === true;
          const stat = findModelUsage(modelUsage, showing, id);
          const coolMs = cooldownRemainingMs(stat, nowMs);
          const todayTok = stat ? todayTokens(stat) : 0;
          return (
            <button type="button" role="menuitemradio" aria-checked={home && id === model} key={id} className="mode-option plain" onClick={() => onPick(showing, id)}>
              <span className="mode-text">
                <span className="name mono">{id}</span>
                {display !== id && <span className="hint">{display}</span>}
                {(isAuto || free || coolMs > 0 || todayTok > 0) && (
                  <span className="model-meta-badges">
                    {isAuto && <span className="model-usage-badge">{t("picker.experimental")}</span>}
                    {free && <span className="model-usage-badge">{t("picker.free")}</span>}
                    {coolMs > 0 && (
                      <span className="model-cooldown-pill" title={stat?.cooldown_reason ?? "冷却中"}>
                        冷却 {formatCooldown(coolMs)}
                      </span>
                    )}
                    {todayTok > 0 && <span className="model-usage-badge">今日 {shortCount(todayTok)}</span>}
                  </span>
                )}
              </span>
              {home && id === model ? CHECK : <span />}
            </button>
          );
        })}
        {shown.length === 0 && <p className="menu-note">{asked.state === "asking" ? "正在问…" : "没有对得上的。"}</p>}
      </div>
      {!home && <p className="menu-note">换到 {name(showing)} 之后，前情照旧带着；只是前面那家的推演过程不带过去，别家读不了它。</p>}
    </>
  );
}

/** The limits the menu offers, read there as 50K, 100K, 200K, 500K. */
export const LIMITS = [50_000, 100_000, 200_000, 500_000];

/** The 禁制 button and its menu: what the 分身 may do without asking. */
export function ModePicker({
  mode,
  onMode,
  trustable,
  cli = null,
}: {
  mode: Mode;
  onMode: (mode: Mode) => void;
  trustable: boolean;
  cli?: CliBackend | null;
}) {
  const { locale, t } = useI18n();
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menu = useKeepInView(open);
  useMenuFocus(open, menu, () => setOpen(false), triggerRef);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => !root.current?.contains(event.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const localizedModes = modesFor(locale);
  const localizedBypass = bypassFor(locale);
  const current = modeOf(mode, locale);
  const currentHint = modeHintFor(mode, cli, locale);
  return (
    <div
      className="mode-picker"
      ref={root}
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <button
        ref={triggerRef}
        type="button"
        className={`select-button ${mode === "bypass" ? "danger" : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        title={`${t("picker.mode")}：${currentHint}。Shift+Tab`}
        onClick={() => setOpen(!open)}
      >
        <ModeIcon mode={mode} />
        <span className="label sr-only">{t("picker.mode")}</span>
        <span className="value">{current.name}</span>
      </button>
      {open && (
        <div className="mode-menu" ref={menu} role="menu" aria-label={t("picker.mode")}>
          <div className="mode-menu-head">
            <span>{t("picker.mode")}</span>
            <span>
              <kbd>Shift</kbd> + <kbd>Tab</kbd>
            </span>
          </div>
          {localizedModes.map((option) => (
            <button
              type="button"
              role="menuitemradio"
              aria-checked={option.value === mode}
              key={option.value}
              className="mode-option"
              onClick={() => {
                onMode(option.value);
                setOpen(false);
              }}
            >
              <ModeIcon mode={option.value} />
              <span className="mode-text">
                <span className="name">{option.name}</span>
                <span className="hint">{modeHintFor(option.value, cli, locale)}</span>
              </span>
              {option.value === mode ? CHECK : <span className="check" aria-hidden="true" />}
            </button>
          ))}
          <button
            type="button"
            role="menuitemradio"
            aria-checked={mode === "bypass"}
            className="mode-option danger"
            disabled={!trustable}
            title={trustable ? undefined : "只有独立工作树的分身才能全权托付：在洞府名旁点分支图标化一个"}
            onClick={() => {
              setOpen(false);
              if (mode !== "bypass") setConfirming(true);
            }}
          >
            <ModeIcon mode="bypass" />
            <span className="mode-text">
              <span className="name">{localizedBypass.name}</span>
              <span className="hint">{trustable ? modeHintFor("bypass", cli, locale) : "只有独立工作树的分身才能托付"}</span>
            </span>
            {mode === "bypass" ? CHECK : <span className="check" aria-hidden="true" />}
          </button>
          {cli && (
            <p className="menu-note">
              当前心法由本机 <code className="mono">{CLI_INFO[cli].bin}</code> 无头驱动，禁制直接映射为该 CLI 的权限与沙箱参数。
            </p>
          )}
        </div>
      )}
      {confirming && (
        <BypassDialog
          onConfirm={() => {
            setConfirming(false);
            onMode("bypass");
          }}
          onClose={() => setConfirming(false)}
        />
      )}
    </div>
  );
}

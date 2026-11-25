import { type ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";
import { shortCount } from "../lib/fold";
import { cooldownRemainingMs, findModelUsage, formatCooldown, todayTokens } from "../lib/modelUsage";
import { BYPASS, MODES, modeOf } from "../lib/modes";
import type { Mode, ModelUsageEntry, Provider } from "../lib/types";
import { Info } from "./Hint";

const MODE_ICONS: Record<Mode, string> = {
  manual: "M5.5 8.5V4a1 1 0 0 1 2 0v3.5M7.5 7V2.8a1 1 0 0 1 2 0V7M9.5 7V3.8a1 1 0 0 1 2 0V8.5M11.5 7.5a1 1 0 0 1 2 0V10a4.5 4.5 0 0 1-4.5 4.5h-.6a4 4 0 0 1-3.2-1.6L3.3 10.3a1 1 0 0 1 1.5-1.3l.7.8",
  edit: "M6 4 2 8l4 4M10 4l4 4-4 4",
  plan: "M4 2.5h8v11H4zM6 5.5h4M6 8h4M6 10.5h2.5",
  auto: "M9 1.5 3.5 9H8l-1 5.5L12.5 7H8z",
  // An open lock: nothing is kept shut.
  bypass: "M4 7.5h8v6H4zM6 7.5V5a2 2 0 0 1 3.9-.6M8 10v1.2",
};

/** Said before 全权托付 is switched on, and only switched on when 本尊 stamps it. */
function BypassDialog({ onConfirm, onClose }: { onConfirm: () => void; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  // The keyboard starts on 算了: Enter alone must not hand everything over.
  useEffect(() => {
    dialog.current?.showModal();
    cancel.current?.focus();
  }, []);
  return (
    <dialog ref={dialog} className="dialog danger" aria-labelledby="bypass-title" onClose={onClose}>
      <form
        method="dialog"
        onSubmit={(event) => {
          event.preventDefault();
          onConfirm();
        }}
      >
        <h2 id="bypass-title">全权托付这个分身？</h2>
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
  return (
    <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
      <path d={MODE_ICONS[mode]} />
    </svg>
  );
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

/** A button in the composer row whose menu opens above it, and closes on Escape or a click elsewhere. */
export function Popup({ label, title, disabled, name, children, summon = 0 }: PopupProps) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const menu = useKeepInView(open);
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
    <div className="mode-picker" ref={root} onKeyDown={(event) => event.key === "Escape" && setOpen(false)}>
      <button type="button" className="select-button" aria-haspopup="menu" aria-expanded={open} title={title} disabled={disabled} onClick={() => setOpen(!open)}>
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

export const CHECK = (
  <svg className="check" aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
    <path d="m3 8.5 3.2 3L13 4.5" />
  </svg>
);

export interface ModelListProps {
  /** Every provider this build knows. */
  providers: Provider[];
  /** Whether a provider is in the list the user keeps: only those are offered. */
  listed: (provider: string) => boolean;
  /** Whether a provider has a key to be asked with: one without cannot be moved to. */
  keyed: (provider: string) => boolean;
  /** The provider and model the session is on. */
  providerId: string;
  model: string;
  onPick: (provider: string, model: string) => void;
  /** Called silently when the current model is missing from the provider's live model list. */
  onAutoFallback?: (provider: string, model: string) => void;
  listModels: (provider: string) => Promise<[string, string][]>;
  modelUsage?: ModelUsageEntry[];
  nowMs?: number;
}

const MODEL_CACHE_KEY = "odg.model-list-cache.v1";

/** Persistent cache of live model lists per provider so opening the picker uses cached models until refreshed. */
const modelListCache: Map<string, [string, string][]> = (() => {
  const map = new Map<string, [string, string][]>();
  try {
    const raw = localStorage.getItem(MODEL_CACHE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      for (const [k, v] of Object.entries(parsed)) {
        if (Array.isArray(v) && v.every((item) => Array.isArray(item) && item.length >= 2 && typeof item[0] === "string" && typeof item[1] === "string")) {
          map.set(k, v as [string, string][]);
        }
      }
    }
  } catch {
    // Ignore storage read errors.
  }
  return map;
})();

function saveModelListCache(providerId: string, models: [string, string][]) {
  modelListCache.set(providerId, models);
  try {
    const obj: Record<string, [string, string][]> = {};
    for (const [k, v] of modelListCache.entries()) obj[k] = v;
    localStorage.setItem(MODEL_CACHE_KEY, JSON.stringify(obj));
  } catch {
    // Ignore storage quota errors.
  }
}

/**
 * The models to choose from, a provider at a time: the session's own first, then the others the user added, under
 * the names given them. Each lists what it says it serves, or its built-in suggestions while that is unknown.
 */
export function ModelList({ providers, listed, keyed, providerId, model, onPick, onAutoFallback, listModels, modelUsage = [], nowMs = Date.now() }: ModelListProps) {
  /** The provider whose models are listed. */
  const [showing, setShowing] = useState(providerId);
  const [asked, setAsked] = useState<{ state: "asking" | "told" | "failed"; models: [string, string][]; why?: string }>(() => {
    const cached = modelListCache.get(providerId);
    return cached ? { state: "told", models: cached } : { state: "asking", models: [] };
  });
  const [refreshTick, setRefreshTick] = useState(0);
  const [filter, setFilter] = useState("");
  const forceRefresh = useRef(false);
  // Use cached list when available; fetch on cache miss or explicit refresh.
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
      (models) => {
        saveModelListCache(showing, models);
        if (current) setAsked({ state: "told", models });
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
  // If the provider returned a live list of models and the session's current model is no longer in it,
  // automatically fall back to the provider's first available model instead of keeping a dead model selected.
  const fellBack = useRef<string | null>(null);
  useEffect(() => {
    if (!home || !onAutoFallback || asked.state !== "told" || asked.models.length === 0) return;
    if (asked.models.some(([id]) => id === model)) return;
    const key = `${providerId}:${model}`;
    if (fellBack.current === key) return;
    fellBack.current = key;
    onAutoFallback(providerId, asked.models[0][0]);
  }, [home, onAutoFallback, asked, providerId, model]);
  const known = asked.models.length > 0 ? asked.models : asked.state === "failed" ? (provider?.models ?? []) : [];
  // The model in use is always there to return to, even when the provider's list leaves it out.
  const models = !home || known.some(([id]) => id === model) ? known : [[model, model] as [string, string], ...known];
  const shown = models.filter(([id, name]) => `${id} ${name}`.toLowerCase().includes(filter.trim().toLowerCase()));
  const name = (id: string) => providers.find((known) => known.id === id)?.name ?? id;
  // The session's own provider first, even one no longer in the list; then the others in it. Kinds not added are left out.
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
          {name(showing)} 的心法
          <Info>换了之后，这个分身接下来的话由新心法来答，前情照旧带着。列表会缓存，点右侧刷新按钮可重新向宗门拉取。</Info>
        </span>
        <span className="menu-head-right">
          <span>{asked.state === "asking" ? "查询中…" : asked.state === "told" ? `共 ${asked.models.length} 门` : "内置列表"}</span>
          <button
            type="button"
            className={`quota-refresh-btn ${asked.state === "asking" ? "spinning" : ""}`}
            disabled={asked.state === "asking"}
            title={asked.state === "asking" ? "正在拉取模型列表…" : "刷新模型列表"}
            aria-label={asked.state === "asking" ? "正在拉取模型列表…" : "刷新模型列表"}
            onClick={triggerRefresh}
          >
            <svg aria-hidden="true" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M13.5 8a5.5 5.5 0 1 1-1.45-3.72" />
              <path d="M13.5 2.75v2.75h-2.75" />
            </svg>
          </button>
        </span>
      </div>
      {asked.state === "failed" && <p className="menu-note">{asked.why}</p>}
      {models.length > 8 && (
        <div className="menu-filter-row">
          <input className="input menu-filter" type="search" placeholder="筛一筛" aria-label="筛选心法" autoFocus value={filter} onChange={(event) => setFilter(event.target.value)} />
        </div>
      )}
      <div className="menu-scroll">
        {shown.map(([id, display]) => {
          const stat = findModelUsage(modelUsage, showing, id);
          const coolMs = cooldownRemainingMs(stat, nowMs);
          const todayTok = stat ? todayTokens(stat) : 0;
          return (
            <button type="button" role="menuitemradio" aria-checked={home && id === model} key={id} className="mode-option plain" onClick={() => onPick(showing, id)}>
              <span className="mode-text">
                <span className="name mono">{id}</span>
                {display !== id && <span className="hint">{display}</span>}
                {(coolMs > 0 || todayTok > 0) && (
                  <span className="model-meta-badges">
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
      {/* Moving to another provider leaves something behind: that is said in the open. Within one, nothing is lost. */}
      {!home && <p className="menu-note">换到 {name(showing)} 之后，前情照旧带着；只是前面那家的推演过程不带过去，别家读不了它。</p>}
    </>
  );
}

/** The limits the menu offers, read there as 50K, 100K, 200K, 500K. */
export const LIMITS = [50_000, 100_000, 200_000, 500_000];

/** The 禁制 button and its menu: what the 分身 may do without asking. */
export function ModePicker({ mode, onMode, trustable }: { mode: Mode; onMode: (mode: Mode) => void; trustable: boolean }) {
  const [open, setOpen] = useState(false);
  /** 全权托付 was picked: the dialog that says what it means is open. */
  const [confirming, setConfirming] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => !root.current?.contains(event.target as Node) && setOpen(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);
  const current = modeOf(mode);
  const menu = useKeepInView(open);
  return (
    <div className="mode-picker" ref={root} onKeyDown={(event) => event.key === "Escape" && setOpen(false)}>
      <button
        type="button"
        className={`select-button ${mode === "bypass" ? "danger" : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        title={`禁制：${current.hint}。Shift+Tab 切换`}
        onClick={() => setOpen(!open)}
      >
        <ModeIcon mode={mode} />
        <span className="label sr-only">禁制</span>
        <span className="value">{current.name}</span>
      </button>
      {open && (
        <div className="mode-menu" ref={menu} role="menu" aria-label="禁制">
          <div className="mode-menu-head">
            <span>禁制</span>
            <span>
              <kbd>Shift</kbd> + <kbd>Tab</kbd> 切换
            </span>
          </div>
          {MODES.map((option) => (
            <button
              type="button"
              role="menuitemradio"
              aria-checked={option.value === mode}
              key={option.value}
              className="mode-option"
              autoFocus={option.value === mode}
              onClick={() => {
                onMode(option.value);
                setOpen(false);
              }}
            >
              <ModeIcon mode={option.value} />
              <span className="mode-text">
                <span className="name">{option.name}</span>
                <span className="hint">{option.hint}</span>
              </span>
              <svg className="check" aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                {option.value === mode && <path d="m3 8.5 3.2 3L13 4.5" />}
              </svg>
            </button>
          ))}
          {/* Apart from the four, in cinnabar, and only where a worktree keeps its work off the folder's own files. */}
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
              <span className="name">{BYPASS.name}</span>
              <span className="hint">{trustable ? BYPASS.hint : "只有独立工作树的分身才能托付"}</span>
            </span>
            <svg className="check" aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              {mode === "bypass" && <path d="m3 8.5 3.2 3L13 4.5" />}
            </svg>
          </button>
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

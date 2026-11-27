import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { formatMoney, formatUsd, hydratePriceCatalog } from "../lib/pricing";
import { shortCount } from "../lib/fold";
import type { BillingMode, Currency, PriceCatalogEntry, PriceCatalogView, SessionItem, UsageLedgerSummary } from "../lib/types";
import { Info } from "./Hint";
import { Icon } from "./Icon";
import { UsageCharts } from "./UsageCharts";

type WindowOption = { label: string; days: number | null };
const WINDOWS: WindowOption[] = [
  { label: "今日", days: 1 },
  { label: "近 7 天", days: 7 },
  { label: "近 30 天", days: 30 },
  { label: "全部", days: null },
];

type GroupTab = "model" | "provider" | "session";
type CatalogFilter = "all" | "overridden" | "custom" | "models_dev" | "builtin";

function isOverridden(entry: PriceCatalogEntry): boolean {
  return entry.rate.source === "custom" && Boolean(entry.overrides);
}

function isPureCustom(entry: PriceCatalogEntry): boolean {
  return entry.rate.source === "custom" && !entry.overrides;
}

interface PriceDraft {
  key: string;
  currency: Currency;
  input: string;
  cached: string;
  cacheWrite: string;
  output: string;
  editing: boolean;
}

function billingLabel(mode: BillingMode): string {
  if (mode === "subscription") return "订阅等效";
  if (mode === "local") return "本地";
  return "API";
}

function entryTag(entry: PriceCatalogEntry): { cls: string; label: string } {
  if (isOverridden(entry)) {
    return {
      cls: "source-overridden",
      label: entry.overrides === "models_dev" ? "已改 · models.dev" : "已改 · 内置",
    };
  }
  if (entry.rate.source === "custom") return { cls: "source-custom", label: "自定义" };
  if (entry.rate.source === "models_dev") return { cls: "source-models_dev", label: "models.dev" };
  return { cls: "source-builtin", label: "内置" };
}

function formatDualCurrency(usd: number, cny: number, totalUsdEquiv: number): { primary: string; secondary: string | null } {
  const parts: string[] = [];
  if (usd > 0) parts.push(formatMoney(usd, "USD"));
  if (cny > 0) parts.push(formatMoney(cny, "CNY"));
  if (parts.length === 0) return { primary: "$0.00", secondary: null };
  if (parts.length === 1) {
    return {
      primary: parts[0],
      secondary: cny > 0 ? `≈ ${formatUsd(totalUsdEquiv)}` : null,
    };
  }
  return {
    primary: parts.join(" + "),
    secondary: `合计 ≈ ${formatUsd(totalUsdEquiv)}`,
  };
}

function formatSyncedAt(ts: string | null | undefined): string {
  if (!ts) return "未同步（当前使用内置表）";
  const ms = Date.parse(ts);
  if (!Number.isFinite(ms)) return ts;
  return new Date(ms).toLocaleString();
}

const ledgerSummaryCache = new Map<string, UsageLedgerSummary>();
let catalogCache: PriceCatalogView | null = null;
let sessionsCache: SessionItem[] | null = null;

function trimSlash(path: string): string {
  return path.replace(/[\\/]+$/, "");
}

function caveName(session: SessionItem, chatFolder?: string): string {
  const ws = trimSlash(session.worktree?.origin || session.workspace || "");
  if (!ws || (chatFolder && ws === trimSlash(chatFolder))) return "闲谈";
  return ws.split(/[\\/]/).pop() || ws;
}

export function UsagePricingSection({
  section = "usage",
  chatFolder,
}: {
  section?: "usage" | "pricing";
  chatFolder?: string;
}) {
  const [days, setDays] = useState<number | null>(7);
  const [groupTab, setGroupTab] = useState<GroupTab>("model");
  const [summary, setSummary] = useState<UsageLedgerSummary | null>(() => ledgerSummaryCache.get("7") ?? null);
  const [catalog, setCatalog] = useState<PriceCatalogView | null>(() => catalogCache);
  const [sessions, setSessions] = useState<SessionItem[]>(() => sessionsCache ?? []);
  const [loadingLedger, setLoadingLedger] = useState(false);
  const [syncingCatalog, setSyncingCatalog] = useState(false);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [sourceFilter, setSourceFilter] = useState<CatalogFilter>("all");
  const [draft, setDraft] = useState<PriceDraft | null>(null);
  const [savingDraft, setSavingDraft] = useState(false);
  const [resettingOverrides, setResettingOverrides] = useState(false);

  const loadLedger = useCallback((winDays: number | null) => {
    const key = String(winDays ?? "all");
    const cached = ledgerSummaryCache.get(key);
    if (cached) setSummary(cached);
    setLoadingLedger(true);
    api
      .getUsageLedgerSummary(winDays)
      .then((res) => {
        ledgerSummaryCache.set(key, res);
        setSummary(res);
      })
      .catch((err) => setErrorMsg(String(err)))
      .finally(() => setLoadingLedger(false));
  }, []);

  const applyCatalog = useCallback((next: PriceCatalogView) => {
    catalogCache = next;
    setCatalog(next);
    hydratePriceCatalog(next);
  }, []);

  const loadCatalog = useCallback(() => {
    api
      .getPriceCatalog()
      .then((res) => applyCatalog(res))
      .catch(() => undefined);
  }, [applyCatalog]);

  useEffect(() => {
    loadLedger(days);
  }, [days, loadLedger]);

  useEffect(() => {
    loadCatalog();
  }, [loadCatalog]);

  useEffect(() => {
    if (section !== "usage") return;
    api
      .listSessions()
      .then((list) => {
        sessionsCache = list;
        setSessions(list);
      })
      .catch(() => undefined);
  }, [section]);

  const sessionInfoMap = useMemo(() => {
    const map = new Map<string, { cave: string; title: string }>();
    for (const item of sessions) {
      map.set(item.id, {
        cave: caveName(item, chatFolder),
        title: item.title?.trim() || "新分身",
      });
    }
    return map;
  }, [sessions, chatFolder]);

  const handleSyncCatalog = async () => {
    setSyncingCatalog(true);
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      const next = await api.syncPriceCatalog();
      applyCatalog(next);
      setStatusMsg(`已从 models.dev 同步 ${next.models_dev_count} 条模型价格。`);
    } catch (err) {
      setErrorMsg(String(err));
    } finally {
      setSyncingCatalog(false);
    }
  };

  const handleResetUsage = async () => {
    setErrorMsg(null);
    try {
      await api.resetModelUsage();
      ledgerSummaryCache.clear();
      loadLedger(days);
      setStatusMsg("已清空本地用量与账本记录。");
    } catch (err) {
      setErrorMsg(String(err));
    }
  };

  const openEditor = (entry?: PriceCatalogEntry) => {
    setErrorMsg(null);
    setStatusMsg(null);
    if (entry) {
      setDraft({
        key: entry.key,
        currency: entry.rate.currency,
        input: String(entry.rate.input_per_m),
        cached: String(entry.rate.cached_per_m),
        cacheWrite: String(entry.rate.cache_write_per_m),
        output: String(entry.rate.output_per_m),
        editing: true,
      });
    } else {
      setDraft({
        key: "",
        currency: "USD",
        input: "0",
        cached: "0",
        cacheWrite: "0",
        output: "0",
        editing: false,
      });
    }
  };

  const handleSaveDraft = async () => {
    if (!draft) return;
    const key = draft.key.trim();
    const input = Number(draft.input);
    const cached = Number(draft.cached);
    const cacheWrite = Number(draft.cacheWrite);
    const output = Number(draft.output);
    if (!key) {
      setErrorMsg("请填写模型标识（例如 moonshot/kimi-k3.1-pro 或 gpt-5.4）。");
      return;
    }
    if (![input, cached, cacheWrite, output].every((v) => Number.isFinite(v) && v >= 0)) {
      setErrorMsg("单价必须是非负数字。");
      return;
    }
    setSavingDraft(true);
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      const next = await api.setCustomPrice(key, {
        currency: draft.currency,
        input_per_m: input,
        cached_per_m: cached,
        cache_write_per_m: cacheWrite,
        output_per_m: output,
        source: "custom",
      });
      applyCatalog(next);
      setDraft(null);
      setStatusMsg(`已保存自定义价格：${key}`);
    } catch (err) {
      setErrorMsg(String(err));
    } finally {
      setSavingDraft(false);
    }
  };

  const handleDeleteCustom = async (key: string, overridden: boolean) => {
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      const next = await api.deleteCustomPrice(key);
      applyCatalog(next);
      setStatusMsg(overridden ? `已恢复默认价格：${key}` : `已删除自定义价格：${key}`);
    } catch (err) {
      setErrorMsg(String(err));
    }
  };

  const handleResetOverrides = async () => {
    setResettingOverrides(true);
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      const next = await api.resetPriceOverrides();
      applyCatalog(next);
      setStatusMsg("已一键恢复所有已改价的默认单价。");
    } catch (err) {
      setErrorMsg(String(err));
    } finally {
      setResettingOverrides(false);
    }
  };

  const catalogCounts = useMemo(() => {
    const entries = catalog?.effective ?? [];
    let overridden = 0;
    let pureCustom = 0;
    let modelsDev = 0;
    let builtin = 0;
    for (const e of entries) {
      if (isOverridden(e)) overridden += 1;
      else if (isPureCustom(e)) pureCustom += 1;
      else if (e.rate.source === "models_dev") modelsDev += 1;
      else builtin += 1;
    }
    return { total: entries.length, overridden, pureCustom, modelsDev, builtin };
  }, [catalog]);

  const filteredCatalog = useMemo(() => {
    const entries = catalog?.effective ?? [];
    const bySource =
      sourceFilter === "all"
        ? entries
        : sourceFilter === "overridden"
          ? entries.filter(isOverridden)
          : sourceFilter === "custom"
            ? entries.filter(isPureCustom)
            : entries.filter((e) => e.rate.source === sourceFilter);
    const q = filter.trim().toLowerCase();
    const list = q
      ? bySource.filter((e) => `${e.key} ${e.name ?? ""} ${entryTag(e).label}`.toLowerCase().includes(q))
      : bySource;
    const rank = (e: PriceCatalogEntry) => {
      if (isOverridden(e)) return 0;
      if (isPureCustom(e)) return 1;
      if (e.rate.source === "builtin") return 2;
      return 3;
    };
    const sorted = [...list].sort((a, b) => rank(a) - rank(b) || a.key.localeCompare(b.key));
    return q || sourceFilter !== "all" ? sorted.slice(0, 200) : sorted.slice(0, 60);
  }, [catalog, filter, sourceFilter]);

  const totalTokens = summary ? summary.total_input_tokens + summary.total_output_tokens : 0;
  const cacheHitPct =
    summary && summary.total_input_tokens > 0 ? Math.round((summary.total_cached_tokens / summary.total_input_tokens) * 100) : 0;
  const apiMoney = formatDualCurrency(
    summary?.api_cost_usd ?? 0,
    summary?.api_cost_cny ?? 0,
    summary?.api_total_usd_equiv ?? 0,
  );
  const subMoney = formatDualCurrency(
    summary?.subscription_equiv_usd ?? 0,
    summary?.subscription_equiv_cny ?? 0,
    summary?.subscription_total_usd_equiv ?? 0,
  );

  if (section === "pricing") {
    return (
      <section className="usage-ledger-section pricing-pane">
        <div className="usage-head">
          <h2>
            价格目录
            <Info>价格按「自定义 / 已改价 → models.dev 同步 → 内置表」三层解析，用于实时折算每次调用的 API 预估花费与订阅等效价值。</Info>
          </h2>
          <div className="usage-actions">
            {catalogCounts.overridden > 0 && (
              <button
                type="button"
                className="button outline small"
                disabled={resettingOverrides}
                onClick={handleResetOverrides}
                title="恢复所有被改价的 models.dev 与内置单价（不影响纯自定义模型）"
              >
                <Icon name="arrow-counterclockwise" size={12} />
                {resettingOverrides ? "恢复中…" : `一键恢复 (${catalogCounts.overridden})`}
              </button>
            )}
            <button
              type="button"
              className="button outline small"
              disabled={syncingCatalog}
              title={`上次同步：${formatSyncedAt(catalog?.synced_at)}`}
              onClick={handleSyncCatalog}
            >
              <Icon name="arrow-repeat" size={12} />
              {syncingCatalog ? "同步中…" : "同步 models.dev"}
            </button>
            <button type="button" className="button primary small" onClick={() => openEditor()}>
              <Icon name="plus-lg" size={12} />
              自定义价格
            </button>
          </div>
        </div>

        {statusMsg && <p className="hint">{statusMsg}</p>}
        {errorMsg && <p className="file-note failed">{errorMsg}</p>}

        {draft && (
          <form
            className="price-editor"
            onSubmit={(e) => {
              e.preventDefault();
              void handleSaveDraft();
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") setDraft(null);
            }}
          >
            <div className="price-editor-head">
              <strong>{draft.editing ? "编辑单价" : "新增自定义单价"}</strong>
              <span className="muted">单位：每百万 Token（/ 1M）</span>
            </div>
            <div className="price-editor-row-top">
              <label className="price-field">
                <span>模型标识</span>
                <input
                  className="input mono"
                  value={draft.key}
                  placeholder="provider/model 或 model，如 moonshot/kimi-k3.1-pro"
                  onChange={(e) => setDraft({ ...draft, key: e.target.value })}
                />
              </label>
              <label className="price-field">
                <span>币种</span>
                <select
                  className="input"
                  value={draft.currency}
                  onChange={(e) => setDraft({ ...draft, currency: e.target.value as Currency })}
                >
                  <option value="USD">USD ($)</option>
                  <option value="CNY">CNY (¥)</option>
                </select>
              </label>
            </div>
            <div className="price-editor-row-rates">
              {(
                [
                  ["输入", "input"],
                  ["缓存读", "cached"],
                  ["缓存写", "cacheWrite"],
                  ["输出", "output"],
                ] as const
              ).map(([label, field]) => (
                <label className="price-field" key={field}>
                  <span>{label}</span>
                  <div className="price-input-wrap">
                    <span className="price-currency-prefix">{draft.currency === "CNY" ? "¥" : "$"}</span>
                    <input
                      className="mono"
                      type="number"
                      min={0}
                      step="any"
                      value={draft[field]}
                      onChange={(e) => setDraft({ ...draft, [field]: e.target.value })}
                    />
                  </div>
                </label>
              ))}
            </div>
            <div className="price-editor-actions">
              <button type="button" className="button outline small" onClick={() => setDraft(null)}>
                取消
              </button>
              <button type="submit" className="button primary small" disabled={savingDraft}>
                <Icon name="check-lg" size={12} />
                {savingDraft ? "保存中…" : "保存单价"}
              </button>
            </div>
          </form>
        )}

        <div className="catalog-body">
          <div className="catalog-toolbar">
            <input
              className="input"
              type="search"
              placeholder="筛选模型或宗门（如 kimi、claude、deepseek）"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
            <div className="catalog-filter-pills" role="tablist" aria-label="按来源筛选价格">
              <button
                type="button"
                role="tab"
                className="ledger-pill"
                aria-selected={sourceFilter === "all"}
                onClick={() => setSourceFilter("all")}
              >
                全部 ({catalogCounts.total})
              </button>
              <button
                type="button"
                role="tab"
                className="ledger-pill is-overridden"
                aria-selected={sourceFilter === "overridden"}
                onClick={() => setSourceFilter("overridden")}
              >
                已改价 ({catalogCounts.overridden})
              </button>
              <button
                type="button"
                role="tab"
                className="ledger-pill is-custom"
                aria-selected={sourceFilter === "custom"}
                onClick={() => setSourceFilter("custom")}
              >
                自定义 ({catalogCounts.pureCustom})
              </button>
              <button
                type="button"
                role="tab"
                className="ledger-pill"
                aria-selected={sourceFilter === "models_dev"}
                onClick={() => setSourceFilter("models_dev")}
              >
                models.dev ({catalogCounts.modelsDev})
              </button>
              <button
                type="button"
                role="tab"
                className="ledger-pill"
                aria-selected={sourceFilter === "builtin"}
                onClick={() => setSourceFilter("builtin")}
              >
                内置 ({catalogCounts.builtin})
              </button>
            </div>
          </div>
          <div className="catalog-list">
            <div className="catalog-grid catalog-grid-head">
              <span>模型（/ 1M Token）</span>
              <span className="num">输入</span>
              <span className="num">缓存读</span>
              <span className="num">缓存写</span>
              <span className="num">输出</span>
              <span>操作</span>
            </div>
            {filteredCatalog.map((entry) => {
              const sym = entry.rate.currency === "CNY" ? "¥" : "$";
              const overridden = isOverridden(entry);
              const pureCustom = isPureCustom(entry);
              const tag = entryTag(entry);
              const rowCls = overridden
                ? "catalog-grid catalog-grid-row is-overridden"
                : pureCustom
                  ? "catalog-grid catalog-grid-row is-custom"
                  : "catalog-grid catalog-grid-row";
              return (
                <div className={rowCls} key={entry.key}>
                  <div className="catalog-model-cell">
                    <span className="mono catalog-model-key" title={entry.key}>{entry.key}</span>
                    <span className={`ledger-tag ${tag.cls}`}>{tag.label}</span>
                  </div>
                  <span className="mono num">
                    {sym}
                    {entry.rate.input_per_m}
                  </span>
                  <span className="mono num">
                    {sym}
                    {entry.rate.cached_per_m}
                  </span>
                  <span className="mono num">
                    {sym}
                    {entry.rate.cache_write_per_m}
                  </span>
                  <span className="mono num">
                    {sym}
                    {entry.rate.output_per_m}
                  </span>
                  <div className="catalog-row-actions">
                    <button
                      type="button"
                      className="catalog-icon-btn"
                      title="改价"
                      aria-label={`修改 ${entry.key} 单价`}
                      onClick={() => openEditor(entry)}
                    >
                      <Icon name="pencil" size={12} />
                    </button>
                    {overridden && (
                      <button
                        type="button"
                        className="catalog-icon-btn"
                        title="恢复默认价格"
                        aria-label={`恢复 ${entry.key} 默认价格`}
                        onClick={() => handleDeleteCustom(entry.key, true)}
                      >
                        <Icon name="arrow-counterclockwise" size={12} />
                      </button>
                    )}
                    {pureCustom && (
                      <button
                        type="button"
                        className="catalog-icon-btn danger"
                        title="删除自定义价格"
                        aria-label={`删除 ${entry.key} 自定义价格`}
                        onClick={() => handleDeleteCustom(entry.key, false)}
                      >
                        <Icon name="trash3" size={12} />
                      </button>
                    )}
                  </div>
                </div>
              );
            })}
            {filteredCatalog.length === 0 && <p className="muted ledger-empty">没有匹配的价格条目。</p>}
          </div>
        </div>
      </section>
    );
  }

  return (
    <section className="usage-ledger-section ledger-pane">
      <div className="usage-head">
        <h2>
          用量账本
          <Info>
            按每轮实际服务的模型记录流水（自动归因 auto 路由），区分 API 预估花费与订阅等效价值。
          </Info>
        </h2>
        <div className="usage-actions">
          <div className="ledger-window-pills" role="tablist" aria-label="统计时间窗口">
            {WINDOWS.map((w) => (
              <button
                key={w.label}
                type="button"
                role="tab"
                className="ledger-pill"
                aria-selected={days === w.days}
                onClick={() => setDays(w.days)}
              >
                {w.label}
              </button>
            ))}
          </div>
          <button type="button" className="button outline small" disabled={loadingLedger} onClick={() => loadLedger(days)}>
            <Icon name="arrow-repeat" size={12} />
            {loadingLedger ? "加载中…" : "刷新"}
          </button>
          <button type="button" className="button outline small danger-text" onClick={handleResetUsage}>
            <Icon name="trash3" size={12} />
            清空
          </button>
        </div>
      </div>

      {statusMsg && <p className="hint">{statusMsg}</p>}
      {errorMsg && <p className="file-note failed">{errorMsg}</p>}

      <div className="ledger-scroll-body">
      <div className="ledger-kpis">
        <div className="ledger-kpi">
          <span className="ledger-kpi-label">API 预估花费</span>
          <strong className="ledger-kpi-value mono">{apiMoney.primary}</strong>
          <span className="ledger-kpi-sub">{apiMoney.secondary ?? "按量计费 API 汇总"}</span>
        </div>
        <div className="ledger-kpi">
          <span className="ledger-kpi-label">订阅等效价值</span>
          <strong className="ledger-kpi-value mono">{subMoney.primary}</strong>
          <span className="ledger-kpi-sub">{subMoney.secondary ?? "OAuth / CLI / Magpie 折算"}</span>
        </div>
        <div className="ledger-kpi">
          <span className="ledger-kpi-label">Token 与缓存</span>
          <strong className="ledger-kpi-value mono">{shortCount(totalTokens)}</strong>
          <span className="ledger-kpi-sub">
            {summary?.total_requests ?? 0} 次调用 · 缓存命中 {cacheHitPct}%
            {(summary?.total_cache_write_tokens ?? 0) > 0 && ` · 写入 ${shortCount(summary!.total_cache_write_tokens)}`}
          </span>
        </div>
      </div>

      {summary && <UsageCharts summary={summary} windowDays={days} />}

      <div className="ledger-subtabs" role="tablist" aria-label="用量分组">
        <button type="button" role="tab" className="ledger-pill" aria-selected={groupTab === "model"} onClick={() => setGroupTab("model")}>
          按心法 ({summary?.by_model.length ?? 0})
        </button>
        <button type="button" role="tab" className="ledger-pill" aria-selected={groupTab === "provider"} onClick={() => setGroupTab("provider")}>
          按宗门 ({summary?.by_provider.length ?? 0})
        </button>
        <button type="button" role="tab" className="ledger-pill" aria-selected={groupTab === "session"} onClick={() => setGroupTab("session")}>
          按分身 ({summary?.by_session.length ?? 0})
        </button>
      </div>

      <div className="ledger-table">
        {groupTab === "model" &&
          (summary && summary.by_model.length > 0 ? (
            summary.by_model.map((row) => (
              <div className="ledger-row" key={`${row.provider}/${row.model}`}>
                <div className="ledger-row-main">
                  <span className="mono">{row.model}</span>
                  <span className="ledger-tag">{row.provider}</span>
                  <span className={`ledger-tag ${row.billing_mode}`}>{billingLabel(row.billing_mode)}</span>
                </div>
                <div className="ledger-row-metrics mono">
                  <span>{row.requests} 次</span>
                  <span>{shortCount(row.input_tokens + row.output_tokens)} tok</span>
                  <strong>
                    {row.native_currency
                      ? formatMoney(row.native_cost, row.native_currency)
                      : formatUsd(row.cost_usd_equiv)}
                  </strong>
                </div>
              </div>
            ))
          ) : (
            <p className="muted ledger-empty">所选时间窗口内暂无调用流水。</p>
          ))}

        {groupTab === "provider" &&
          (summary && summary.by_provider.length > 0 ? (
            summary.by_provider.map((row) => {
              const money =
                row.billing_mode === "subscription"
                  ? formatUsd(row.subscription_equiv_usd)
                  : formatDualCurrency(row.api_cost_usd, row.api_cost_cny, row.api_total_usd_equiv).primary;
              return (
                <div className="ledger-row" key={row.provider}>
                  <div className="ledger-row-main">
                    <span className="mono">{row.provider}</span>
                    <span className={`ledger-tag ${row.billing_mode}`}>{billingLabel(row.billing_mode)}</span>
                  </div>
                  <div className="ledger-row-metrics mono">
                    <span>{row.requests} 次</span>
                    <span>{shortCount(row.input_tokens + row.output_tokens)} tok</span>
                    <strong>{money}</strong>
                  </div>
                </div>
              );
            })
          ) : (
            <p className="muted ledger-empty">所选时间窗口内暂无宗门汇总。</p>
          ))}

        {groupTab === "session" &&
          (summary && summary.by_session.length > 0 ? (
            summary.by_session.map((row) => {
              const info = sessionInfoMap.get(row.session_id);
              return (
                <div className="ledger-row" key={row.session_id}>
                  <div className="ledger-row-main ledger-session-main">
                    {info ? (
                      <>
                        <span className="ledger-tag">{info.cave}</span>
                        <span className="ledger-session-title" title={`${info.cave} · ${info.title}`}>
                          {info.title}
                        </span>
                      </>
                    ) : (
                      <>
                        <span className="ledger-tag">已删分身</span>
                        <span className="mono muted">{row.session_id}</span>
                      </>
                    )}
                  </div>
                  <div className="ledger-row-metrics mono">
                    <span>{row.requests} 次</span>
                    <span>{shortCount(row.input_tokens + row.output_tokens)} tok</span>
                    <strong>{formatUsd(row.api_cost_usd + row.subscription_equiv_usd)}</strong>
                  </div>
                </div>
              );
            })
          ) : (
            <p className="muted ledger-empty">所选时间窗口内暂无分身账单。</p>
          ))}
      </div>
      </div>
    </section>
  );
}

import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { formatMoney, formatUsd, hydratePriceCatalog } from "../lib/pricing";
import { shortCount } from "../lib/fold";
import type { BillingMode, Currency, PriceCatalogEntry, PriceCatalogView, PriceSource, UsageLedgerSummary } from "../lib/types";
import { Info } from "./Hint";
import { UsageCharts } from "./UsageCharts";

type WindowOption = { label: string; days: number | null };
const WINDOWS: WindowOption[] = [
  { label: "今日", days: 1 },
  { label: "近 7 天", days: 7 },
  { label: "近 30 天", days: 30 },
  { label: "全部", days: null },
];

type GroupTab = "model" | "provider" | "session";

interface PriceDraft {
  key: string;
  currency: Currency;
  input: string;
  cached: string;
  cacheWrite: string;
  output: string;
}

function billingLabel(mode: BillingMode): string {
  if (mode === "subscription") return "订阅等效";
  if (mode === "local") return "本地";
  return "API";
}

function sourceLabel(source: PriceSource): string {
  if (source === "custom") return "自定义";
  if (source === "models_dev") return "models.dev";
  return "内置";
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

export function UsagePricingSection() {
  const [days, setDays] = useState<number | null>(7);
  const [groupTab, setGroupTab] = useState<GroupTab>("model");
  const [summary, setSummary] = useState<UsageLedgerSummary | null>(null);
  const [catalog, setCatalog] = useState<PriceCatalogView | null>(null);
  const [loadingLedger, setLoadingLedger] = useState(false);
  const [syncingCatalog, setSyncingCatalog] = useState(false);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [showCatalog, setShowCatalog] = useState(false);
  const [filter, setFilter] = useState("");
  const [draft, setDraft] = useState<PriceDraft | null>(null);
  const [savingDraft, setSavingDraft] = useState(false);

  const loadLedger = useCallback((winDays: number | null) => {
    setLoadingLedger(true);
    api
      .getUsageLedgerSummary(winDays)
      .then((res) => setSummary(res))
      .catch((err) => setErrorMsg(String(err)))
      .finally(() => setLoadingLedger(false));
  }, []);

  const loadCatalog = useCallback(() => {
    api
      .getPriceCatalog()
      .then((res) => {
        setCatalog(res);
        hydratePriceCatalog(res);
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    loadLedger(days);
  }, [days, loadLedger]);

  useEffect(() => {
    loadCatalog();
  }, [loadCatalog]);

  const handleSyncCatalog = async () => {
    setSyncingCatalog(true);
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      const next = await api.syncPriceCatalog();
      setCatalog(next);
      hydratePriceCatalog(next);
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
      });
    } else {
      setDraft({
        key: "",
        currency: "USD",
        input: "0",
        cached: "0",
        cacheWrite: "0",
        output: "0",
      });
    }
    setShowCatalog(true);
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
      setCatalog(next);
      hydratePriceCatalog(next);
      setDraft(null);
      setStatusMsg(`已保存自定义价格：${key}`);
    } catch (err) {
      setErrorMsg(String(err));
    } finally {
      setSavingDraft(false);
    }
  };

  const handleDeleteCustom = async (key: string) => {
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      const next = await api.deleteCustomPrice(key);
      setCatalog(next);
      hydratePriceCatalog(next);
      setStatusMsg(`已恢复默认价格：${key}`);
    } catch (err) {
      setErrorMsg(String(err));
    }
  };

  const filteredCatalog = useMemo(() => {
    const entries = catalog?.effective ?? [];
    const q = filter.trim().toLowerCase();
    const list = q
      ? entries.filter((e) => `${e.key} ${e.name ?? ""} ${e.rate.source}`.toLowerCase().includes(q))
      : entries;
    // Show custom entries first, then builtin, then models_dev (limit to 60 unless searching).
    const sorted = [...list].sort((a, b) => {
      const rank = (s: PriceSource) => (s === "custom" ? 0 : s === "builtin" ? 1 : 2);
      return rank(a.rate.source) - rank(b.rate.source) || a.key.localeCompare(b.key);
    });
    return q ? sorted.slice(0, 80) : sorted.slice(0, 40);
  }, [catalog, filter]);

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

  return (
    <section className="usage-ledger-section">
      <div className="usage-head">
        <h2>
          用量账本与价格目录
          <Info>
            按每轮实际服务的模型记录流水（自动归因 auto 路由），区分 API 预估花费与订阅等效价值；价格按「自定义 → models.dev 同步 → 内置表」三层解析。
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
          <button type="button" className="button text small" disabled={loadingLedger} onClick={() => loadLedger(days)}>
            {loadingLedger ? "加载中…" : "刷新"}
          </button>
          <button type="button" className="button text small" onClick={handleResetUsage}>
            清空
          </button>
        </div>
      </div>

      {statusMsg && <p className="hint">{statusMsg}</p>}
      {errorMsg && <p className="file-note failed">{errorMsg}</p>}

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
          按模型 ({summary?.by_model.length ?? 0})
        </button>
        <button type="button" role="tab" className="ledger-pill" aria-selected={groupTab === "provider"} onClick={() => setGroupTab("provider")}>
          按宗门 ({summary?.by_provider.length ?? 0})
        </button>
        <button type="button" role="tab" className="ledger-pill" aria-selected={groupTab === "session"} onClick={() => setGroupTab("session")}>
          按会话 ({summary?.by_session.length ?? 0})
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
            summary.by_session.map((row) => (
              <div className="ledger-row" key={row.session_id}>
                <div className="ledger-row-main">
                  <span className="mono">{row.session_id}</span>
                </div>
                <div className="ledger-row-metrics mono">
                  <span>{row.requests} 次</span>
                  <span>{shortCount(row.input_tokens + row.output_tokens)} tok</span>
                  <strong>{formatUsd(row.api_cost_usd + row.subscription_equiv_usd)}</strong>
                </div>
              </div>
            ))
          ) : (
            <p className="muted ledger-empty">所选时间窗口内暂无会话账单。</p>
          ))}
      </div>

      <div className="catalog-box">
        <div className="catalog-head">
          <div>
            <button type="button" className="button text small" onClick={() => setShowCatalog((v) => !v)}>
              {showCatalog ? "▾ 收起价格目录" : "▸ 管理价格目录与自定义单价"}
            </button>
            <span className="muted catalog-meta">
              {catalog
                ? `自定义 ${catalog.custom_count} 条 · models.dev ${catalog.models_dev_count} 条 · ${formatSyncedAt(catalog.synced_at)}`
                : "加载价格目录中…"}
            </span>
          </div>
          <div className="usage-actions">
            <button type="button" className="button text small" disabled={syncingCatalog} onClick={handleSyncCatalog}>
              {syncingCatalog ? "同步中…" : "同步 models.dev"}
            </button>
            <button type="button" className="button text small" onClick={() => openEditor()}>
              + 自定义价格
            </button>
          </div>
        </div>

        {draft && (
          <div className="price-editor">
            <div className="price-editor-grid">
              <label className="price-field">
                <span>模型标识（provider/model 或 model）</span>
                <input
                  className="input mono"
                  value={draft.key}
                  placeholder="例如 moonshot/kimi-k3.1-pro"
                  onChange={(e) => setDraft({ ...draft, key: e.target.value })}
                />
              </label>
              <label className="price-field short">
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
              <label className="price-field num">
                <span>输入 / 1M</span>
                <input className="input mono" type="number" min={0} step="0.01" value={draft.input} onChange={(e) => setDraft({ ...draft, input: e.target.value })} />
              </label>
              <label className="price-field num">
                <span>缓存读 / 1M</span>
                <input className="input mono" type="number" min={0} step="0.01" value={draft.cached} onChange={(e) => setDraft({ ...draft, cached: e.target.value })} />
              </label>
              <label className="price-field num">
                <span>缓存写 / 1M</span>
                <input className="input mono" type="number" min={0} step="0.01" value={draft.cacheWrite} onChange={(e) => setDraft({ ...draft, cacheWrite: e.target.value })} />
              </label>
              <label className="price-field num">
                <span>输出 / 1M</span>
                <input className="input mono" type="number" min={0} step="0.01" value={draft.output} onChange={(e) => setDraft({ ...draft, output: e.target.value })} />
              </label>
            </div>
            <div className="price-editor-actions">
              <button type="button" className="button primary small" disabled={savingDraft} onClick={handleSaveDraft}>
                {savingDraft ? "保存中…" : "保存单价"}
              </button>
              <button type="button" className="button text small" onClick={() => setDraft(null)}>
                取消
              </button>
            </div>
          </div>
        )}

        {showCatalog && (
          <div className="catalog-body">
            <input
              className="input"
              type="search"
              placeholder="筛选模型或宗门（如 kimi、claude、deepseek）"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
            <div className="catalog-list">
              {filteredCatalog.map((entry) => {
                const sym = entry.rate.currency === "CNY" ? "¥" : "$";
                return (
                  <div className="ledger-row" key={entry.key}>
                    <div className="ledger-row-main">
                      <span className="mono">{entry.key}</span>
                      <span className={`ledger-tag source-${entry.rate.source}`}>{sourceLabel(entry.rate.source)}</span>
                    </div>
                    <div className="ledger-row-metrics mono">
                      <span title="输入 / 缓存读 / 缓存写 / 输出（每百万 Token）">
                        {sym}
                        {entry.rate.input_per_m} / {sym}
                        {entry.rate.cached_per_m} / {sym}
                        {entry.rate.cache_write_per_m} / {sym}
                        {entry.rate.output_per_m}
                      </span>
                      <button type="button" className="button text small" onClick={() => openEditor(entry)}>
                        改价
                      </button>
                      {entry.rate.source === "custom" && (
                        <button type="button" className="button text small" onClick={() => handleDeleteCustom(entry.key)}>
                          恢复默认
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
              {filteredCatalog.length === 0 && <p className="muted ledger-empty">没有匹配的价格条目。</p>}
            </div>
          </div>
        )}
      </div>
    </section>
  );
}

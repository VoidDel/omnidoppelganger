import { useMemo, useState } from "react";
import { shortCount } from "../lib/fold";
import { formatMoney, formatUsd } from "../lib/pricing";
import type { UsageDaySummary, UsageLedgerSummary } from "../lib/types";

type TrendMetric = "cost" | "tokens" | "requests";

function formatLocalDay(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

function buildDaySeries(byDay: UsageDaySummary[], windowDays: number | null): UsageDaySummary[] {
  const map = new Map<string, UsageDaySummary>();
  for (const row of byDay) {
    map.set(row.day, row);
  }
  const span = windowDays === 30 ? 14 : windowDays === null ? Math.max(7, Math.min(30, byDay.length || 7)) : 7;
  if (windowDays === null && byDay.length >= 7) {
    return byDay.slice(-30);
  }
  const today = new Date();
  const series: UsageDaySummary[] = [];
  for (let i = span - 1; i >= 0; i--) {
    const d = new Date(today.getFullYear(), today.getMonth(), today.getDate() - i);
    const key = formatLocalDay(d);
    series.push(
      map.get(key) ?? {
        day: key,
        requests: 0,
        total_tokens: 0,
        api_cost_usd: 0,
        subscription_equiv_usd: 0,
      },
    );
  }
  return series;
}

function formatTick(metric: TrendMetric, val: number): string {
  if (val <= 0) return "0";
  if (metric === "cost") return formatUsd(val);
  if (metric === "tokens") return shortCount(Math.round(val));
  return String(Math.round(val));
}

export function UsageCharts({ summary, windowDays }: { summary: UsageLedgerSummary; windowDays: number | null }) {
  const [metric, setMetric] = useState<TrendMetric>("cost");
  const [hoveredIdx, setHoveredIdx] = useState<number | null>(null);

  const series = useMemo(() => buildDaySeries(summary.by_day, windowDays), [summary.by_day, windowDays]);

  const maxVal = useMemo(() => {
    let m = 0;
    for (const d of series) {
      const v = metric === "cost" ? d.api_cost_usd + d.subscription_equiv_usd : metric === "tokens" ? d.total_tokens : d.requests;
      if (v > m) m = v;
    }
    return m;
  }, [series, metric]);

  const activeDay = hoveredIdx !== null ? series[hoveredIdx] : null;

  // SVG chart geometry
  const W = 560;
  const H = 148;
  const padL = 46;
  const padR = 10;
  const padT = 10;
  const padB = 24;
  const plotW = W - padL - padR;
  const plotH = H - padT - padB;
  const slotW = series.length > 0 ? plotW / series.length : plotW;
  const barW = Math.max(8, Math.min(26, slotW * 0.56));
  const labelEvery = series.length > 14 ? 3 : series.length > 9 ? 2 : 1;

  // Token breakdown donut data
  const cachedRead = Math.min(summary.total_input_tokens, summary.total_cached_tokens);
  const cacheWrite = Math.min(Math.max(0, summary.total_input_tokens - cachedRead), summary.total_cache_write_tokens);
  const uncachedInput = Math.max(0, summary.total_input_tokens - cachedRead - cacheWrite);
  const outputTok = summary.total_output_tokens;
  const totalTok = uncachedInput + cachedRead + cacheWrite + outputTok;

  const tokenSegments = [
    { key: "uncached", label: "普通输入", value: uncachedInput, color: "var(--chart-1)" },
    { key: "cached", label: "缓存读取", value: cachedRead, color: "var(--chart-2)" },
    { key: "cache_write", label: "缓存写入", value: cacheWrite, color: "var(--chart-3)" },
    { key: "output", label: "模型输出", value: outputTok, color: "var(--chart-4)" },
  ];

  const R = 42;
  const C = 2 * Math.PI * R;
  let offsetAcc = 0;
  const donutArcs = tokenSegments.map((seg) => {
    const frac = totalTok > 0 ? seg.value / totalTok : 0;
    const len = frac * C;
    const dashOffset = -offsetAcc;
    offsetAcc += len;
    return { ...seg, frac, len, dashOffset };
  });

  // Top models distribution
  const topModels = summary.by_model.slice(0, 5);
  const modelMetricTotal = useMemo(() => {
    const costSum = topModels.reduce((acc, m) => acc + m.cost_usd_equiv, 0);
    if (costSum > 0) return { mode: "cost" as const, total: summary.api_total_usd_equiv + summary.subscription_total_usd_equiv };
    return { mode: "tokens" as const, total: Math.max(1, totalTok) };
  }, [topModels, summary.api_total_usd_equiv, summary.subscription_total_usd_equiv, totalTok]);

  return (
    <div className="usage-charts">
      <div className="chart-card">
        <div className="chart-card-head">
          <div className="chart-card-title">
            <span>每日用量趋势</span>
            {activeDay ? (
              <span className="chart-hover-readout mono">
                {activeDay.day} · {activeDay.requests} 次 · {shortCount(activeDay.total_tokens)} tok · API {formatUsd(activeDay.api_cost_usd)} · 订阅{" "}
                {formatUsd(activeDay.subscription_equiv_usd)}
              </span>
            ) : (
              <span className="chart-legend">
                {metric === "cost" ? (
                  <>
                    <span className="chart-legend-item">
                      <i style={{ background: "var(--chart-1)" }} />
                      API 费用
                    </span>
                    <span className="chart-legend-item">
                      <i style={{ background: "var(--chart-3)" }} />
                      订阅等效
                    </span>
                  </>
                ) : (
                  <span className="chart-legend-item">
                    <i style={{ background: "var(--chart-1)" }} />
                    {metric === "tokens" ? "每日 Token" : "每日调用次数"}
                  </span>
                )}
              </span>
            )}
          </div>
          <div className="ledger-window-pills" role="tablist" aria-label="趋势指标">
            <button type="button" role="tab" className="ledger-pill" aria-selected={metric === "cost"} onClick={() => setMetric("cost")}>
              费用
            </button>
            <button type="button" role="tab" className="ledger-pill" aria-selected={metric === "tokens"} onClick={() => setMetric("tokens")}>
              Token
            </button>
            <button type="button" role="tab" className="ledger-pill" aria-selected={metric === "requests"} onClick={() => setMetric("requests")}>
              次数
            </button>
          </div>
        </div>

        <svg className="trend-svg" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="每日用量统计图">
          {[0, 0.5, 1].map((ratio) => {
            const y = padT + plotH * (1 - ratio);
            const tickVal = maxVal * ratio;
            return (
              <g key={ratio}>
                <line x1={padL} y1={y} x2={W - padR} y2={y} className="chart-grid-line" />
                <text x={padL - 6} y={y + 3} textAnchor="end" className="chart-axis-text mono">
                  {formatTick(metric, tickVal)}
                </text>
              </g>
            );
          })}

          {series.map((day, idx) => {
            const cx = padL + idx * slotW + slotW / 2;
            const x = cx - barW / 2;
            const isHover = hoveredIdx === idx;

            let apiH = 0;
            let subH = 0;
            let singleH = 0;
            if (maxVal > 0) {
              if (metric === "cost") {
                apiH = (day.api_cost_usd / maxVal) * plotH;
                subH = (day.subscription_equiv_usd / maxVal) * plotH;
                if (day.api_cost_usd > 0 && apiH < 2) apiH = 2;
                if (day.subscription_equiv_usd > 0 && subH < 2) subH = 2;
              } else {
                const v = metric === "tokens" ? day.total_tokens : day.requests;
                singleH = (v / maxVal) * plotH;
                if (v > 0 && singleH < 2) singleH = 2;
              }
            }

            const yBase = padT + plotH;
            const showLabel = idx % labelEvery === 0 || idx === series.length - 1;

            return (
              <g
                key={day.day}
                className={`chart-bar-group ${isHover ? "hovered" : ""}`}
                onMouseEnter={() => setHoveredIdx(idx)}
                onMouseLeave={() => setHoveredIdx((cur) => (cur === idx ? null : cur))}
              >
                <rect x={padL + idx * slotW} y={padT} width={slotW} height={plotH} fill="transparent" />
                {metric === "cost" ? (
                  <>
                    {apiH > 0 && (
                      <rect
                        x={x}
                        y={yBase - apiH}
                        width={barW}
                        height={apiH}
                        rx={subH > 0 ? 0 : 3}
                        fill="var(--chart-1)"
                        opacity={isHover ? 1 : 0.88}
                      />
                    )}
                    {subH > 0 && (
                      <rect
                        x={x}
                        y={yBase - apiH - subH}
                        width={barW}
                        height={subH}
                        rx={3}
                        fill="var(--chart-3)"
                        opacity={isHover ? 1 : 0.88}
                      />
                    )}
                  </>
                ) : (
                  singleH > 0 && (
                    <rect
                      x={x}
                      y={yBase - singleH}
                      width={barW}
                      height={singleH}
                      rx={3}
                      fill="var(--chart-1)"
                      opacity={isHover ? 1 : 0.88}
                    />
                  )
                )}
                {showLabel && (
                  <text x={cx} y={H - 6} textAnchor="middle" className="chart-axis-text mono">
                    {day.day.slice(5)}
                  </text>
                )}
              </g>
            );
          })}
        </svg>
      </div>

      <div className="chart-split-grid">
        <div className="chart-card">
          <div className="chart-card-head">
            <span className="chart-card-title">Token 构成</span>
          </div>
          <div className="donut-wrap">
            <svg className="donut-svg" viewBox="0 0 112 112" role="img" aria-label="Token 构成环形图">
              <circle cx="56" cy="56" r={R} fill="none" stroke="var(--line)" strokeWidth="14" />
              {totalTok > 0 &&
                donutArcs.map(
                  (arc) =>
                    arc.len > 0 && (
                      <circle
                        key={arc.key}
                        cx="56"
                        cy="56"
                        r={R}
                        fill="none"
                        stroke={arc.color}
                        strokeWidth="14"
                        strokeDasharray={`${arc.len} ${C - arc.len}`}
                        strokeDashoffset={arc.dashOffset}
                        transform="rotate(-90 56 56)"
                      />
                    ),
                )}
              <text x="56" y="54" textAnchor="middle" className="donut-center-val mono">
                {totalTok > 0 ? shortCount(totalTok) : "0"}
              </text>
              <text x="56" y="68" textAnchor="middle" className="donut-center-sub">
                总 Token
              </text>
            </svg>
            <div className="donut-legend">
              {donutArcs.map((seg) => (
                <div className="donut-legend-row" key={seg.key}>
                  <span className="donut-legend-name">
                    <i style={{ background: seg.color }} />
                    {seg.label}
                  </span>
                  <span className="donut-legend-val mono">
                    {shortCount(seg.value)} ({Math.round(seg.frac * 100)}%)
                  </span>
                </div>
              ))}
            </div>
          </div>
        </div>

        <div className="chart-card">
          <div className="chart-card-head">
            <span className="chart-card-title">模型消耗占比 (Top 5)</span>
          </div>
          {topModels.length > 0 ? (
            <div className="dist-list">
              {topModels.map((row) => {
                const value = modelMetricTotal.mode === "cost" ? row.cost_usd_equiv : row.input_tokens + row.output_tokens;
                const pct = modelMetricTotal.total > 0 ? Math.min(100, Math.round((value / modelMetricTotal.total) * 100)) : 0;
                const displayVal =
                  modelMetricTotal.mode === "cost"
                    ? row.native_currency
                      ? formatMoney(row.native_cost, row.native_currency)
                      : formatUsd(row.cost_usd_equiv)
                    : `${shortCount(row.input_tokens + row.output_tokens)} tok`;
                const barColor = row.billing_mode === "subscription" ? "var(--chart-3)" : "var(--chart-1)";
                return (
                  <div className="dist-row" key={`${row.provider}/${row.model}`}>
                    <div className="dist-row-head">
                      <span className="dist-name mono" title={`${row.provider}/${row.model}`}>
                        {row.model}
                      </span>
                      <span className="dist-val mono">
                        {displayVal} · {pct}%
                      </span>
                    </div>
                    <div className="dist-track">
                      <span className="dist-fill" style={{ width: `${Math.max(value > 0 ? 4 : 0, pct)}%`, background: barColor }} />
                    </div>
                  </div>
                );
              })}
            </div>
          ) : (
            <p className="muted ledger-empty">暂无模型消耗记录。</p>
          )}
        </div>
      </div>
    </div>
  );
}

import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/api";
import { fileSize, type ToolItem } from "../lib/fold";
import type { JobSnapshot } from "../lib/types";
import type { QuotePayload } from "./Composer";

export interface AgentJobsPaneProps {
  sessionId: string;
  runs: ToolItem[];
  onSendToChat: (quote: QuotePayload) => void;
  onError: (message: string) => void;
}

/** Dedicated panel for the agent's live background commands and historical command runs. */
export function AgentJobsPane({ sessionId, runs, onSendToChat, onError }: AgentJobsPaneProps) {
  const [subTab, setSubTab] = useState<"jobs" | "runs">("jobs");
  const [jobs, setJobs] = useState<JobSnapshot[]>([]);
  const [openJobId, setOpenJobId] = useState<string | null>(null);
  const [jobLogs, setJobLogs] = useState<Record<string, string>>({});
  const [jobInputs, setJobInputs] = useState<Record<string, string>>({});
  const [jobBusy, setJobBusy] = useState<string | null>(null);

  const refreshJobs = useCallback(() => {
    api.listJobs(sessionId).then(setJobs, () => undefined);
  }, [sessionId]);

  useEffect(() => {
    setJobs([]);
    setOpenJobId(null);
    setJobLogs({});
    setJobInputs({});
  }, [sessionId]);

  useEffect(() => {
    refreshJobs();
    const interval = setInterval(refreshJobs, subTab === "jobs" ? 2000 : 5000);
    return () => clearInterval(interval);
  }, [refreshJobs, subTab, runs.length]);

  const loadJobLog = useCallback(
    (jobId: string) => {
      api.readJobOutput(sessionId, jobId).then(
        (view) => {
          setJobLogs((prev) => (prev[jobId] === view.output ? prev : { ...prev, [jobId]: view.output }));
          setJobs((prev) => prev.map((j) => (j.id === view.job.id ? view.job : j)));
        },
        () => undefined,
      );
    },
    [sessionId],
  );

  useEffect(() => {
    if (subTab !== "jobs" || !openJobId) return;
    loadJobLog(openJobId);
    const activeJob = jobs.find((j) => j.id === openJobId);
    if (!activeJob?.running) return;
    const interval = setInterval(() => loadJobLog(openJobId), 1200);
    return () => clearInterval(interval);
  }, [subTab, openJobId, loadJobLog, jobs]);

  const handleStopJob = async (jobId: string) => {
    setJobBusy(jobId);
    try {
      const updated = await api.stopJob(sessionId, jobId);
      setJobs((prev) => prev.map((j) => (j.id === updated.id ? updated : j)));
      loadJobLog(jobId);
    } catch (err) {
      onError(String(err));
    } finally {
      setJobBusy(null);
    }
  };

  const handleDismissJob = async (jobId: string) => {
    try {
      await api.dismissJob(sessionId, jobId);
      setJobs((prev) => prev.filter((j) => j.id !== jobId));
      if (openJobId === jobId) setOpenJobId(null);
    } catch (err) {
      onError(String(err));
    }
  };

  const handleSendJobInput = async (jobId: string) => {
    const text = jobInputs[jobId] ?? "";
    if (!text) return;
    try {
      await api.writeJobInput(sessionId, jobId, `${text}\n`);
      setJobInputs((prev) => ({ ...prev, [jobId]: "" }));
      setTimeout(() => loadJobLog(jobId), 150);
    } catch (err) {
      onError(String(err));
    }
  };

  const runningCount = jobs.filter((j) => j.running).length;

  return (
    <div className="panel-body agent-jobs-pane">
      <div className="panel-head">
        <h2>分身任务与后台</h2>
        <div className="agent-jobs-switch" role="tablist" aria-label="分身任务视图">
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "jobs"}
            className={`term-log-switch ${subTab === "jobs" ? "active" : ""}`}
            onClick={() => setSubTab("jobs")}
          >
            {runningCount > 0 && <span className="job-live-dot" aria-hidden="true" />}
            后台进程{runningCount > 0 ? ` (${runningCount})` : jobs.length > 0 ? ` (${jobs.length})` : ""}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "runs"}
            className={`term-log-switch ${subTab === "runs" ? "active" : ""}`}
            onClick={() => setSubTab("runs")}
          >
            命令记录{runs.length > 0 ? ` (${runs.length})` : ""}
          </button>
        </div>
      </div>

      {subTab === "jobs" ? (
        <div className="term-jobs">
          {jobs.length === 0 ? (
            <p className="muted">当前会话没有后台运行中的命令。分身通过后台模式启动开发服务器或监听进程后会显示在这里。</p>
          ) : (
            jobs.map((job) => {
              const expanded = openJobId === job.id;
              const logText = jobLogs[job.id] ?? "";
              const failed = !job.running && !job.stopped && job.exit_code !== null && job.exit_code !== 0;
              const statusLabel = job.running
                ? "运行中"
                : job.stopped
                  ? "已停止"
                  : job.exit_code === 0
                    ? "已完成"
                    : `退出 ${job.exit_code ?? "?"}`;
              return (
                <div className={`term-job-card ${expanded ? "open" : ""}`} key={job.id}>
                  <div className="term-job-head">
                    <button
                      type="button"
                      className="term-job-toggle"
                      title={job.command}
                      onClick={() => setOpenJobId(expanded ? null : job.id)}
                    >
                      <span className={`job-dot ${job.running ? "running" : failed ? "failed" : "ended"}`} aria-hidden="true" />
                      <span className="term-job-id mono">#{job.id}</span>
                      <span className="term-job-cmd mono">{job.command}</span>
                    </button>
                    <span className="term-job-meta">
                      {job.pid ? <span className="muted mono">PID {job.pid}</span> : null}
                      <span className={`term-run-status ${job.running ? "running" : failed ? "failed" : "muted"}`}>{statusLabel}</span>
                      {job.running ? (
                        <button
                          type="button"
                          className="button danger small"
                          disabled={jobBusy === job.id}
                          onClick={() => void handleStopJob(job.id)}
                        >
                          {jobBusy === job.id ? "停止中…" : "停止"}
                        </button>
                      ) : (
                        <button
                          type="button"
                          className="button text small"
                          title="从列表移除此已结束任务"
                          onClick={() => void handleDismissJob(job.id)}
                        >
                          清除
                        </button>
                      )}
                    </span>
                  </div>
                  {expanded && (
                    <div className="term-job-body">
                      <div className="term-job-toolbar">
                        <span className="muted">{fileSize(job.output_bytes)} 输出</span>
                        <div className="term-job-toolbar-acts">
                          <button type="button" className="button text small" onClick={() => loadJobLog(job.id)}>
                            刷新日志
                          </button>
                          {logText.trim() && (
                            <button
                              type="button"
                              className="button text small"
                              onClick={() =>
                                onSendToChat({
                                  kind: "job",
                                  jobId: job.id,
                                  command: job.command,
                                  content: logText.slice(-6000),
                                })
                              }
                            >
                              发给分身
                            </button>
                          )}
                        </div>
                      </div>
                      <pre className="terminal term-job-log">{logText || "（暂无输出）"}</pre>
                      {job.running && (
                        <form
                          className="term-job-stdin"
                          onSubmit={(e) => {
                            e.preventDefault();
                            void handleSendJobInput(job.id);
                          }}
                        >
                          <input
                            className="input mono"
                            placeholder="向后台任务发送一行标准输入（Enter 发送）…"
                            value={jobInputs[job.id] ?? ""}
                            onChange={(e) => setJobInputs((prev) => ({ ...prev, [job.id]: e.target.value }))}
                          />
                          <button type="submit" className="button outline small" disabled={!(jobInputs[job.id] ?? "")}>
                            发送
                          </button>
                        </form>
                      )}
                    </div>
                  )}
                </div>
              );
            })
          )}
        </div>
      ) : (
        <div className="term-runs">
          {runs.length === 0 && <p className="muted">分身还没有运行过命令。</p>}
          {runs.map((run) => {
            const meta = run.result!.meta!;
            const cmdLine = `${meta.cwd ? `[${meta.cwd}] ` : ""}${meta.command ?? ""}`.replace(/\s+/g, " ").trim();
            const failed = Boolean(meta.stopped) || (!meta.background && !meta.running && meta.exit_code !== 0);
            const outputLines = meta.output ? meta.output.replace(/\n$/, "").split("\n").length : 0;
            const badge = meta.stopped
              ? meta.stopped === "timed out"
                ? "超时"
                : "已停止"
              : run.name === "run_command" && meta.background
                ? `后台 #${meta.job}`
                : meta.running
                  ? "运行中"
                  : meta.exit_code !== 0
                    ? `退出 ${meta.exit_code ?? "?"}`
                    : outputLines > 0
                      ? `${outputLines} 行`
                      : "成功";
            return (
              <details className="term-run" key={run.id}>
                <summary className="term-run-head" title={cmdLine}>
                  <span className="term-run-chevron" aria-hidden="true">
                    ▸
                  </span>
                  <span className="prompt">$</span>
                  <span className="term-run-cmd">{cmdLine}</span>
                  <span className={`term-run-status ${failed ? "failed" : "muted"}`}>{badge}</span>
                </summary>
                <pre className="terminal term-run-body">
                  <span className="prompt">$</span> {meta.cwd ? `[${meta.cwd}] ` : ""}
                  {meta.command}
                  {run.name === "run_command" && meta.background ? <span className="muted">{`\n[后台运行，编号 ${meta.job}]`}</span> : null}
                  {run.name !== "run_command" ? <span className="muted">{`  （${meta.job}${run.name === "stop_command" ? "，停止" : ""}）`}</span> : null}
                  {meta.output ? `\n${meta.output}` : ""}
                  {meta.stopped ? <span className="failed">{`\n[${meta.stopped === "timed out" ? "超时停止" : "已停止"}]`}</span> : null}
                  {!meta.stopped && !meta.background && !meta.running && meta.exit_code !== 0 ? (
                    <span className="failed">{`\n[退出码 ${meta.exit_code ?? "未知"}]`}</span>
                  ) : null}
                </pre>
              </details>
            );
          })}
        </div>
      )}
    </div>
  );
}

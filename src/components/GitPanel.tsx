import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import type { DiffStage, GitBranch, GitCommitItem, GitFile, GitStatus, SessionItem } from "../lib/types";

export interface GitPanelProps {
  sessionId: string;
  isWorktree: boolean;
  status: GitStatus | null;
  refreshTick?: number;
  working: boolean;
  /** With the status an action handed back, or without one to have it read again. */
  onStatusChanged: (next?: GitStatus) => void;
  /** Files on disk changed by an action here (discarding): what shows them is read again. */
  onFilesChanged?: () => void;
  onBranchSwitched: (item: SessionItem) => void;
  onOpenDiff: (path: string, stage: DiffStage, pin: boolean, commit?: string) => void;
  onOpenFile: (path: string, pin: boolean) => void;
  onAskReview: () => void;
  onNote: (note: string) => void;
  onError: (error: string) => void;
}

function splitPath(relPath: string): { name: string; dir: string } {
  const cut = relPath.lastIndexOf("/");
  return cut >= 0 ? { name: relPath.slice(cut + 1), dir: relPath.slice(0, cut) } : { name: relPath, dir: "" };
}

function shortDate(unixSecs: number): string {
  if (!unixSecs) return "";
  const d = new Date(unixSecs * 1000);
  return d.toLocaleDateString([], { month: "2-digit", day: "2-digit" });
}

export function GitPanel({
  sessionId,
  isWorktree,
  status,
  refreshTick = 0,
  working,
  onStatusChanged,
  onFilesChanged,
  onBranchSwitched,
  onOpenDiff,
  onOpenFile,
  onAskReview,
  onNote,
  onError,
}: GitPanelProps) {
  const [commitMsg, setCommitMsg] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  // Branch popover / drawer
  const [branchMenuOpen, setBranchMenuOpen] = useState(false);
  const [branches, setBranches] = useState<GitBranch[]>([]);
  const [newBranchName, setNewBranchName] = useState("");
  const [creatingBranch, setCreatingBranch] = useState(false);

  // Pull Request inline drawer
  const [prOpen, setPrOpen] = useState(false);
  const [prTitle, setPrTitle] = useState("");
  const [prBody, setPrBody] = useState("");
  const [prBase, setPrBase] = useState("main");
  const [prDraft, setPrDraft] = useState(false);
  const [prHasGh, setPrHasGh] = useState(true);

  // Discard confirmation target (`"*"` = all unstaged, else single relative path)
  const [discardTarget, setDiscardTarget] = useState<string | null>(null);

  // Section folding
  const [folded, setFolded] = useState<Record<string, boolean>>({ history: false });
  const toggleSection = (key: string) => setFolded((prev) => ({ ...prev, [key]: !prev[key] }));

  // Recent commit log & expanded commit files
  const [commits, setCommits] = useState<GitCommitItem[]>([]);
  const [openCommit, setOpenCommit] = useState<string | null>(null);
  const [commitFiles, setCommitFiles] = useState<Record<string, GitFile[]>>({});

  const loadLog = useCallback(() => {
    if (!status?.is_repo) return;
    // The same history keeps the list as it is, with what is expanded in it.
    api.gitLog(sessionId, 25).then(
      (next) => setCommits((shown) => (shown.length === next.length && shown.every((c, at) => c.hash === next[at].hash && c.refs === next[at].refs) ? shown : next)),
      () => undefined,
    );
  }, [sessionId, status?.is_repo]);

  // At once for another repository or branch; after a change on disk, once the burst of changes is over.
  const logKey = useRef<string | null>(null);
  useEffect(() => {
    const key = `${sessionId}\u0000${status?.branch ?? ""}`;
    const fresh = logKey.current !== key;
    logKey.current = key;
    const timer = setTimeout(loadLog, fresh ? 0 : 600);
    return () => clearTimeout(timer);
  }, [loadLog, refreshTick, sessionId, status?.branch]);

  /** After discarding, the files themselves changed too: the tree and what is open are read again. */
  const afterDiscard = (next: GitStatus) => {
    onStatusChanged(next);
    onFilesChanged?.();
  };

  const openBranches = () => {
    const next = !branchMenuOpen;
    setBranchMenuOpen(next);
    setCreatingBranch(false);
    setNewBranchName("");
    if (next) {
      api.gitBranches(sessionId).then(setBranches, (err) => onError(String(err)));
    }
  };

  const switchBranch = async (name: string, create = false) => {
    if (!name.trim() || busy) return;
    setBusy("checkout");
    try {
      const updated = await api.gitCheckout(sessionId, name.trim(), create);
      setBranchMenuOpen(false);
      setCreatingBranch(false);
      setNewBranchName("");
      onBranchSwitched(updated);
      onStatusChanged();
      loadLog();
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(null);
    }
  };

  const runSync = async (action: "fetch" | "pull" | "push") => {
    if (busy) return;
    setBusy(action);
    try {
      const msg = await api.gitSync(sessionId, action);
      onStatusChanged();
      loadLog();
      const label = action === "fetch" ? "已抓取远端更新" : action === "pull" ? "已拉取远端提交" : "已推送到远端";
      onNote(msg ? `${label}：${msg.split("\n")[0]}` : `${label}。`);
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(null);
    }
  };

  const openPr = () => {
    const next = !prOpen;
    setPrOpen(next);
    if (next) {
      setBranchMenuOpen(false);
      api.gitPrDefaults(sessionId).then(
        (d) => {
          setPrTitle(d.title);
          setPrBody(d.body);
          setPrBase(d.base);
          setPrHasGh(d.has_gh);
        },
        (err) => {
          setPrOpen(false);
          onError(String(err));
        },
      );
    }
  };

  const submitPr = async () => {
    const title = prTitle.trim();
    if (!title || busy) return;
    setBusy("pr");
    try {
      const res = await api.gitCreatePr(sessionId, title, prBody, prBase.trim() || null, prDraft);
      setPrOpen(false);
      onStatusChanged();
      loadLog();
      onNote(res.url ? `已创建 PR：${res.url}` : `已为分支 ${res.branch} 创建 PR。`);
      if (res.url && /^https?:\/\//i.test(res.url)) {
        void api.openExternal(res.url).catch(() => undefined);
      }
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(null);
    }
  };

  const doCommit = async (stageAll: boolean) => {
    const msg = commitMsg.trim();
    if (!msg || busy) return;
    setBusy("commit");
    try {
      const res = await api.gitCommit(sessionId, msg, stageAll);
      setCommitMsg("");
      onStatusChanged();
      loadLog();
      onNote(`已提交 ${res.hash}${res.branch ? `（${res.branch}）` : ""}。`);
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(null);
    }
  };

  if (!status) {
    return <p className="muted explorer-empty">正在读取 Git 状态…</p>;
  }

  if (!status.is_repo) {
    return (
      <div className="git-empty">
        <p className="muted">当前洞府还不是 Git 仓库。</p>
        <p className="muted">可以在右侧终端运行 <span className="mono">git init</span>，或让分身帮忙初始化。</p>
      </div>
    );
  }

  const totalDirty = status.conflicts.length + status.staged.length + status.unstaged.length + status.untracked.length;
  const canCommitStaged = status.staged.length > 0;
  const canCommitAll = !canCommitStaged && status.unstaged.length + status.untracked.length > 0 && status.conflicts.length === 0;

  const toggleCommit = (hash: string) => {
    if (openCommit === hash) {
      setOpenCommit(null);
      return;
    }
    setOpenCommit(hash);
    if (!commitFiles[hash]) {
      api.gitCommitFiles(sessionId, hash).then(
        (list) => setCommitFiles((prev) => ({ ...prev, [hash]: list })),
        (err) => onError(String(err)),
      );
    }
  };

  const renderFileRow = (file: GitFile, group: "conflict" | "staged" | "unstaged" | "untracked") => {
    const { name, dir } = splitPath(file.path);
    const tone =
      file.status === "U"
        ? "conflict"
        : file.status === "A" || file.status === "?"
          ? "added"
          : file.status === "D"
            ? "deleted"
            : "modified";
    const diffStage: DiffStage = group === "staged" ? "staged" : "unstaged";

    return (
      <div key={`${group}:${file.path}`} className="git-item-wrap">
        <div className="tree-row git-file-row">
          <button
            type="button"
            className="tree-main"
            title={`${file.path}${file.orig_path ? ` ← ${file.orig_path}` : ""}`}
            onClick={() => onOpenDiff(file.path, diffStage, false)}
            onDoubleClick={() => onOpenDiff(file.path, diffStage, true)}
          >
            <span className={`tree-git ${tone}`}>{file.status === "?" ? "U" : file.status}</span>
            <span className="tree-label">{name}</span>
            {dir && <span className="tree-sub">{dir}</span>}
          </button>
          <span className="tree-acts">
            {file.status !== "D" && (
              <button
                type="button"
                className="icon-button tiny"
                title="打开源文件"
                onClick={(e) => {
                  e.stopPropagation();
                  onOpenFile(file.path, false);
                }}
              >
                ↗
              </button>
            )}
            {group === "staged" ? (
              <button
                type="button"
                className="icon-button tiny"
                title="取消暂存"
                disabled={!!busy}
                onClick={(e) => {
                  e.stopPropagation();
                  api.gitUnstage(sessionId, [file.path]).then(onStatusChanged, (err) => onError(String(err)));
                }}
              >
                −
              </button>
            ) : (
              <>
                <button
                  type="button"
                  className="icon-button tiny"
                  title="暂存此文件"
                  disabled={!!busy}
                  onClick={(e) => {
                    e.stopPropagation();
                    api.gitStage(sessionId, [file.path]).then(onStatusChanged, (err) => onError(String(err)));
                  }}
                >
                  +
                </button>
                {group !== "conflict" && (
                  <button
                    type="button"
                    className="icon-button tiny danger"
                    title={group === "untracked" ? "删除未跟踪文件" : "放弃工作区改动"}
                    disabled={!!busy}
                    onClick={(e) => {
                      e.stopPropagation();
                      setDiscardTarget(discardTarget === file.path ? null : file.path);
                    }}
                  >
                    ↺
                  </button>
                )}
              </>
            )}
          </span>
        </div>
        {discardTarget === file.path && (
          <div className="tree-confirm" role="alertdialog">
            <span>{group === "untracked" ? `删除未跟踪的「${name}」？` : `放弃「${name}」尚未暂存的改动？不可撤销。`}</span>
            <span className="tree-confirm-acts">
              <button type="button" className="button text small" onClick={() => setDiscardTarget(null)}>
                取消
              </button>
              <button
                type="button"
                className="button danger small"
                onClick={() => {
                  const target = file.path;
                  setDiscardTarget(null);
                  api.gitDiscard(sessionId, [target]).then(afterDiscard, (err) => onError(String(err)));
                }}
              >
                确认放弃
              </button>
            </span>
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="git-panel">
      {/* Branch & Remote Sync Header */}
      <div className="git-head">
        <button
          type="button"
          className="git-branch-btn"
          disabled={isWorktree}
          title={isWorktree ? "独立工作树分身固定在其专属分支上" : "点击切换或新建 Git 分支"}
          onClick={openBranches}
        >
          <svg aria-hidden="true" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <circle cx="4.5" cy="3.5" r="1.7" />
            <circle cx="4.5" cy="12.5" r="1.7" />
            <circle cx="11.5" cy="5.5" r="1.7" />
            <path d="M4.5 5.2v5.6M11.5 7.2c0 2.6-7 1.6-7 3.6" />
          </svg>
          <span className="git-branch-name mono">
            {status.branch ?? "尚无提交"}
            {status.detached ? " (游离)" : ""}
          </span>
        </button>
        <div className="git-sync-tools">
          <button
            type="button"
            className="git-sync-btn"
            disabled={!!busy}
            title="抓取远端分支状态 (git fetch)"
            aria-label="抓取远端 (git fetch)"
            onClick={() => void runSync("fetch")}
          >
            <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M13.2 8A5.2 5.2 0 1 1 11.8 4.4M13.2 2.6v2.8h-2.8" />
            </svg>
          </button>
          <button
            type="button"
            className="git-sync-btn"
            disabled={!!busy}
            title={`拉取远端更新 (git pull --ff-only)${status.behind ? `：落后 ${status.behind} 个提交` : ""}`}
            onClick={() => void runSync("pull")}
          >
            ↓{status.behind > 0 ? status.behind : ""}
          </button>
          <button
            type="button"
            className="git-sync-btn"
            disabled={!!busy}
            title={`推送到远端 (git push)${status.ahead ? `：领先 ${status.ahead} 个提交` : ""}`}
            onClick={() => void runSync("push")}
          >
            ↑{status.ahead > 0 ? status.ahead : ""}
          </button>
          <button
            type="button"
            className={`git-sync-btn ${prOpen ? "active" : ""}`}
            disabled={!!busy || !status.branch || status.detached}
            title="一键推送当前分支并创建 GitHub PR (gh pr create)"
            onClick={openPr}
          >
            PR
          </button>
        </div>
      </div>

      {prOpen && (
        <div className="git-branches-box">
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 6, marginBottom: 6 }}>
            <span className="muted" style={{ fontSize: "var(--t-micro)" }}>
              <span className="mono">{status.branch}</span> → <span className="mono">{prBase || "main"}</span>
            </span>
            <label style={{ display: "inline-flex", alignItems: "center", gap: 4, fontSize: "var(--t-micro)", color: "var(--ink-2)", cursor: "pointer" }}>
              <input type="checkbox" checked={prDraft} onChange={(e) => setPrDraft(e.target.checked)} />
              草稿 PR
            </label>
          </div>
          {!prHasGh && (
            <p className="muted" style={{ fontSize: "var(--t-micro)", color: "var(--cinnabar-ink)", margin: "0 0 6px" }}>
              未检测到本机 <span className="mono">gh</span> CLI，请先安装并运行 <span className="mono">gh auth login</span>。
            </p>
          )}
          <input
            className="input tree-input"
            style={{ width: "100%", marginBottom: 6 }}
            placeholder="PR 标题…"
            value={prTitle}
            onChange={(e) => setPrTitle(e.target.value)}
          />
          <textarea
            className="input"
            rows={4}
            style={{ width: "100%", fontSize: "var(--t-small)", marginBottom: 6, resize: "vertical" }}
            placeholder="PR 改动说明（Markdown）…"
            value={prBody}
            onChange={(e) => setPrBody(e.target.value)}
          />
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <input
              className="input tree-input mono"
              style={{ width: 96 }}
              placeholder="目标分支"
              title="目标 base 分支"
              value={prBase}
              onChange={(e) => setPrBase(e.target.value)}
            />
            <button type="button" className="button text small" onClick={() => setPrOpen(false)}>
              取消
            </button>
            <button
              type="button"
              className="button primary small"
              style={{ marginLeft: "auto" }}
              disabled={!!busy || !prTitle.trim() || !prHasGh}
              onClick={() => void submitPr()}
            >
              {busy === "pr" ? "创建中…" : "推送并创建 PR"}
            </button>
          </div>
        </div>
      )}

      {branchMenuOpen && !isWorktree && (
        <div className="git-branches-box">
          {creatingBranch ? (
            <div className="tree-inline-edit">
              <input
                className="input tree-input"
                autoFocus
                placeholder="新分支名称…"
                value={newBranchName}
                onChange={(e) => setNewBranchName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.nativeEvent.isComposing) void switchBranch(newBranchName, true);
                  if (e.key === "Escape") setCreatingBranch(false);
                }}
              />
              <button type="button" className="button primary small" onClick={() => void switchBranch(newBranchName, true)}>
                创建
              </button>
            </div>
          ) : (
            <button type="button" className="git-new-branch" onClick={() => setCreatingBranch(true)}>
              + 基于当前提交新建分支…
            </button>
          )}
          <div className="git-branch-list">
            {branches.map((b) => (
              <button
                key={`${b.remote ? "r" : "l"}:${b.name}`}
                type="button"
                disabled={b.current || b.locked_elsewhere || !!busy}
                className={`git-branch-item ${b.current ? "current" : ""}`}
                title={b.locked_elsewhere ? "此分支已在另一个工作树检出" : b.name}
                onClick={() => void switchBranch(b.name, false)}
              >
                <span className="mono">{b.name}</span>
                {b.current && <span className="chip done">当前</span>}
                {b.remote && <span className="chip">远端</span>}
                {b.locked_elsewhere && <span className="chip">占用</span>}
              </button>
            ))}
          </div>
        </div>
      )}

      {/* Commit Box */}
      <div className="git-commit-box">
        <textarea
          className="input git-commit-input"
          rows={2}
          placeholder="写一句提交说明 (Ctrl/Cmd+Enter 提交)…"
          value={commitMsg}
          onChange={(e) => setCommitMsg(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && commitMsg.trim()) {
              e.preventDefault();
              if (canCommitStaged) void doCommit(false);
              else if (canCommitAll) void doCommit(true);
            }
          }}
        />
        <div className="git-commit-acts">
          <button
            type="button"
            className="button primary small git-commit-submit"
            disabled={!commitMsg.trim() || (!canCommitStaged && !canCommitAll) || !!busy}
            onClick={() => void doCommit(!canCommitStaged && canCommitAll)}
          >
            {busy === "commit"
              ? "正在提交…"
              : canCommitStaged
                ? `提交暂存 (${status.staged.length})`
                : canCommitAll
                  ? `全部暂存并提交 (${status.unstaged.length + status.untracked.length})`
                  : "提交"}
          </button>
          {totalDirty > 0 && (
            <button
              type="button"
              className="button outline small"
              disabled={working}
              title={working ? "分身正在运功" : "让分身审查当前尚未提交的改动"}
              onClick={onAskReview}
            >
              请分身审查
            </button>
          )}
        </div>
      </div>

      {/* Scrollable Change Groups & History */}
      <div className="explorer-scroll git-scroll">
        {totalDirty === 0 && <p className="muted explorer-empty">工作区干干净净，没有未提交改动。</p>}

        {status.conflicts.length > 0 && (
          <div className="git-group">
            <div className="git-group-head">
              <button type="button" className="git-group-title" onClick={() => toggleSection("conflicts")}>
                <span>合并冲突 ({status.conflicts.length})</span>
              </button>
            </div>
            {!folded.conflicts && status.conflicts.map((f) => renderFileRow(f, "conflict"))}
          </div>
        )}

        {status.staged.length > 0 && (
          <div className="git-group">
            <div className="git-group-head">
              <button type="button" className="git-group-title" onClick={() => toggleSection("staged")}>
                <span>已暂存 ({status.staged.length})</span>
              </button>
              <button
                type="button"
                className="icon-button tiny"
                title="全部取消暂存"
                onClick={() => api.gitUnstage(sessionId, []).then(onStatusChanged, (err) => onError(String(err)))}
              >
                −
              </button>
            </div>
            {!folded.staged && status.staged.map((f) => renderFileRow(f, "staged"))}
          </div>
        )}

        {status.unstaged.length > 0 && (
          <div className="git-group">
            <div className="git-group-head">
              <button type="button" className="git-group-title" onClick={() => toggleSection("unstaged")}>
                <span>更改 ({status.unstaged.length})</span>
              </button>
              <span className="git-group-acts">
                <button
                  type="button"
                  className="icon-button tiny"
                  title="全部暂存"
                  onClick={() =>
                    api
                      .gitStage(
                        sessionId,
                        status.unstaged.map((f) => f.path),
                      )
                      .then(onStatusChanged, (err) => onError(String(err)))
                  }
                >
                  +
                </button>
                <button
                  type="button"
                  className="icon-button tiny danger"
                  title="放弃全部已跟踪文件的改动"
                  onClick={() => setDiscardTarget(discardTarget === "*" ? null : "*")}
                >
                  ↺
                </button>
              </span>
            </div>
            {discardTarget === "*" && (
              <div className="tree-confirm" role="alertdialog">
                <span>放弃全部 {status.unstaged.length} 个已跟踪文件的未暂存改动？不可撤销。</span>
                <span className="tree-confirm-acts">
                  <button type="button" className="button text small" onClick={() => setDiscardTarget(null)}>
                    取消
                  </button>
                  <button
                    type="button"
                    className="button danger small"
                    onClick={() => {
                      const paths = status.unstaged.map((f) => f.path);
                      setDiscardTarget(null);
                      api.gitDiscard(sessionId, paths).then(afterDiscard, (err) => onError(String(err)));
                    }}
                  >
                    全部放弃
                  </button>
                </span>
              </div>
            )}
            {!folded.unstaged && status.unstaged.map((f) => renderFileRow(f, "unstaged"))}
          </div>
        )}

        {status.untracked.length > 0 && (
          <div className="git-group">
            <div className="git-group-head">
              <button type="button" className="git-group-title" onClick={() => toggleSection("untracked")}>
                <span>
                  未跟踪 ({status.untracked.length}
                  {status.untracked_truncated ? "+" : ""})
                </span>
              </button>
              <button
                type="button"
                className="icon-button tiny"
                title="暂存全部新文件"
                onClick={() =>
                  api
                    .gitStage(
                      sessionId,
                      status.untracked.map((f) => f.path),
                    )
                    .then(onStatusChanged, (err) => onError(String(err)))
                }
              >
                +
              </button>
            </div>
            {!folded.untracked && status.untracked.map((f) => renderFileRow(f, "untracked"))}
          </div>
        )}

        {commits.length > 0 && (
          <div className="git-group git-history">
            <div className="git-group-head">
              <button type="button" className="git-group-title" onClick={() => toggleSection("history")}>
                <span>最近提交 ({commits.length})</span>
              </button>
            </div>
            {!folded.history &&
              commits.map((c) => {
                const expanded = openCommit === c.hash;
                const touched = commitFiles[c.hash] ?? [];
                return (
                  <div key={c.hash} className="git-commit-row">
                    <button
                      type="button"
                      className={`git-commit-head ${expanded ? "open" : ""}`}
                      title={`${c.subject}\n${c.author} · ${c.short_hash}`}
                      onClick={() => toggleCommit(c.hash)}
                    >
                      <span className="git-commit-subject">{c.subject}</span>
                      <span className="git-commit-meta mono">
                        {c.short_hash} · {shortDate(c.timestamp)}
                      </span>
                    </button>
                    {expanded && (
                      <div className="git-commit-files">
                        {touched.length === 0 ? (
                          <span className="muted git-commit-loading">读取文件列表…</span>
                        ) : (
                          touched.map((f) => {
                            const { name, dir } = splitPath(f.path);
                            return (
                              <button
                                key={f.path}
                                type="button"
                                className="tree-main git-commit-file"
                                title={f.path}
                                onClick={() => onOpenDiff(f.path, "commit", false, c.short_hash)}
                                onDoubleClick={() => onOpenDiff(f.path, "commit", true, c.short_hash)}
                              >
                                <span className="tree-git modified">{f.status}</span>
                                <span className="tree-label">{name}</span>
                                {dir && <span className="tree-sub">{dir}</span>}
                              </button>
                            );
                          })
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
          </div>
        )}
      </div>
    </div>
  );
}

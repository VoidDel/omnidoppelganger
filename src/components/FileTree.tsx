import "../styles/workbench.css";
import { type KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import { useMenuFocus } from "../lib/menuFocus";
import { sameEntries } from "../lib/refresh";
import type { FoundFile, TreeEntry } from "../lib/types";
import { Icon } from "./Icon";

export interface GitBadge {
  label: string;
  tone: "added" | "modified" | "deleted" | "conflict";
}

export interface FileTreeProps {
  sessionId: string;
  workspace: string;
  activePath: string | null;
  refreshTick?: number;
  gitBadges?: Record<string, GitBadge>;
  onOpenFile: (relPath: string, pin: boolean) => void;
  onAttachFile: (absPath: string, relPath: string) => void;
  onAttachToAside?: (absPath: string, relPath: string) => void;
  onDeletedPath?: (relPath: string) => void;
  onRenamedPath?: (fromRel: string, toRel: string) => void;
  onError: (message: string) => void;
}

const IGNORED_PREF_KEY = "odg.explorer.showIgnored";

function joinWorkspacePath(workspace: string, relPath: string): string {
  const sep = workspace.includes("\\") ? "\\" : "/";
  const base = workspace.replace(/[\\/]+$/, "");
  const sub = relPath.replace(/\//g, sep);
  return sub ? `${base}${sep}${sub}` : base;
}

function parentOf(relPath: string): string {
  const idx = relPath.lastIndexOf("/");
  return idx >= 0 ? relPath.slice(0, idx) : "";
}

interface FlatNode {
  entry: TreeEntry;
  depth: number;
}

export function FileTree({
  sessionId,
  workspace,
  activePath,
  refreshTick = 0,
  gitBadges,
  onOpenFile,
  onAttachFile,
  onAttachToAside,
  onDeletedPath,
  onRenamedPath,
  onError,
}: FileTreeProps) {
  const [showIgnored, setShowIgnored] = useState<boolean>(() => {
    try {
      return localStorage.getItem(IGNORED_PREF_KEY) !== "false";
    } catch {
      return true;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(IGNORED_PREF_KEY, String(showIgnored));
    } catch {
      // Ignore storage errors
    }
  }, [showIgnored]);

  const [entriesByDir, setEntriesByDir] = useState<Record<string, TreeEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [selectedPath, setSelectedPath] = useState<string>("");
  const [selectedIsDir, setSelectedIsDir] = useState<boolean>(false);
  const [loadingRoot, setLoadingRoot] = useState<boolean>(false);

  const [query, setQuery] = useState("");
  const [found, setFound] = useState<FoundFile[] | null>(null);
  const [searching, setSearching] = useState(false);

  const [creating, setCreating] = useState<{ parentDir: string; isDir: boolean; name: string } | null>(null);
  const [renaming, setRenaming] = useState<{ entry: TreeEntry; name: string } | null>(null);
  const [deleting, setDeleting] = useState<TreeEntry | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    target: { kind: "entry"; entry: TreeEntry } | { kind: "found"; item: FoundFile } | { kind: "root" };
  } | null>(null);
  const createInputRef = useRef<HTMLInputElement>(null);
  const renameInputRef = useRef<HTMLInputElement>(null);
  const contextMenuRef = useRef<HTMLDivElement>(null);
  const treeRef = useRef<HTMLDivElement>(null);
  useMenuFocus(!!contextMenu, contextMenuRef, () => setContextMenu(null));

  useEffect(() => {
    if (!contextMenu) return;
    const onPointerDown = (event: PointerEvent) => {
      if (contextMenuRef.current?.contains(event.target as Node)) return;
      setContextMenu(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setContextMenu(null);
    };
    const onBlur = () => setContextMenu(null);
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("blur", onBlur);
    };
  }, [contextMenu]);

  const openContextMenu = (
    event: React.MouseEvent,
    target: { kind: "entry"; entry: TreeEntry } | { kind: "found"; item: FoundFile } | { kind: "root" },
  ) => {
    event.preventDefault();
    event.stopPropagation();
    const menuWidth = 196;
    const menuHeight = target.kind === "root" ? 160 : 280;
    const x = Math.min(event.clientX, Math.max(8, window.innerWidth - menuWidth - 8));
    const y = Math.min(event.clientY, Math.max(8, window.innerHeight - menuHeight - 8));
    if (target.kind === "entry") {
      setSelectedPath(target.entry.path);
      setSelectedIsDir(target.entry.expandable);
    }
    setContextMenu({ x, y, target });
  };

  const copyText = (value: string) => {
    navigator.clipboard.writeText(value).catch((err) => onError(`复制失败：${String(err)}`));
  };

  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;

  const loadDir = useCallback(
    async (dir: string) => {
      const list = await api.listWorkspaceDir(sessionId, dir, true, showIgnored);
      setEntriesByDir((prev) => ({ ...prev, [dir]: list }));
      return list;
    },
    [sessionId, showIgnored],
  );

  const reloadAll = useCallback(
    async (quiet = false) => {
      if (!quiet) setLoadingRoot(true);
      try {
        const dirs = ["", ...expandedRef.current];
        const results = await Promise.all(
          dirs.map((dir) =>
            api.listWorkspaceDir(sessionId, dir, true, showIgnored).then(
              (list) => [dir, list] as const,
              () => null,
            ),
          ),
        );
        // A directory that reads the same keeps its old listing, and a tree where nothing changed is not drawn again.
        setEntriesByDir((shown) => {
          const next: Record<string, TreeEntry[]> = {};
          let changed = Object.keys(shown).length !== results.filter(Boolean).length;
          for (const pair of results) {
            if (!pair) continue;
            const [dir, list] = pair;
            if (sameEntries(shown[dir], list)) next[dir] = shown[dir];
            else {
              next[dir] = list;
              changed = true;
            }
          }
          return changed ? next : shown;
        });
      } catch (error) {
        onError(String(error));
      } finally {
        if (!quiet) setLoadingRoot(false);
      }
    },
    [sessionId, showIgnored, onError],
  );

  // Reset expansion when switching to a different workspace folder; reload at once for a new folder or filter,
  // and once a burst of changes on disk is over for the rest.
  const prevWorkspace = useRef(workspace);
  const lastReload = useRef<typeof reloadAll | null>(null);
  useEffect(() => {
    let quiet = lastReload.current === reloadAll;
    if (prevWorkspace.current !== workspace) {
      prevWorkspace.current = workspace;
      quiet = false;
      setExpanded(new Set());
      setSelectedPath("");
      setSelectedIsDir(false);
      setCreating(null);
      setRenaming(null);
      setDeleting(null);
    }
    lastReload.current = reloadAll;
    const timer = setTimeout(() => void reloadAll(quiet), quiet ? 250 : 0);
    return () => clearTimeout(timer);
  }, [workspace, reloadAll, refreshTick]);

  // Fuzzy file search when query is entered.
  useEffect(() => {
    const trimmed = query.trim();
    if (!trimmed) {
      setFound(null);
      setSearching(false);
      return;
    }
    let alive = true;
    setSearching(true);
    const timer = setTimeout(() => {
      api.searchWorkspaceFiles(sessionId, trimmed, 100).then(
        (items) => {
          if (!alive) return;
          setFound(items);
          setSearching(false);
        },
        (error) => {
          if (!alive) return;
          setSearching(false);
          onError(String(error));
        },
      );
    }, 150);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [query, sessionId, refreshTick, onError]);

  useEffect(() => {
    if (creating) createInputRef.current?.focus();
  }, [creating]);

  useEffect(() => {
    if (renaming) {
      const el = renameInputRef.current;
      if (el) {
        el.focus();
        const dot = renaming.entry.kind === "file" ? renaming.name.lastIndexOf(".") : -1;
        el.setSelectionRange(0, dot > 0 ? dot : renaming.name.length);
      }
    }
  }, [renaming]);

  const toggleDir = (entry: TreeEntry) => {
    setSelectedPath(entry.path);
    setSelectedIsDir(true);
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(entry.path)) {
        next.delete(entry.path);
      } else {
        next.add(entry.path);
        void loadDir(entry.path).catch((err) => onError(String(err)));
      }
      return next;
    });
  };

  const startCreate = (isDir: boolean, targetParent?: string) => {
    const parentDir = targetParent !== undefined ? targetParent : selectedIsDir ? selectedPath : selectedPath ? parentOf(selectedPath) : "";
    if (parentDir && !expanded.has(parentDir)) {
      setExpanded((prev) => new Set([...prev, parentDir]));
      void loadDir(parentDir).catch((err) => onError(String(err)));
    }
    setRenaming(null);
    setDeleting(null);
    setCreating({ parentDir, isDir, name: "" });
  };

  const submitCreate = async () => {
    if (!creating) return;
    const name = creating.name.trim();
    if (!name) {
      setCreating(null);
      return;
    }
    const rel = creating.parentDir ? `${creating.parentDir}/${name}` : name;
    try {
      const created = await api.createWorkspaceEntry(sessionId, rel, creating.isDir);
      setCreating(null);
      await loadDir(creating.parentDir);
      if (!creating.isDir) {
        onOpenFile(created, true);
      } else {
        setExpanded((prev) => new Set([...prev, created]));
        await loadDir(created);
      }
    } catch (error) {
      onError(String(error));
    }
  };

  const submitRename = async () => {
    if (!renaming) return;
    const name = renaming.name.trim();
    if (!name || name === renaming.entry.name) {
      setRenaming(null);
      return;
    }
    const parent = parentOf(renaming.entry.path);
    const toRel = parent ? `${parent}/${name}` : name;
    try {
      const updated = await api.renameWorkspaceEntry(sessionId, renaming.entry.path, toRel);
      const oldPath = renaming.entry.path;
      setRenaming(null);
      if (expanded.has(oldPath)) {
        setExpanded((prev) => {
          const next = new Set<string>();
          for (const item of prev) {
            if (item === oldPath) next.add(updated);
            else if (item.startsWith(`${oldPath}/`)) next.add(`${updated}${item.slice(oldPath.length)}`);
            else next.add(item);
          }
          return next;
        });
      }
      onRenamedPath?.(oldPath, updated);
      await reloadAll();
    } catch (error) {
      onError(String(error));
    }
  };

  const confirmDelete = async (entry: TreeEntry) => {
    try {
      await api.deleteWorkspaceEntry(sessionId, entry.path);
      setDeleting(null);
      if (selectedPath === entry.path) {
        setSelectedPath("");
        setSelectedIsDir(false);
      }
      onDeletedPath?.(entry.path);
      await reloadAll();
    } catch (error) {
      onError(String(error));
    }
  };

  // Flatten visible tree nodes in DFS order.
  const flatNodes = useMemo(() => {
    const out: FlatNode[] = [];
    const walk = (dir: string, depth: number) => {
      const list = entriesByDir[dir] ?? [];
      for (const entry of list) {
        out.push({ entry, depth });
        if (entry.expandable && expanded.has(entry.path)) {
          walk(entry.path, depth + 1);
        }
      }
    };
    walk("", 0);
    return out;
  }, [entriesByDir, expanded]);

  const focusTreePath = (targetPath: string, isDir: boolean) => {
    setSelectedPath(targetPath);
    setSelectedIsDir(isDir);
    const applyFocus = () => {
      const items = treeRef.current?.querySelectorAll<HTMLButtonElement>('button[role="treeitem"]');
      if (!items) return false;
      for (const btn of items) {
        if (btn.dataset.treePath === targetPath) {
          btn.focus();
          btn.scrollIntoView({ block: "nearest" });
          return true;
        }
      }
      return false;
    };
    if (!applyFocus()) {
      requestAnimationFrame(() => {
        applyFocus();
      });
    }
  };

  const handleTreeKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (found !== null || flatNodes.length === 0) return;
    const target = event.target as HTMLElement | null;
    if (!target || target.getAttribute("role") !== "treeitem") return;
    const focusedPath = target.dataset.treePath ?? selectedPath;
    const idx = flatNodes.findIndex((n) => n.entry.path === focusedPath);
    const curIdx = idx >= 0 ? idx : 0;
    const cur = flatNodes[curIdx];
    if (!cur) return;

    if (event.key === "ArrowDown") {
      event.preventDefault();
      const next = flatNodes[Math.min(curIdx + 1, flatNodes.length - 1)];
      if (next) focusTreePath(next.entry.path, next.entry.expandable);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      const prev = flatNodes[Math.max(curIdx - 1, 0)];
      if (prev) focusTreePath(prev.entry.path, prev.entry.expandable);
    } else if (event.key === "Home") {
      event.preventDefault();
      const first = flatNodes[0];
      if (first) focusTreePath(first.entry.path, first.entry.expandable);
    } else if (event.key === "End") {
      event.preventDefault();
      const last = flatNodes[flatNodes.length - 1];
      if (last) focusTreePath(last.entry.path, last.entry.expandable);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      if (cur.entry.expandable) {
        if (!expanded.has(cur.entry.path)) {
          toggleDir(cur.entry);
        } else {
          const child = flatNodes[curIdx + 1];
          if (child && child.depth === cur.depth + 1) {
            focusTreePath(child.entry.path, child.entry.expandable);
          }
        }
      }
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      if (cur.entry.expandable && expanded.has(cur.entry.path)) {
        toggleDir(cur.entry);
      } else {
        const parent = parentOf(cur.entry.path);
        if (parent) {
          const parentNode = flatNodes.find((n) => n.entry.path === parent);
          if (parentNode) focusTreePath(parentNode.entry.path, true);
        }
      }
    }
  };

  // Propagate child git badges onto collapsed folders so modified files inside closed folders remain visible.
  const dirBadges = useMemo(() => {
    if (!gitBadges) return {};
    const out: Record<string, GitBadge["tone"]> = {};
    const rank: Record<GitBadge["tone"], number> = { conflict: 4, modified: 3, added: 2, deleted: 1 };
    for (const [path, badge] of Object.entries(gitBadges)) {
      let cur = parentOf(path);
      while (cur) {
        const prev = out[cur];
        if (!prev || rank[badge.tone] > rank[prev]) out[cur] = badge.tone;
        cur = parentOf(cur);
      }
    }
    return out;
  }, [gitBadges]);

  const renderCreateRow = (depth: number) => {
    if (!creating) return null;
    return (
      <div className="tree-inline-edit" style={{ paddingLeft: `${depth * 14 + 10}px` }}>
        <span className="tree-kind-label">{creating.isDir ? "新目录" : "新文件"}</span>
        <input
          ref={createInputRef}
          className="input tree-input"
          value={creating.name}
          placeholder={creating.isDir ? "文件夹名称" : "文件名称"}
          onChange={(e) => setCreating({ ...creating, name: e.target.value })}
          onBlur={() => void submitCreate()}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void submitCreate();
            }
            if (e.key === "Escape") {
              e.preventDefault();
              setCreating(null);
            }
          }}
        />
      </div>
    );
  };

  return (
    <div className="explorer">
      <div className="explorer-toolbar">
        <input
          className="input explorer-search"
          type="search"
          aria-label="搜索洞府文件"
          placeholder="搜文件名…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="explorer-tools" role="toolbar" aria-label="文件操作">
          <button type="button" className="icon-button small" title="新建文件" onClick={() => startCreate(false)}>
            <Icon name="file-earmark-plus" size={14} />
            <span className="sr-only">新建文件</span>
          </button>
          <button type="button" className="icon-button small" title="新建文件夹" onClick={() => startCreate(true)}>
            <Icon name="folder-plus" size={14} />
            <span className="sr-only">新建文件夹</span>
          </button>
          <button
            type="button"
            className={`icon-button small ${showIgnored ? "" : "active"}`}
            aria-pressed={!showIgnored}
            title={showIgnored ? "当前显示被 Git 忽略的文件，点此隐藏" : "当前已隐藏 Git 忽略文件，点此显示"}
            onClick={() => setShowIgnored((v) => !v)}
          >
            <Icon name={showIgnored ? "eye" : "eye-slash"} size={14} />
            <span className="sr-only">{showIgnored ? "隐藏忽略项" : "显示忽略项"}</span>
          </button>
          <button type="button" className="icon-button small" title="收起全部目录" onClick={() => setExpanded(new Set())}>
            <Icon name="arrows-collapse" size={14} />
            <span className="sr-only">收起全部</span>
          </button>
          <button type="button" className="icon-button small" title="刷新文件列表" onClick={() => void reloadAll()}>
            <Icon name="arrow-clockwise" size={14} />
            <span className="sr-only">刷新</span>
          </button>
        </div>
      </div>

      <div
        ref={treeRef}
        className="explorer-scroll"
        role="tree"
        aria-label="洞府文件树"
        onKeyDown={handleTreeKeyDown}
        onScroll={() => {
          if (contextMenu) setContextMenu(null);
        }}
        onContextMenu={(e) => openContextMenu(e, { kind: "root" })}
      >
        {found !== null ? (
          found.length === 0 ? (
            <p className="muted explorer-empty">{searching ? "正在搜寻…" : "没找到匹配的文件。"}</p>
          ) : (
            found.map((item) => (
              <div
                key={item.path}
                className={`tree-row search-hit ${activePath === item.path ? "current" : ""}`}
                onContextMenu={(e) => openContextMenu(e, { kind: "found", item })}
              >
                <button
                  type="button"
                  className="tree-main"
                  title={item.path}
                  onClick={() => onOpenFile(item.path, false)}
                  onDoubleClick={() => onOpenFile(item.path, true)}
                >
                  <span className="tree-label">{item.name}</span>
                  {item.dir && <span className="tree-sub">{item.dir}</span>}
                </button>
                <span className="tree-acts">
                  <button
                    type="button"
                    className="icon-button tiny"
                    title="附入对话"
                    onClick={() => onAttachFile(joinWorkspacePath(workspace, item.path), item.path)}
                  >
                    @
                  </button>
                </span>
              </div>
            ))
          )
        ) : (
          <>
            {creating && creating.parentDir === "" && renderCreateRow(0)}
            {flatNodes.length === 0 && !creating ? (
              <p className="muted explorer-empty">{loadingRoot ? "正在读取目录…" : "这个文件夹还是空的。"}</p>
            ) : (
              flatNodes.map(({ entry, depth }) => {
                const isOpen = entry.expandable && expanded.has(entry.path);
                const isCurrent = activePath === entry.path;
                const isSelected = selectedPath === entry.path;
                const badge = gitBadges?.[entry.path];
                const folderTone = !isOpen && entry.expandable ? dirBadges[entry.path] : undefined;

                if (renaming?.entry.path === entry.path) {
                  return (
                    <div key={entry.path} className="tree-inline-edit" style={{ paddingLeft: `${depth * 14 + 10}px` }}>
                      <input
                        ref={renameInputRef}
                        className="input tree-input"
                        value={renaming.name}
                        onChange={(e) => setRenaming({ ...renaming, name: e.target.value })}
                        onBlur={() => void submitRename()}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" && !e.nativeEvent.isComposing) {
                            e.preventDefault();
                            void submitRename();
                          }
                          if (e.key === "Escape") {
                            e.preventDefault();
                            setRenaming(null);
                          }
                        }}
                      />
                    </div>
                  );
                }

                return (
                  <div key={entry.path} className="tree-item-group">
                    <div
                      className={[
                        "tree-row",
                        isCurrent && "current",
                        isSelected && "selected",
                        entry.ignored && "ignored",
                        badge && `git-${badge.tone}`,
                      ]
                        .filter(Boolean)
                        .join(" ")}
                      style={{ paddingLeft: `${depth * 14 + 6}px` }}
                      onContextMenu={(e) => openContextMenu(e, { kind: "entry", entry })}
                    >
                      <button
                        type="button"
                        role="treeitem"
                        data-tree-path={entry.path}
                        tabIndex={
                          selectedPath
                            ? isSelected
                              ? 0
                              : -1
                            : activePath
                              ? isCurrent
                                ? 0
                                : -1
                              : flatNodes[0]?.entry.path === entry.path
                                ? 0
                                : -1
                        }
                        aria-level={depth + 1}
                        aria-expanded={entry.expandable ? isOpen : undefined}
                        aria-selected={isCurrent || isSelected}
                        className="tree-main"
                        title={entry.path}
                        onClick={() => {
                          if (entry.expandable) {
                            toggleDir(entry);
                          } else {
                            setSelectedPath(entry.path);
                            setSelectedIsDir(false);
                            onOpenFile(entry.path, false);
                          }
                        }}
                        onDoubleClick={() => {
                          if (!entry.expandable) onOpenFile(entry.path, true);
                        }}
                      >
                        {entry.expandable ? (
                          <Icon name="chevron-right" size={10} className={`tree-caret ${isOpen ? "open" : ""}`} />
                        ) : (
                          <span className="tree-dot" aria-hidden="true" />
                        )}
                        <span className="tree-label">{entry.name}</span>
                        {badge && <span className={`tree-git ${badge.tone}`}>{badge.label}</span>}
                        {!badge && folderTone && <span className={`tree-folder-dot ${folderTone}`} aria-hidden="true" />}
                      </button>
                      <span className="tree-acts">
                        {entry.expandable ? (
                          <button
                            type="button"
                            className="icon-button tiny"
                            title="在此文件夹内新建文件"
                            onClick={(e) => {
                              e.stopPropagation();
                              startCreate(false, entry.path);
                            }}
                          >
                            +
                          </button>
                        ) : (
                          <button
                            type="button"
                            className="icon-button tiny"
                            title="附入对话"
                            onClick={(e) => {
                              e.stopPropagation();
                              onAttachFile(joinWorkspacePath(workspace, entry.path), entry.path);
                            }}
                          >
                            @
                          </button>
                        )}
                        <button
                          type="button"
                          className="icon-button tiny"
                          title="重命名"
                          onClick={(e) => {
                            e.stopPropagation();
                            setCreating(null);
                            setDeleting(null);
                            setRenaming({ entry, name: entry.name });
                          }}
                        >
                          ✎
                        </button>
                        <button
                          type="button"
                          className="icon-button tiny"
                          title="在文件夹中显示"
                          onClick={(e) => {
                            e.stopPropagation();
                            api.openWorkspaceEntry(sessionId, entry.path, true).catch((err) => onError(String(err)));
                          }}
                        >
                          ↗
                        </button>
                        <button
                          type="button"
                          className="icon-button tiny danger"
                          title="删除"
                          onClick={(e) => {
                            e.stopPropagation();
                            setCreating(null);
                            setRenaming(null);
                            setDeleting(deleting?.path === entry.path ? null : entry);
                          }}
                        >
                          ×
                        </button>
                      </span>
                    </div>
                    {deleting?.path === entry.path && (
                      <div className="tree-confirm" role="alertdialog" aria-label={`删除 ${entry.name}`}>
                        <span>删掉「{entry.name}」{entry.expandable ? "及其中所有内容" : ""}？</span>
                        <span className="tree-confirm-acts">
                          <button type="button" className="button text small" onClick={() => setDeleting(null)}>
                            取消
                          </button>
                          <button type="button" className="button danger small" onClick={() => void confirmDelete(entry)}>
                            删除
                          </button>
                        </span>
                      </div>
                    )}
                    {creating && creating.parentDir === entry.path && isOpen && renderCreateRow(depth + 1)}
                  </div>
                );
              })
            )}
          </>
        )}
      </div>
      {contextMenu && (
        <div
          ref={contextMenuRef}
          className="tree-context-menu"
          style={{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }}
          role="menu"
          aria-label="文件操作菜单"
        >
          {contextMenu.target.kind === "root" ? (
            <>
              <button
                type="button"
                role="menuitem"
                className="tree-context-item"
                onClick={() => {
                  setContextMenu(null);
                  startCreate(false, "");
                }}
              >
                新建文件
              </button>
              <button
                type="button"
                role="menuitem"
                className="tree-context-item"
                onClick={() => {
                  setContextMenu(null);
                  startCreate(true, "");
                }}
              >
                新建文件夹
              </button>
              <div className="tree-context-sep" role="separator" />
              <button
                type="button"
                role="menuitem"
                className="tree-context-item"
                onClick={() => {
                  setContextMenu(null);
                  copyText(workspace);
                }}
              >
                复制洞府绝对路径
              </button>
              <button
                type="button"
                role="menuitem"
                className="tree-context-item"
                onClick={() => {
                  setContextMenu(null);
                  void reloadAll();
                }}
              >
                刷新文件列表
              </button>
            </>
          ) : (() => {
            const target = contextMenu.target;
            const relPath = target.kind === "entry" ? target.entry.path : target.item.path;
            const isDir = target.kind === "entry" ? target.entry.expandable : false;
            const absPath = joinWorkspacePath(workspace, relPath);
            const entry = target.kind === "entry" ? target.entry : null;
            return (
              <>
                {isDir ? (
                  <>
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        setContextMenu(null);
                        startCreate(false, relPath);
                      }}
                    >
                      新建文件
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        setContextMenu(null);
                        startCreate(true, relPath);
                      }}
                    >
                      新建文件夹
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    role="menuitem"
                    className="tree-context-item"
                    onClick={() => {
                      setContextMenu(null);
                      onOpenFile(relPath, true);
                    }}
                  >
                    打开文件
                  </button>
                )}
                <button
                  type="button"
                  role="menuitem"
                  className="tree-context-item"
                  onClick={() => {
                    setContextMenu(null);
                    onAttachFile(absPath, relPath);
                  }}
                >
                  附入对话
                </button>
                {onAttachToAside && (
                  <button
                    type="button"
                    role="menuitem"
                    className="tree-context-item"
                    onClick={() => {
                      setContextMenu(null);
                      onAttachToAside(absPath, relPath);
                    }}
                  >
                    附入旁聊
                  </button>
                )}
                <div className="tree-context-sep" role="separator" />
                <button
                  type="button"
                  role="menuitem"
                  className="tree-context-item"
                  onClick={() => {
                    setContextMenu(null);
                    copyText(relPath);
                  }}
                >
                  复制相对路径
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="tree-context-item"
                  onClick={() => {
                    setContextMenu(null);
                    copyText(absPath);
                  }}
                >
                  复制绝对路径
                </button>
                <div className="tree-context-sep" role="separator" />
                <button
                  type="button"
                  role="menuitem"
                  className="tree-context-item"
                  onClick={() => {
                    setContextMenu(null);
                    api.openWorkspaceEntry(sessionId, relPath, true).catch((err) => onError(String(err)));
                  }}
                >
                  在文件夹中显示
                </button>
                {entry && (
                  <>
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item"
                      onClick={() => {
                        setContextMenu(null);
                        setCreating(null);
                        setDeleting(null);
                        setRenaming({ entry, name: entry.name });
                      }}
                    >
                      重命名
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="tree-context-item danger"
                      onClick={() => {
                        setContextMenu(null);
                        setCreating(null);
                        setRenaming(null);
                        setDeleting(entry);
                      }}
                    >
                      删除
                    </button>
                  </>
                )}
              </>
            );
          })()}
        </div>
      )}
    </div>
  );
}

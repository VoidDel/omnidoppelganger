import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renameWorkbenchTabs } from "../hooks/useWorkbenchState";
import { api } from "../lib/api";
import { makeMockGitStatus, makeMockSessionItem } from "../test/mockApi";
import { DiffViewer } from "./DiffViewer";
import { GitPanel, type GitPanelProps } from "./GitPanel";
import type { WorkbenchTab } from "./Workbench";

function renderGitPanel(overrides: Partial<GitPanelProps> = {}) {
  const props: GitPanelProps = {
    sessionId: "sess-git",
    workspace: "/ws/repo",
    isWorktree: false,
    status: makeMockGitStatus(),
    working: false,
    onStatusChanged: vi.fn(),
    onFilesChanged: vi.fn(),
    onBranchSwitched: vi.fn(),
    onOpenDiff: vi.fn(),
    onOpenFile: vi.fn(),
    onAskReview: vi.fn(),
    onNote: vi.fn(),
    onError: vi.fn(),
    ...overrides,
  };
  return { ...render(<GitPanel {...props} />), props };
}

describe("GitPanel & DiffViewer multi-repo / merge / UTF-8 guards (P1-15 & P2-3)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.spyOn(api, "gitLog").mockResolvedValue([]);
  });

  it("stages, unstages, and confirms single-file discard", async () => {
    const nextStatus = makeMockGitStatus({ staged: [{ path: "src/a.ts", orig_path: null, status: "M" }] });
    const stageSpy = vi.spyOn(api, "gitStage").mockResolvedValue(nextStatus);
    const unstageSpy = vi.spyOn(api, "gitUnstage").mockResolvedValue(makeMockGitStatus());
    const discardSpy = vi.spyOn(api, "gitDiscard").mockResolvedValue(makeMockGitStatus());

    const { rerender, props } = renderGitPanel({
      status: makeMockGitStatus({
        staged: [{ path: "src/staged.ts", orig_path: null, status: "M" }],
        unstaged: [{ path: "src/dirty.ts", orig_path: null, status: "M" }],
      }),
    });

    fireEvent.click(screen.getByTitle("取消暂存"));
    await waitFor(() => expect(unstageSpy).toHaveBeenCalledWith("sess-git", ["src/staged.ts"], undefined));

    fireEvent.click(screen.getByTitle("暂存此文件"));
    await waitFor(() => expect(stageSpy).toHaveBeenCalledWith("sess-git", ["src/dirty.ts"], undefined));

    fireEvent.click(screen.getByTitle("放弃工作区改动"));
    fireEvent.click(screen.getByRole("button", { name: "确认放弃" }));
    await waitFor(() => {
      expect(discardSpy).toHaveBeenCalledWith("sess-git", ["src/dirty.ts"], undefined);
      expect(props.onFilesChanged).toHaveBeenCalled();
    });
    rerender(<GitPanel {...props} />);
  });

  it("blocks commit while conflicts remain and allows empty-staged merge commit when merge_head is true", async () => {
    const commitSpy = vi.spyOn(api, "gitCommit").mockResolvedValue({ hash: "abc1234", branch: "main" });

    // Conflicts present: commit button MUST stay disabled even with staged files and merge_head!
    const { rerender, props } = renderGitPanel({
      status: makeMockGitStatus({
        merge_head: true,
        conflicts: [{ path: "src/conflict.ts", orig_path: null, status: "U" }],
        staged: [{ path: "src/resolved.ts", orig_path: null, status: "M" }],
      }),
    });

    const msgBox = screen.getByPlaceholderText(/写一句提交说明/);
    fireEvent.change(msgBox, { target: { value: "Merge branch feature" } });

    const submitBtn = screen.getByRole("button", { name: "提交" });
    expect((submitBtn as HTMLButtonElement).disabled).toBe(true);

    // Conflicts resolved, empty diff relative to HEAD (`staged: []`), `merge_head: true` -> enabled!
    rerender(
      <GitPanel
        {...props}
        status={makeMockGitStatus({
          merge_head: true,
          conflicts: [],
          staged: [],
          unstaged: [],
        })}
      />,
    );

    const mergeBtn = screen.getByRole("button", { name: "提交合并" });
    expect((mergeBtn as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(mergeBtn);
    await waitFor(() => {
      expect(commitSpy).toHaveBeenCalledWith("sess-git", "Merge branch feature", false, undefined);
    });
  });

  it("disables branch switcher only on root worktree repo and enables it on child sub-repos", async () => {
    const branchesSpy = vi.spyOn(api, "gitBranches").mockResolvedValue([
      { name: "main", current: true, remote: false, upstream: null, locked_elsewhere: false },
      { name: "dev", current: false, remote: false, upstream: null, locked_elsewhere: false },
    ]);
    const checkoutSpy = vi.spyOn(api, "gitCheckout").mockResolvedValue(makeMockSessionItem({ branch: "dev" }));

    const { rerender, props } = renderGitPanel({
      isWorktree: true,
      status: makeMockGitStatus({ active_repo: null, branch: "wt-branch" }),
    });
    const rootBranchBtn = screen.getByRole("button", { name: /wt-branch/ });
    expect((rootBranchBtn as HTMLButtonElement).disabled).toBe(true);

    // Switch to child sub-repository inside the worktree workspace -> branch switching is allowed!
    rerender(
      <GitPanel
        {...props}
        isWorktree
        status={makeMockGitStatus({ active_repo: "packages/sub", branch: "main" })}
      />,
    );
    const subBranchBtn = screen.getByRole("button", { name: /main/ });
    expect((subBranchBtn as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(subBranchBtn);

    await waitFor(() => expect(branchesSpy).toHaveBeenCalledWith("sess-git", "packages/sub"));
    const devBtn = await screen.findByRole("button", { name: "dev" });
    fireEvent.click(devBtn);
    await waitFor(() => expect(checkoutSpy).toHaveBeenCalledWith("sess-git", "dev", false, "packages/sub"));
  });

  it("disables single-file stage/unstage/discard buttons on unrepresentable_path entries", () => {
    renderGitPanel({
      status: makeMockGitStatus({
        staged: [{ path: "bad\uFFFDstaged.bin", orig_path: null, status: "M", unrepresentable_path: true }],
        unstaged: [{ path: "bad\uFFFDunstaged.bin", orig_path: null, status: "M", unrepresentable_path: true }],
      }),
    });

    expect(screen.getAllByText("非 UTF-8 路径")).toHaveLength(2);
    const hintButtons = screen.getAllByTitle("路径含非 UTF-8 字节，请在终端处理");
    for (const el of hintButtons) {
      if (el instanceof HTMLButtonElement) {
        expect(el.disabled).toBe(true);
      }
    }
  });

  it("passes explicit repo to DiffViewer gitDiff and updates both file and diff tabs on rename", async () => {
    const diffSpy = vi.spyOn(api, "gitDiff").mockResolvedValue({
      path: "src/index.ts",
      name: "index.ts",
      language: "typescript",
      stage: "unstaged",
      commit: null,
      binary: false,
      added: 1,
      removed: 0,
      hunks: [],
    });

    render(
      <DiffViewer
        sessionId="sess-1"
        path="subrepo/src/index.ts"
        stage="unstaged"
        repo="subrepo"
        onOpenSource={vi.fn()}
        onQuoteToMain={vi.fn()}
      />,
    );
    await waitFor(() => {
      expect(diffSpy).toHaveBeenCalledWith("sess-1", "src/index.ts", "unstaged", undefined, "subrepo");
    });

    const initialTabs: WorkbenchTab[] = [
      { id: "file:subrepo/src/old.ts", kind: "file", path: "subrepo/src/old.ts", name: "old.ts" },
      {
        id: "diff:subrepo:unstaged::subrepo/src/old.ts",
        kind: "diff",
        path: "subrepo/src/old.ts",
        name: "old.ts",
        stage: "unstaged",
        repo: "subrepo",
      },
    ];
    const renamed = renameWorkbenchTabs(
      initialTabs,
      "diff:subrepo:unstaged::subrepo/src/old.ts",
      "subrepo/src/old.ts",
      "subrepo/src/new.ts",
    );
    expect(renamed.tabs[0]).toMatchObject({ id: "file:subrepo/src/new.ts", path: "subrepo/src/new.ts", name: "new.ts" });
    expect(renamed.tabs[1]).toMatchObject({
      id: "diff:subrepo:unstaged::subrepo/src/new.ts",
      path: "subrepo/src/new.ts",
      name: "new.ts",
      repo: "subrepo",
    });
    expect(renamed.activeTabId).toBe("diff:subrepo:unstaged::subrepo/src/new.ts");
  });

  it("progressively mounts 4,000-line diffs in 400-line chunks (initial 800 rows) and emits structured diff quotes (P3-5 & P2-13)", async () => {
    const lines = Array.from({ length: 4_000 }, (_, i) => ({
      kind: "+" as const,
      old_line: null,
      new_line: i + 1,
      text: `export const line_${i + 1} = ${i + 1};`,
    }));

    vi.spyOn(api, "gitDiff").mockResolvedValue({
      path: "src/massive.ts",
      name: "massive.ts",
      language: "typescript",
      stage: "staged",
      commit: null,
      binary: false,
      added: 4_000,
      removed: 0,
      hunks: [{ header: "@@ -0,0 +1,4000 @@", old_start: 0, old_lines: 0, new_start: 1, new_lines: 4_000, lines }],
    });

    const onQuoteToMain = vi.fn();
    const { container } = render(
      <DiffViewer
        sessionId="sess-big-diff"
        path="src/massive.ts"
        stage="staged"
        onOpenSource={vi.fn()}
        onQuoteToMain={onQuoteToMain}
      />,
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".rich-diff-row")).toHaveLength(800);
    });
    expect(container.querySelectorAll(".rich-hunk-chunk")).toHaveLength(2);

    const loadMoreBtn = screen.getByRole("button", { name: /继续加载下 1,000 行/ });
    fireEvent.click(loadMoreBtn);

    await waitFor(() => {
      expect(container.querySelectorAll(".rich-diff-row")).toHaveLength(1_800);
    });
    expect(container.querySelectorAll(".rich-hunk-chunk")).toHaveLength(5);

    fireEvent.click(screen.getByRole("button", { name: "引入到对话" }));
    expect(onQuoteToMain).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "diff",
        path: "src/massive.ts",
        stage: "staged",
        added: 4_000,
        removed: 0,
      }),
    );
  });
});

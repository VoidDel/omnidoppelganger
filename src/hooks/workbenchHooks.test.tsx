import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { normalizeQuote } from "../components/AppPanes";
import { ChatEmptyStage, HomeEmptyStage } from "../components/EmptyStage";
import type { SidebarTab } from "../components/Sidebar";
import { StatusBanners } from "../components/StatusBanners";
import { useGlobalShortcuts } from "../hooks/useGlobalShortcuts";
import { useSlashCommands } from "../hooks/useSlashCommands";
import { renameWorkbenchTabs, useWorkbenchState } from "../hooks/useWorkbenchState";
import { useWorkspaceGitSync } from "../hooks/useWorkspaceGitSync";
import { api } from "../lib/api";
import type { SlashCommand } from "../lib/slash";
import { makeMockGitStatus, makeMockProvider, makeMockSessionItem } from "../test/mockApi";

describe("Workbench hooks & StatusBanners (P2-2, P2-12)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("useWorkspaceGitSync ignores workspace-changed events from other workspaces", async () => {
    let wsHandler: ((payload: { workspace: string; git: boolean }) => void) | null = null;
    vi.spyOn(api, "watchWorkspace").mockResolvedValue(undefined);
    vi.spyOn(api, "onWorkspaceChanged").mockImplementation(async (fn) => {
      wsHandler = fn;
      return () => undefined;
    });
    const gitStatusSpy = vi.spyOn(api, "gitStatus").mockResolvedValue(makeMockGitStatus());

    let ticks = { git: 0, fs: 0 };
    function Harness() {
      const sync = useWorkspaceGitSync({
        inChat: true,
        isProjectSession: true,
        activeId: "s1",
        activeWorkspace: "/ws/alpha",
        events: [],
        onExternalBranchChange: vi.fn(),
      });
      ticks = { git: sync.gitRefreshTick, fs: sync.fsRefreshTick };
      return <div>{ticks.fs}</div>;
    }

    render(<Harness />);
    await waitFor(() => expect(gitStatusSpy).toHaveBeenCalledTimes(1));
    expect(ticks).toEqual({ git: 0, fs: 0 });

    // Foreign workspace notification MUST be ignored!
    act(() => {
      wsHandler?.({ workspace: "/ws/other-project", git: true });
      wsHandler?.({ workspace: "/ws/other-project", git: false });
    });
    expect(ticks).toEqual({ git: 0, fs: 0 });

    // Matching workspace notification (even with trailing slash) updates ticks!
    act(() => {
      wsHandler?.({ workspace: "/ws/alpha/", git: false });
    });
    expect(ticks).toEqual({ git: 0, fs: 1 });

    act(() => {
      wsHandler?.({ workspace: "/ws/alpha", git: true });
    });
    expect(ticks).toEqual({ git: 1, fs: 2 });
  });

  it("useGlobalShortcuts handles Ctrl+K, Ctrl+P, Ctrl+B, Ctrl+J, and mobile Escape drawer dismissal", () => {
    const setDrawerOpen = vi.fn();
    const setJumping = vi.fn();
    const setSidebar = vi.fn();
    const summonTerminal = vi.fn();

    function ShortcutHarness() {
      useGlobalShortcuts({
        inChat: true,
        isProjectSession: true,
        hasActive: true,
        compactLayout: true,
        drawerOpen: true,
        setDrawerOpen,
        setJumping,
        setSidebar,
        summonTerminal,
      });
      return <div>shortcuts</div>;
    }

    render(<ShortcutHarness />);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    expect(setJumping).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(window, { key: "p", metaKey: true });
    expect(setJumping).toHaveBeenCalledTimes(2);

    fireEvent.keyDown(window, { key: "b", ctrlKey: true });
    expect(setSidebar).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(window, { key: "j", metaKey: true });
    expect(summonTerminal).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(setDrawerOpen).toHaveBeenCalledWith(false);
  });

  it("StatusBanners renders error, note, and untrusted workspace prompt with trust/dismiss actions", async () => {
    const trustSpy = vi.spyOn(api, "setWorkspaceTrust").mockResolvedValue(makeMockSessionItem({ project_trusted: true }));
    const onDismissError = vi.fn();
    const onDismissNote = vi.fn();
    const onTrusted = vi.fn();

    const untrustedSession = makeMockSessionItem({
      id: "s-untrusted",
      workspace: "/ws/stranger",
      project_trusted: false,
      untrusted_resources: ["AGENTS.md", ".odg/skills"],
    });

    const { rerender } = render(
      <StatusBanners
        error="网络异常"
        note="已导出"
        active={untrustedSession}
        replaying={false}
        onDismissError={onDismissError}
        onDismissNote={onDismissNote}
        onTrusted={onTrusted}
        onError={vi.fn()}
      />,
    );

    expect(screen.getByRole("alert").textContent).toContain("网络异常");
    expect(screen.getByRole("status").textContent).toContain("已导出");
    expect(screen.getByRole("region", { name: "洞府项目信任确认" }).textContent).toContain("AGENTS.md、.odg/skills");

    fireEvent.click(screen.getByRole("button", { name: "信任此洞府" }));
    await waitFor(() => {
      expect(trustSpy).toHaveBeenCalledWith("s-untrusted", true);
      expect(onTrusted).toHaveBeenCalled();
    });

    // Dismissing untrusted banner hides it for that workspace
    const anotherUntrusted = makeMockSessionItem({
      id: "s-2",
      workspace: "/ws/stranger-2",
      project_trusted: false,
      untrusted_resources: ["AGENTS.md"],
    });
    rerender(
      <StatusBanners
        error={null}
        note={null}
        active={anotherUntrusted}
        replaying={false}
        onDismissError={onDismissError}
        onDismissNote={onDismissNote}
        onTrusted={onTrusted}
        onError={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "暂不加载" }));
    expect(screen.queryByRole("region", { name: "洞府项目信任确认" })).toBeNull();
  });

  it("useWorkbenchState attaches repo to diff tabs and updates both file and diff tabs on rename/delete", () => {
    let apiRef!: ReturnType<typeof useWorkbenchState>;
    function WbHarness({ repo }: { repo: string | null }) {
      const state = useWorkbenchState({
        inChat: true,
        isProjectSession: true,
        activeWorkspace: "/ws/mono",
        replaying: false,
        selectedGitRepo: repo,
        leftTab: "files" as SidebarTab,
        setLeftTab: vi.fn(),
      });
      apiRef = state;
      return <div>{state.wbTabs.length}</div>;
    }

    render(<WbHarness repo="packages/core" />);

    act(() => {
      apiRef.openWorkbenchFile("packages/core/src/old.ts", true, 12);
      apiRef.openWorkbenchDiff("packages/core/src/old.ts", "unstaged", true);
    });
    expect(apiRef.wbTabs).toHaveLength(2);
    const diffTab = apiRef.wbTabs.find((t) => t.kind === "diff");
    expect(diffTab && diffTab.kind === "diff" ? diffTab.repo : null).toBe("packages/core");

    // Renaming updates BOTH `"file"` and `"diff"` tabs and keeps activeTabId in sync!
    act(() => {
      apiRef.handleRenamedWorkspacePath("packages/core/src/old.ts", "packages/core/src/new.ts");
    });
    expect(apiRef.wbTabs.map((t) => t.path)).toEqual(["packages/core/src/new.ts", "packages/core/src/new.ts"]);
    expect(apiRef.activeTabId).toContain("packages/core/src/new.ts");

    // Also test pure `renameWorkbenchTabs` folder prefix rename
    const folderRenamed = renameWorkbenchTabs(apiRef.wbTabs, apiRef.activeTabId, "packages/core", "packages/sdk");
    expect(folderRenamed.tabs.map((t) => t.path)).toEqual(["packages/sdk/src/new.ts", "packages/sdk/src/new.ts"]);

    // Deleting removes matching tabs and exits workbench when empty
    act(() => {
      apiRef.handleDeletedWorkspacePath("packages/core");
    });
    expect(apiRef.wbTabs).toHaveLength(0);
    expect(apiRef.showWorkbench).toBe(false);
  });

  it("useSlashCommands memoizes commands and omits project-only commands in chat sessions", async () => {
    vi.spyOn(api, "listPromptTemplates").mockResolvedValue([
      { name: "fix-ci", scope: "project", description: "Fix CI", argument_hint: null },
    ]);
    const active = makeMockSessionItem({ id: "s-slash", workspace: "/ws/proj" });
    const emptyEvents: never[] = [];
    const skills = [{ name: "refactor", description: "Refactor code", source: "bundled" as const, enabled: true, path: "/s" }];
    let latestCommands: SlashCommand[] = [];

    function SlashHarness({ isChat, tick }: { isChat: boolean; tick: number }) {
      latestCommands = useSlashCommands({
        active,
        isChatSession: isChat,
        status: "idle",
        events: emptyEvents,
        settingsSkills: skills,
        hasRows: true,
        hasStops: true,
        fsRefreshTick: 0,
        createIn: () => void tick,
        summonAside: vi.fn(),
        summonTerminal: vi.fn(),
        forkFrom: vi.fn(),
        exportSession: vi.fn(),
        send: vi.fn(),
        showSidebarTab: vi.fn(),
        startReplay: vi.fn(),
        toMcp: vi.fn(),
        toSkills: vi.fn(),
        toSchedules: vi.fn(),
        toSettings: vi.fn(),
        onSessionUpdated: vi.fn(),
        onNote: vi.fn(),
        onError: vi.fn(),
      });
      return <div>{latestCommands.length}</div>;
    }

    const { rerender } = render(<SlashHarness isChat={false} tick={1} />);
    await waitFor(() => expect(latestCommands.some((c) => c.name === "fix-ci")).toBe(true));
    expect(latestCommands.some((c) => c.name === "init")).toBe(true);
    expect(latestCommands.some((c) => c.name === "trust")).toBe(true);

    const prevRef = latestCommands;
    // Re-rendering with fresh callback closures does NOT invalidate memoized SlashCommand[] reference!
    rerender(<SlashHarness isChat={false} tick={2} />);
    expect(latestCommands).toBe(prevRef);

    // Switching to chat session filters out init, review, files, git, trust
    rerender(<SlashHarness isChat={true} tick={2} />);
    expect(latestCommands.some((c) => c.name === "init")).toBe(false);
    expect(latestCommands.some((c) => c.name === "git")).toBe(false);
    expect(latestCommands.some((c) => c.name === "trust")).toBe(false);
  });

  it("EmptyStage renders HomeEmptyStage missing-key callout and ChatEmptyStage mode hint", () => {
    const onOpenSettings = vi.fn();
    const { rerender } = render(
      <HomeEmptyStage
        missingKey
        defaultProviderName="Anthropic"
        sessions={[]}
        schedules={[]}
        onCreate={vi.fn()}
        onChat={vi.fn()}
        onOpenSettings={onOpenSettings}
        onOpenSession={vi.fn()}
        onOpenSchedules={vi.fn()}
      />,
    );
    expect(screen.getByText(/还没有 Anthropic 的密钥/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "去填密钥" }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);

    rerender(
      <ChatEmptyStage
        active={makeMockSessionItem({ mode: "plan" })}
        isChatSession={false}
        providers={[makeMockProvider()]}
      />,
    );
    expect(screen.getByText(/先谋后动/)).toBeTruthy();
  });

  it("StatusBanners hides untrusted prompt during replay and normalizeQuote parses file/terminal strings", () => {
    const untrusted = makeMockSessionItem({
      id: "s-rep",
      workspace: "/ws/rep",
      project_trusted: false,
      untrusted_resources: ["AGENTS.md"],
    });
    render(
      <StatusBanners
        error={null}
        note={null}
        active={untrusted}
        replaying
        onDismissError={vi.fn()}
        onDismissNote={vi.fn()}
        onTrusted={vi.fn()}
        onError={vi.fn()}
      />,
    );
    expect(screen.queryByRole("region", { name: "洞府项目信任确认" })).toBeNull();

    expect(
      normalizeQuote({ kind: "code", path: "src/App.tsx", lineRange: [10, 10], content: "const x = 1;" }, "zh-CN").label,
    ).toBe("App.tsx:L10");
    expect(normalizeQuote({ kind: "terminal", content: "$ ls" }, "zh-CN").label).toBe("终端输出");
    expect(normalizeQuote({ kind: "job", jobId: "b1", command: "pnpm dev", content: "ready" }, "en").label).toBe("Job #b1");
    expect(
      normalizeQuote({ kind: "diff", path: "src/App.tsx", stage: "staged", added: 2, removed: 1, content: "+x" }, "en").label,
    ).toBe("App.tsx · staged diff");
    expect(normalizeQuote("raw plain quote", "en").label).toBe("Quoted snippet");
  });
});

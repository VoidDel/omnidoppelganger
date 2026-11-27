import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../lib/api";
import type { SlashCommand } from "../lib/slash";
import { makeMockProvider, makeMockUsage } from "../test/mockApi";
import { Combo } from "./Combo";
import { Composer, type ComposerProps } from "./Composer";
import { FileTree } from "./FileTree";
import { WorkbenchTabsBar, type WorkbenchTab } from "./Workbench";

function makeComposerProps(overrides: Partial<ComposerProps> = {}): ComposerProps {
  const provider = makeMockProvider();
  return {
    status: "idle",
    providers: [provider],
    listed: () => true,
    keyed: () => true,
    providerId: "anthropic",
    model: "claude-sonnet-4-6",
    effort: "high",
    effortSet: false,
    efforts: ["low", "medium", "high", "xhigh", "max"],
    onEffort: vi.fn(),
    vision: true,
    mode: "edit",
    usage: makeMockUsage({ context: 1200 }),
    contextLimit: 160_000,
    contextCeiling: 160_000,
    contextSetting: 160_000,
    contextAuto: true,
    onLimit: vi.fn(),
    onCompact: vi.fn(),
    onModel: vi.fn(),
    listModels: api.listModels,
    planReady: false,
    onMode: vi.fn(),
    onCarryOut: vi.fn(),
    onSend: vi.fn(),
    onStop: vi.fn(),
    trustable: false,
    sessionKey: "sess-main",
    workspace: "/ws/demo",
    seed: null,
    commands: [],
    ...overrides,
  };
}

describe("Accessibility & Unique DOM IDs (P2-13)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.spyOn(api, "getModelUsage").mockResolvedValue([]);
    vi.spyOn(api, "onFilesDragged").mockResolvedValue(() => undefined);
    vi.spyOn(api, "listModels").mockResolvedValue([["claude-sonnet-4-6", "Claude Sonnet 4.6"]]);
  });

  it("renders dual Composer instances (main + keep-alive aside) with zero duplicate DOM IDs even with menus open", async () => {
    const user = userEvent.setup();
    vi.spyOn(api, "searchWorkspaceFiles").mockResolvedValue([
      { path: "src/App.tsx", name: "App.tsx", dir: "src" },
    ]);

    const commands: SlashCommand[] = [
      { name: "review", label: "审查", aliases: ["审查"], hint: "审查改动", run: vi.fn() },
      { name: "init", label: "立规矩", aliases: ["规矩"], hint: "生成 AGENTS.md", run: vi.fn() },
    ];

    render(
      <div>
        <Composer {...makeComposerProps({ sessionKey: "sess-main", commands })} />
        <div style={{ display: "none" }}>
          <Composer {...makeComposerProps({ sessionKey: "sess-aside", aside: true, commands })} />
        </div>
      </div>,
    );

    const textareas = document.querySelectorAll("textarea");
    expect(textareas).toHaveLength(2);
    expect(textareas[0].id).toBeTruthy();
    expect(textareas[1].id).toBeTruthy();
    expect(textareas[0].id).not.toBe(textareas[1].id);

    // Open slash menu on main Composer and @file menu on aside Composer simultaneously
    await user.type(textareas[0], "/");
    fireEvent.change(textareas[1], { target: { value: "@App", selectionStart: 4 } });

    await waitFor(() => {
      expect(document.getElementById(textareas[0].getAttribute("aria-controls") ?? "")).not.toBeNull();
      expect(document.getElementById(textareas[1].getAttribute("aria-controls") ?? "")).not.toBeNull();
    });

    const allWithId = [...document.querySelectorAll("[id]")];
    expect(allWithId.length).toBeGreaterThanOrEqual(6);
    const ids = allWithId.map((el) => el.id);
    const uniqueIds = new Set(ids);
    expect(uniqueIds.size).toBe(ids.length);
  });

  it("renders sibling <button role='tab'> and close buttons in WorkbenchTabsBar with ArrowLeft/Right/Home/End navigation", () => {
    const tabs: WorkbenchTab[] = [
      { id: "file:a.ts", kind: "file", path: "src/a.ts", name: "a.ts" },
      { id: "diff:b.yaml", kind: "diff", path: "pnpm-workspace.yaml", name: "pnpm-workspace.yaml", stage: "unstaged" },
    ];
    const onSelectChat = vi.fn();
    const onSelectTab = vi.fn();
    const onCloseTab = vi.fn();

    const { container } = render(
      <WorkbenchTabsBar
        tabs={tabs}
        inWorkbench={true}
        activeTabId="file:a.ts"
        chatStatus="idle"
        onSelectChat={onSelectChat}
        onSelectTab={onSelectTab}
        onMoveTab={vi.fn()}
        onCloseTab={onCloseTab}
        onCloseAllTabs={vi.fn()}
      />,
    );

    const tabButtons = screen.getAllByRole("tab");
    expect(tabButtons).toHaveLength(3);
    // Ensure zero nested buttons inside any `[role="tab"]`
    for (const btn of tabButtons) {
      expect(btn.tagName).toBe("BUTTON");
      expect(btn.querySelectorAll("button")).toHaveLength(0);
    }
    expect(container.querySelectorAll(".wb-tab-item .wb-tab-close")).toHaveLength(2);

    // Active tab (`a.ts`, index 1) has tabIndex=0; inactive tabs have tabIndex=-1
    expect(tabButtons[0].tabIndex).toBe(-1);
    expect(tabButtons[1].tabIndex).toBe(0);
    expect(tabButtons[2].tabIndex).toBe(-1);

    expect(tabButtons[2].getAttribute("title")).toBe("pnpm-workspace.yaml（改动对比）");
    expect(container.querySelector(".wb-tab-tag")?.textContent).toBe("改动对比");

    tabButtons[1].focus();
    const tablist = screen.getByRole("tablist");
    fireEvent.keyDown(tablist, { key: "ArrowRight" });
    expect(onSelectTab).toHaveBeenCalledWith("diff:b.yaml");

    tabButtons[2].focus();
    fireEvent.keyDown(tablist, { key: "Home" });
    expect(onSelectChat).toHaveBeenCalledTimes(1);

    tabButtons[0].focus();
    fireEvent.keyDown(tablist, { key: "End" });
    expect(onSelectTab).toHaveBeenLastCalledWith("diff:b.yaml");
  });

  it("binds aria-activedescendant on Combo role='combobox' during keyboard navigation", () => {
    render(
      <Combo
        id="test-combo"
        value="a"
        options={[
          { value: "a", text: "Option A" },
          { value: "b", text: "Option B" },
        ]}
        editable={false}
        onSave={vi.fn()}
      />,
    );

    const combobox = screen.getByRole("combobox");
    fireEvent.keyDown(combobox, { key: "ArrowDown" });

    const activeId = combobox.getAttribute("aria-activedescendant");
    expect(activeId).toBe("test-combo-opt-0");
    expect(document.getElementById(activeId!)?.textContent).toContain("Option A");

    fireEvent.keyDown(combobox, { key: "ArrowDown" });
    expect(combobox.getAttribute("aria-activedescendant")).toBe("test-combo-opt-1");
  });

  it("supports 4-way WAI-ARIA TreeView arrow key navigation in FileTree", async () => {
    vi.spyOn(api, "listWorkspaceDir").mockImplementation(async (_sid, rel) => {
      if (!rel) {
        return [
          { name: "src", path: "src", kind: "dir", expandable: true, size: null, modified_ms: null, ignored: false },
          { name: "README.md", path: "README.md", kind: "file", expandable: false, size: 42, modified_ms: null, ignored: false },
        ];
      }
      return [
        { name: "main.ts", path: "src/main.ts", kind: "file", expandable: false, size: 10, modified_ms: null, ignored: false },
      ];
    });

    render(
      <FileTree
        sessionId="sess-tree"
        workspace="/ws/demo"
        activePath={null}
        onOpenFile={vi.fn()}
        onAttachFile={vi.fn()}
        onDeletedPath={vi.fn()}
        onRenamedPath={vi.fn()}
        onError={vi.fn()}
      />,
    );

    await screen.findByRole("tree");
    await waitFor(() => expect(screen.getAllByRole("treeitem")).toHaveLength(2));

    const items = screen.getAllByRole("treeitem");
    items[0].focus();
    // ArrowRight expands collapsed directory `src`
    fireEvent.keyDown(items[0], { key: "ArrowRight" });
    await waitFor(() => expect(screen.getAllByRole("treeitem")).toHaveLength(3));

    // ArrowRight again moves focus into first child `src/main.ts`
    const expandedItems = screen.getAllByRole("treeitem");
    fireEvent.keyDown(expandedItems[0], { key: "ArrowRight" });
    expect(document.activeElement).toBe(expandedItems[1]);

    // ArrowLeft from child `src/main.ts` returns focus to parent directory `src`
    fireEvent.keyDown(expandedItems[1], { key: "ArrowLeft" });
    expect(document.activeElement).toBe(expandedItems[0]);

    // ArrowDown moves focus from `src` to `src/main.ts`, then `README.md`
    fireEvent.keyDown(expandedItems[0], { key: "ArrowDown" });
    fireEvent.keyDown(expandedItems[1], { key: "ArrowDown" });
    expect(document.activeElement).toBe(expandedItems[2]);
  });
});

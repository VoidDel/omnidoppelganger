import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../lib/api";
import type { SlashCommand } from "../lib/slash";
import { makeMockProvider, makeMockUsage } from "../test/mockApi";
import { Composer, type ComposerDraft, type ComposerProps } from "./Composer";

function setupBaseMocks() {
  vi.spyOn(api, "getModelUsage").mockResolvedValue([]);
  vi.spyOn(api, "onFilesDragged").mockResolvedValue(() => undefined);
  vi.spyOn(api, "listModels").mockResolvedValue([["claude-sonnet-4-6", "Claude Sonnet 4.6"]]);
}

function renderComposer(overrides: Partial<ComposerProps> = {}) {
  const provider = makeMockProvider();
  const props: ComposerProps = {
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
    sessionKey: "sess-1",
    workspace: "/ws/demo",
    seed: null,
    commands: [],
    ...overrides,
  };
  return { ...render(<Composer {...props} />), props };
}

describe("Composer component (P1-15 & P2-12)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    setupBaseMocks();
  });

  it("filters slash commands on '/' input and runs the chosen command on Enter", async () => {
    const user = userEvent.setup();
    const runReview = vi.fn();
    const commands: SlashCommand[] = [
      { name: "review", label: "审查", aliases: ["审查"], hint: "审查未提交改动", run: runReview },
      { name: "init", label: "立规矩", aliases: ["规矩"], hint: "生成 AGENTS.md", run: vi.fn() },
    ];
    renderComposer({ commands });

    const input = screen.getByRole("textbox");
    await user.type(input, "/rev");
    const listbox = screen.getByRole("listbox", { name: "命令" });
    expect(listbox.textContent).toContain("/review");
    expect(listbox.textContent).not.toContain("/init");

    await user.keyboard("{Enter}");
    expect(runReview).toHaveBeenCalledTimes(1);
    expect((input as HTMLTextAreaElement).value).toBe("");
  });

  it("autocompletes @file mentions from workspace search and attaches the selected file", async () => {
    const user = userEvent.setup();
    const searchSpy = vi.spyOn(api, "searchWorkspaceFiles").mockResolvedValue([
      { name: "App.tsx", path: "src/App.tsx", dir: "src" },
      { name: "api.ts", path: "src/lib/api.ts", dir: "src/lib" },
    ]);
    const attachSpy = vi.spyOn(api, "attachFiles").mockResolvedValue([
      { kind: "file", path: "/ws/demo/src/App.tsx", name: "App.tsx", bytes: 512, inline: true },
    ]);

    renderComposer();
    const input = screen.getByRole("textbox");
    await user.type(input, "请检查 @App");

    await waitFor(() => {
      expect(searchSpy).toHaveBeenCalledWith("sess-1", "App", 24);
      expect(screen.getByRole("listbox", { name: "引用文件" })).toBeTruthy();
    });

    await user.keyboard("{Enter}");
    expect((input as HTMLTextAreaElement).value).toContain("@src/App.tsx ");
    await waitFor(() => {
      expect(attachSpy).toHaveBeenCalledWith(["/ws/demo/src/App.tsx"]);
      expect(screen.getByText("App.tsx")).toBeTruthy();
    });
  });

  it("collapses oversized pasted text into a chip and calls removePastedFile when removed unsent", async () => {
    const saveSpy = vi.spyOn(api, "savePastedFile").mockResolvedValue({
      kind: "file",
      path: "/Users/test/.odg/pastes/paste-1.txt",
      name: "paste-1.txt",
      bytes: 4200,
      inline: true,
    });
    const removeSpy = vi.spyOn(api, "removePastedFile").mockResolvedValue(undefined);

    renderComposer();
    const input = screen.getByRole("textbox");
    const longPaste = Array.from({ length: 45 }, (_, i) => `line ${i + 1}: some long log output content`).join("\n");

    fireEvent.paste(input, {
      clipboardData: {
        items: [],
        getData: (type: string) => (type === "text/plain" ? longPaste : ""),
      },
    });

    await waitFor(() => {
      expect(saveSpy).toHaveBeenCalledTimes(1);
      expect(screen.getByText("paste-1.txt")).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: "去掉 paste-1.txt" }));
    await waitFor(() => {
      expect(removeSpy).toHaveBeenCalledWith("/Users/test/.odg/pastes/paste-1.txt");
    });
    expect(screen.queryByText("paste-1.txt")).toBeNull();
  });

  it("executes inline !command on Enter and attaches its output as a quote chip", async () => {
    const user = userEvent.setup();
    const inlineSpy = vi.spyOn(api, "runInlineCommand").mockResolvedValue({
      command: "git status -s",
      output: " M src/App.tsx\n",
      exit_code: 0,
      timed_out: false,
      truncated: false,
    });

    const { props } = renderComposer();
    const input = screen.getByRole("textbox");
    await user.type(input, "!git status -s{Enter}");

    await waitFor(() => {
      expect(inlineSpy).toHaveBeenCalledWith("sess-1", "git status -s");
      expect(screen.getByText("! git status -s")).toBeTruthy();
    });
    expect(props.onSend).not.toHaveBeenCalled();
  });

  it("cycles mode on Shift+Tab", () => {
    const onMode = vi.fn();
    renderComposer({ mode: "edit", onMode });
    const input = screen.getByRole("textbox");
    fireEvent.keyDown(input, { key: "Tab", shiftKey: true });
    expect(onMode).toHaveBeenCalledWith("plan");
  });

  it("isolates drafts per sessionKey across session switches", async () => {
    const user = userEvent.setup();
    const drafts = new Map<string, ComposerDraft>();
    const provider = makeMockProvider();

    const baseProps: ComposerProps = {
      status: "idle",
      providers: [provider],
      listed: () => true,
      keyed: () => true,
      providerId: "anthropic",
      model: "claude-sonnet-4-6",
      effort: "high",
      effortSet: false,
      efforts: ["low", "medium", "high"],
      onEffort: vi.fn(),
      vision: true,
      mode: "edit",
      usage: makeMockUsage(),
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
      workspace: "/ws/demo",
      drafts,
      seed: null,
      commands: [],
    };

    const { rerender } = render(<Composer key="s1" {...baseProps} sessionKey="s1" />);
    await user.type(screen.getByRole("textbox"), "draft for session one");
    expect(drafts.get("s1")?.text).toBe("draft for session one");

    rerender(<Composer key="s2" {...baseProps} sessionKey="s2" />);
    expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe("");
    await user.type(screen.getByRole("textbox"), "draft for session two");

    rerender(<Composer key="s1" {...baseProps} sessionKey="s1" />);
    expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe("draft for session one");
  });

  it("allows clearing active model rate-limit cooldown from the stones popup", async () => {
    const futureIso = new Date(Date.now() + 90_000).toISOString();
    vi.spyOn(api, "getModelUsage").mockResolvedValue([
      {
        provider: "anthropic",
        model: "claude-sonnet-4-6",
        requests: 5,
        input_tokens: 1000,
        cached_tokens: 200,
        output_tokens: 300,
        today_key: "2026-11-26",
        today_requests: 5,
        today_input_tokens: 1000,
        today_cached_tokens: 200,
        today_output_tokens: 300,
        cooldown_until: futureIso,
        cooldown_reason: "429 rate limit",
      },
    ]);
    const clearSpy = vi.spyOn(api, "clearModelCooldown").mockResolvedValue([]);

    renderComposer();
    await waitFor(() => {
      expect(screen.getByText(/冷却/)).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: /灵石/ }));
    const clearBtn = await screen.findByRole("button", { name: "清除冷却" });
    fireEvent.click(clearBtn);
    expect(clearSpy).toHaveBeenCalledWith("anthropic", "claude-sonnet-4-6");
  });
});

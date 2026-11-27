import { vi } from "vitest";
import { api } from "../lib/api";
import type { Usage } from "../lib/fold";
import type * as Types from "../lib/types";
import type {
  GitStatus,
  Provider,
  SessionItem,
  SessionView,
  Settings,
  SettingsView,
} from "../lib/types";

export type MockedApi = {
  [K in keyof typeof api]: ReturnType<typeof vi.fn<typeof api[K]>>;
};

/** Spy/stub helper that resets and mocks `api` methods with type safety. */
export function mockApiMethod<K extends keyof typeof api>(
  key: K,
  impl?: (...args: Parameters<(typeof api)[K]>) => ReturnType<(typeof api)[K]>,
) {
  const spy = vi.spyOn(api, key);
  if (impl) {
    spy.mockImplementation(impl as never);
  }
  return spy;
}

export function makeMockSessionItem(overrides: Partial<Types.SessionItem> = {}): Types.SessionItem {
  return {
    id: "sess-1",
    title: "测试分身",
    worktree: null,
    parent: null,
    schedule: null,
    fork: null,
    aside: null,
    workspace: "/workspace/demo",
    provider: "anthropic",
    model: "claude-sonnet-4-6",
    effort: null,
    mode: "edit",
    context_limit: 160_000,
    context_ceiling: 160_000,
    branch: "main",
    vision: true,
    project_trusted: true,
    untrusted_resources: [],
    created_at: "2026-11-26T08:00:00.000Z",
    updated_at: "2026-11-26T08:00:00.000Z",
    turn_open: false,
    awaiting_approval: false,
    status: "idle",
    ...overrides,
  };
}

export function makeMockSessionView(overrides: Partial<Types.SessionView> = {}): Types.SessionView {
  return {
    ...makeMockSessionItem(overrides),
    events: [],
    ...overrides,
  };
}

export function makeMockUsage(overrides: Partial<Usage> = {}): Usage {
  return {
    context: 0,
    spent: { input: 0, cached: 0, output: 0 },
    breakdown: { prompt: 0, input: 0, output: 0, skill: 0, mcp: 0, tools: 0 },
    ...overrides,
  };
}

export function makeMockProvider(overrides: Partial<Types.Provider> = {}): Types.Provider {
  const id = overrides.id ?? "anthropic";
  return {
    id,
    kind: overrides.kind ?? id,
    name: overrides.name ?? "Anthropic",
    protocol: overrides.protocol ?? "anthropic",
    base_url: overrides.base_url ?? "https://api.anthropic.com",
    other_urls: overrides.other_urls ?? [],
    key_env: overrides.key_env ?? "ANTHROPIC_API_KEY",
    models: overrides.models ?? [
      ["claude-sonnet-4-6", "Claude Sonnet 4.6"],
      ["claude-opus-4-6", "Claude Opus 4.6"],
    ],
  };
}

export function makeMockSettings(overrides: Partial<Types.Settings> = {}): Types.Settings {
  return {
    provider: "anthropic",
    model: "claude-sonnet-4-6",
    effort: "high",
    mode: "edit",
    context_limit: 160_000,
    search_engine: "off",
    painter: "off",
    close_to_tray: true,
    theme: "light",
    base_urls: {},
    provider_names: {},
    disabled_providers: [],
    sandbox: true,
    sandbox_paths: [],
    context_windows: {},
    context_auto: true,
    guardian: false,
    guardian_models: {},
    approval_timeout: 0,
    ...overrides,
  };
}

export function makeMockSettingsView(overrides: Partial<Types.SettingsView> = {}): Types.SettingsView {
  const anthropic = makeMockProvider();
  return {
    settings: makeMockSettings(overrides.settings),
    keys: {
      anthropic: { source: "saved", preview: "sk-ant-...1234", plaintext: false },
    },
    providers: [anthropic],
    kinds: [
      {
        id: anthropic.id,
        name: anthropic.name,
        protocol: anthropic.protocol,
        base_url: anthropic.base_url,
        other_urls: anthropic.other_urls,
        key_env: anthropic.key_env,
        models: anthropic.models,
      },
    ],
    efforts: ["low", "medium", "high", "xhigh", "max"],
    home: "/Users/test/.odg",
    grants: {},
    search_engines: [["search_std", "标准搜索"]],
    search_ready: true,
    painters: [],
    painter_ready: null,
    context_window: { tokens: 200_000, source: "documented" },
    sandbox_support: { Ok: "seatbelt" },
    skills: [],
    skills_directory: "/Users/test/.odg/skills",
    chat_folder: "/Users/test/.odg/chat",
    mcp_config: "{}",
    mcp_servers: [],
    version: "0.8.0",
    ...overrides,
  };
}

export function makeMockGitStatus(overrides: Partial<Types.GitStatus> = {}): Types.GitStatus {
  return {
    is_repo: true,
    active_repo: null,
    repos: [],
    branch: "main",
    detached: false,
    upstream: "origin/main",
    ahead: 0,
    behind: 0,
    merge_head: false,
    rebase_head: false,
    conflicts: [],
    staged: [],
    unstaged: [],
    untracked: [],
    untracked_truncated: false,
    ...overrides,
  };
}

export type { GitStatus, Provider, SessionItem, SessionView, Settings, SettingsView };

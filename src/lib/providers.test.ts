import { describe, expect, it } from "vitest";
import { cliOf, enabledProviders, isProviderEnabled, isSetUp, setUp } from "./providers";
import type { Provider, SettingsView } from "./types";

const provider = (id: string, kind = id, protocol: Provider["protocol"] = { chat: "generic" }): Provider => ({
  id,
  kind,
  name: id,
  protocol,
  base_url: "",
  other_urls: [],
  key_env: null,
  models: [],
});

describe("the 宗门 the user keeps", () => {
  it("are those with a key or an address, those added beside the first of a kind, and the default; not the other kinds", () => {
    const providers = [
      provider("anthropic"),
      provider("openai"),
      provider("zhipu"),
      provider("custom"),
      provider("custom-2", "custom"),
      provider("custom_anthropic"),
      provider("deepseek"),
      provider("claude_code", "claude_code", { cli: "claude_code" }),
      provider("codex", "codex", { cli: "codex" }),
    ];
    const view = {
      providers,
      keys: {
        anthropic: { source: "env" },
        openai: { source: "none" },
        zhipu: { source: "saved" },
        custom: { source: "none" },
        "custom-2": { source: "none" },
        custom_anthropic: { source: "none" },
        deepseek: { source: "none" },
        claude_code: { source: "cli", preview: "/usr/local/bin/claude" },
        codex: { source: "none" },
      },
      settings: {
        provider: "deepseek",
        base_urls: { custom: "http://127.0.0.1:3425" },
        provider_names: { "custom-2": "本地二号" },
        disabled_providers: ["claude_code", "deepseek"],
      },
    } as unknown as SettingsView;
    expect(setUp(view).map((one) => one.id)).toEqual(["anthropic", "zhipu", "custom", "custom-2", "deepseek", "claude_code"]);
    // Disabled providers stay in `setUp` (for the settings list) but are excluded from `enabledProviders` (except the active default).
    expect(enabledProviders(view).map((one) => one.id)).toEqual(["anthropic", "zhipu", "custom", "custom-2", "deepseek"]);
    expect(isProviderEnabled(view, provider("claude_code", "claude_code", { cli: "claude_code" }))).toBe(false);
    // A compatible kind or uninstalled CLI is not offered until it is added or detected.
    expect(isSetUp(view, provider("custom_anthropic"))).toBe(false);
    expect(isSetUp(view, provider("openai"))).toBe(false);
    expect(isSetUp(view, provider("codex", "codex", { cli: "codex" }))).toBe(false);
    expect(cliOf(provider("claude_code", "claude_code", { cli: "claude_code" }))).toBe("claude_code");
    expect(cliOf(provider("openai", "openai", "responses"))).toBeNull();
  });
});

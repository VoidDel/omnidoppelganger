import { describe, expect, it } from "vitest";
import { cliOf, enabledProviders, isProviderEnabled, isProviderKeyed, isSetUp, setUp } from "./providers";
import type { KeyStatus, Provider, SettingsView } from "./types";

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

  it("treats ollama, magpie, and custom* providers as keyed without requiring a saved API key", () => {
    const providers = [
      provider("anthropic"),
      provider("openai"),
      provider("ollama"),
      provider("magpie"),
      provider("custom"),
      provider("custom-local", "custom_anthropic"),
      provider("claude_code", "claude_code", { cli: "claude_code" }),
    ];
    const keys: Record<string, KeyStatus> = {
      anthropic: { source: "saved", preview: "sk-...1", plaintext: false },
      openai: { source: "none", preview: null, plaintext: false },
      ollama: { source: "none", preview: null, plaintext: false },
      magpie: { source: "none", preview: null, plaintext: false },
      custom: { source: "none", preview: null, plaintext: false },
      "custom-local": { source: "none", preview: null, plaintext: false },
      claude_code: { source: "cli", preview: "/opt/bin/claude", plaintext: false },
    };

    expect(isProviderKeyed("anthropic", providers, keys)).toBe(true);
    expect(isProviderKeyed("openai", providers, keys)).toBe(false);
    expect(isProviderKeyed("ollama", providers, keys)).toBe(true);
    expect(isProviderKeyed("magpie", providers, keys)).toBe(true);
    expect(isProviderKeyed("custom", providers, keys)).toBe(true);
    expect(isProviderKeyed("custom-local", providers, keys)).toBe(true);
    expect(isProviderKeyed("claude_code", providers, keys)).toBe(true);
  });

  it("returns false when keys map is undefined for cloud API providers while keeping keyless providers enabled", () => {
    const providers = [provider("anthropic"), provider("ollama"), provider("magpie")];
    expect(isProviderKeyed("anthropic", providers, undefined)).toBe(false);
    expect(isProviderKeyed("ollama", providers, undefined)).toBe(true);
    expect(isProviderKeyed("magpie", providers, undefined)).toBe(true);
  });

  it("treats custom-kind providers as keyed even when id does not start with custom", () => {
    const providers = [provider("intranet-gw", "custom"), provider("corp-claude", "custom_anthropic")];
    expect(isProviderKeyed("intranet-gw", providers, {})).toBe(true);
    expect(isProviderKeyed("corp-claude", providers, {})).toBe(true);
    expect(isProviderKeyed("unknown-cloud", providers, {})).toBe(false);
  });
});

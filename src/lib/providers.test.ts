import { describe, expect, it } from "vitest";
import { isSetUp, setUp } from "./providers";
import type { Provider, SettingsView } from "./types";

const provider = (id: string, kind = id): Provider => ({ id, kind, name: id, protocol: { chat: "generic" }, base_url: "", other_urls: [], key_env: null, models: [] });

describe("the 宗门 the user keeps", () => {
  it("are those with a key or an address, those added beside the first of a kind, and the default; not the other kinds", () => {
    const providers = [provider("anthropic"), provider("openai"), provider("zhipu"), provider("custom"), provider("custom-2", "custom"), provider("custom_anthropic"), provider("deepseek")];
    const view = {
      providers,
      keys: { anthropic: { source: "env" }, openai: { source: "none" }, zhipu: { source: "saved" }, custom: { source: "none" }, "custom-2": { source: "none" }, custom_anthropic: { source: "none" }, deepseek: { source: "none" } },
      settings: { provider: "deepseek", base_urls: { custom: "http://127.0.0.1:3425" }, provider_names: { "custom-2": "本地二号" } },
    } as unknown as SettingsView;
    expect(setUp(view).map((one) => one.id)).toEqual(["anthropic", "zhipu", "custom", "custom-2", "deepseek"]);
    // A compatible kind is not offered until it is added, though it may need no key.
    expect(isSetUp(view, provider("custom_anthropic"))).toBe(false);
    expect(isSetUp(view, provider("openai"))).toBe(false);
  });
});

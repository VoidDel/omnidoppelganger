import type { CliBackend, Provider, SettingsView } from "./types";

export const CLI_INFO: Record<CliBackend, { bin: string; label: string }> = {
  antigravity: { bin: "agy", label: "Antigravity (agy)" },
  codex: { bin: "codex", label: "Codex CLI (codex)" },
  claude_code: { bin: "claude", label: "Claude Code (claude)" },
  cursor: { bin: "cursor-agent", label: "Cursor Agent (cursor-agent)" },
  grok: { bin: "grok", label: "Grok CLI (grok)" },
};

/** Return the local CLI backend of a provider/kind, or null for HTTP API providers. */
export function cliOf(provider?: { protocol?: Provider["protocol"] } | null): CliBackend | null {
  const proto = provider?.protocol;
  return proto && typeof proto === "object" && "cli" in proto ? proto.cli : null;
}

/**
 * Whether a provider is in the list the user keeps: it has a key or an address of its own, or the user added it
 * beside the first of its kind, or new 分身 come from it (so that a missing key shows). These are the 宗门 the
 * settings list and the model menu offers; the other kinds wait to be added.
 */
export function isSetUp(view: SettingsView, provider: Provider): boolean {
  const { settings } = view;
  return (view.keys[provider.id]?.source ?? "none") !== "none" || !!settings.base_urls[provider.id] || provider.id !== provider.kind || provider.id === settings.provider;
}

/** The providers in the list the user keeps, in the order the settings give them. */
export function setUp(view: SettingsView): Provider[] {
  return view.providers.filter((provider) => isSetUp(view, provider));
}

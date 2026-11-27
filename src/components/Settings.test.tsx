import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { I18nProvider, setCurrentLocale } from "../i18n";
import { api } from "../lib/api";
import { makeMockProvider, makeMockSettingsView } from "../test/mockApi";
import { SettingsPage } from "./Settings";

describe("SettingsPage component (P1-15 & P2-14)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    setCurrentLocale("zh-CN");
    vi.spyOn(api, "runtimeStatus").mockResolvedValue({
      status: { state: "missing", outdated: false },
      python_version: "3.12",
      download_mb: 30,
      libreoffice: null,
      supported: true,
    });
    vi.spyOn(api, "onRuntimeProgress").mockResolvedValue(() => undefined);
    vi.spyOn(api, "languageServers").mockResolvedValue([]);
  });

  it("statically displays the current app version offline before checking updates and falls back to __APP_VERSION__", async () => {
    const checkSpy = vi.spyOn(api, "checkAppUpdate").mockRejectedValue(new Error("offline"));
    const view = makeMockSettingsView({ version: "0.9.0" });

    const { rerender } = render(
      <SettingsPage
        view={view}
        listModels={vi.fn().mockResolvedValue([])}
        onRefresh={vi.fn()}
        onRevoke={vi.fn()}
        onSave={vi.fn()}
        onSaveKey={vi.fn()}
        onAddProvider={vi.fn()}
        onRemoveProvider={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "关于" }));
    expect(screen.getByText("v0.9.0")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "检查更新" }));
    await waitFor(() => expect(checkSpy).toHaveBeenCalledTimes(1));
    expect(screen.getByText("v0.9.0")).toBeTruthy();

    // When backend omits `view.version`, falls back cleanly to `__APP_VERSION__` from package.json
    const withoutBackendVer = makeMockSettingsView({ version: undefined });
    rerender(
      <SettingsPage
        view={withoutBackendVer}
        listModels={vi.fn().mockResolvedValue([])}
        onRefresh={vi.fn()}
        onRevoke={vi.fn()}
        onSave={vi.fn()}
        onSaveKey={vi.fn()}
        onAddProvider={vi.fn()}
        onRemoveProvider={vi.fn()}
      />,
    );
    expect(screen.getByText(`v${__APP_VERSION__}`)).toBeTruthy();
  });

  it("renders provider key status, queries provider models, and saves model/grant changes", async () => {
    const listModels = vi.fn().mockResolvedValue([
      ["claude-sonnet-4-6", "Claude Sonnet 4.6"],
      ["claude-haiku-4-5", "Claude Haiku 4.5"],
    ]);
    const onSave = vi.fn();
    const onRevoke = vi.fn();

    const openai = makeMockProvider({ id: "openai", name: "OpenAI", protocol: "responses", key_env: "OPENAI_API_KEY" });
    const view = makeMockSettingsView({
      providers: [makeMockProvider(), openai],
      keys: {
        anthropic: { source: "saved", preview: "sk-ant-...9999", plaintext: false },
        openai: { source: "env", preview: null, plaintext: false },
      },
      grants: {
        "/ws/demo": ["command:cargo test"],
      },
    });

    render(
      <SettingsPage
        view={view}
        listModels={listModels}
        onRefresh={vi.fn()}
        onRevoke={onRevoke}
        onSave={onSave}
        onSaveKey={vi.fn()}
        onAddProvider={vi.fn()}
        onRemoveProvider={vi.fn()}
      />,
    );

    expect(screen.getByText("sk-ant-...9999")).toBeTruthy();
    expect(screen.getByText("OPENAI_API_KEY")).toBeTruthy();

    await waitFor(() => {
      expect(listModels).toHaveBeenCalledWith("anthropic");
      expect(screen.getByText(/问到它提供的 2 门心法/)).toBeTruthy();
    });

    // Switch effort radio (`xhigh` = 元婴)
    fireEvent.click(screen.getByLabelText("元婴"));
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ effort: "xhigh" }));

    // Switch to rules tab and revoke a saved grant
    fireEvent.click(screen.getByRole("button", { name: "禁制" }));
    expect(screen.getByText("cargo test")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "收回" }));
    expect(onRevoke).toHaveBeenCalledWith("/ws/demo", "command:cargo test");
  });

  it("switches theme in look tab and updates default mode / guardian settings in rules tab", () => {
    const onSave = vi.fn();
    const view = makeMockSettingsView();
    render(
      <SettingsPage
        view={view}
        listModels={vi.fn().mockResolvedValue([])}
        onRefresh={vi.fn()}
        onRevoke={vi.fn()}
        onSave={onSave}
        onSaveKey={vi.fn()}
        onAddProvider={vi.fn()}
        onRemoveProvider={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "外观" }));
    fireEvent.click(screen.getByLabelText(/玄夜/));
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ theme: "dark" }));

    fireEvent.click(screen.getByRole("button", { name: "禁制" }));
    fireEvent.click(screen.getByRole("radio", { name: /放手施为/ }));
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ mode: "auto" }));

    const timeoutInput = document.getElementById("approval-timeout") as HTMLInputElement;
    fireEvent.change(timeoutInput, { target: { value: "30" } });
    fireEvent.blur(timeoutInput);
  });

  it("shows update notes and download link when checkAppUpdate reports a newer release", async () => {
    vi.spyOn(api, "checkAppUpdate").mockResolvedValue({
      current_version: "0.8.0",
      latest_version: "0.9.0",
      has_update: true,
      html_url: "https://github.com/VoidDel/omnidoppelganger/releases/tag/v0.9.0",
      notes: "60fps streaming & font subset",
      published_at: "2026-11-26T00:00:00Z",
    });
    const openSpy = vi.spyOn(api, "openExternal").mockResolvedValue(undefined);

    render(
      <SettingsPage
        view={makeMockSettingsView({ version: "0.8.0" })}
        listModels={vi.fn().mockResolvedValue([])}
        onRefresh={vi.fn()}
        onRevoke={vi.fn()}
        onSave={vi.fn()}
        onSaveKey={vi.fn()}
        onAddProvider={vi.fn()}
        onRemoveProvider={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "关于" }));
    fireEvent.click(screen.getByRole("button", { name: "检查更新" }));

    const dlBtn = await screen.findByRole("button", { name: "前往 GitHub 下载 v0.9.0" });
    expect(screen.getByText("60fps streaming & font subset")).toBeTruthy();
    fireEvent.click(dlBtn);
    expect(openSpy).toHaveBeenCalledWith("https://github.com/VoidDel/omnidoppelganger/releases/tag/v0.9.0");
  });

  it("merges dirty-field patches onto latest view.settings without reverting concurrent external updates (P3-5)", () => {
    const onSave = vi.fn();
    const initialView = makeMockSettingsView();
    const listModels = vi.fn().mockResolvedValue([]);

    const { rerender } = render(
      <SettingsPage
        view={initialView}
        listModels={listModels}
        onRefresh={vi.fn()}
        onRevoke={vi.fn()}
        onSave={onSave}
        onSaveKey={vi.fn()}
        onAddProvider={vi.fn()}
        onRemoveProvider={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "禁制" }));
    const timeoutInput = document.getElementById("approval-timeout") as HTMLInputElement;
    // User edits timeout draft to 45 without blurring yet
    fireEvent.change(timeoutInput, { target: { value: "45" } });
    expect(timeoutInput.value).toBe("45");

    // External concurrent change updates `mode`, `model`, and `effort` in `view.settings`
    const externallyUpdatedView = makeMockSettingsView({
      settings: {
        ...initialView.settings,
        mode: "plan",
        model: "claude-opus-4-6",
        effort: "max",
      },
    });
    rerender(
      <SettingsPage
        view={externallyUpdatedView}
        listModels={listModels}
        onRefresh={vi.fn()}
        onRevoke={vi.fn()}
        onSave={onSave}
        onSaveKey={vi.fn()}
        onAddProvider={vi.fn()}
        onRemoveProvider={vi.fn()}
      />,
    );

    // Uncommitted dirty draft stays "45", and blurring merges patch onto `externallyUpdatedView.settings`
    expect(timeoutInput.value).toBe("45");
    fireEvent.blur(timeoutInput);
    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({
        mode: "plan",
        model: "claude-opus-4-6",
        effort: "max",
        approval_timeout: 45,
      }),
    );
  });

  it("switches interface locale (updating html[lang]) and toggles auto_update_runtime (P2-13 & P2-16)", () => {
    const onSave = vi.fn();
    const view = makeMockSettingsView();

    render(
      <I18nProvider locale="zh-CN">
        <SettingsPage
          view={view}
          listModels={vi.fn().mockResolvedValue([])}
          onRefresh={vi.fn()}
          onRevoke={vi.fn()}
          onSave={onSave}
          onSaveKey={vi.fn()}
          onAddProvider={vi.fn()}
          onRemoveProvider={vi.fn()}
        />
      </I18nProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "神通" }));
    const autoRuntimeToggle = document.getElementById("auto-update-runtime") as HTMLInputElement;
    expect(autoRuntimeToggle.checked).toBe(false);
    fireEvent.click(autoRuntimeToggle);
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ auto_update_runtime: true }));

    fireEvent.click(screen.getByRole("button", { name: "外观" }));
    fireEvent.click(screen.getByRole("radio", { name: /English/ }));
    expect(document.documentElement.lang).toBe("en");
    expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ locale: "en" }));
  });
});

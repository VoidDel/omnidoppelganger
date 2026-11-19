import { useEffect, useRef, useState, type ReactNode } from "react";
import { MODES } from "../lib/modes";
import { Combo } from "./Combo";
import { mcpTool, shortCount } from "../lib/fold";
import type { KeyStatus, McpServer, Provider, Settings, SettingsView } from "../lib/types";

// The agent's realm (境界): cultivation stages, lowest to highest, for the API's five effort levels.
const EFFORT_NAMES: Record<string, string> = { low: "练气", medium: "筑基", high: "金丹", xhigh: "元婴", max: "化神" };

const PAPERS: { value: Settings["theme"]; name: string; hint: string; swatches: [string, string][] }[] = [
  { value: "light", name: "白昼", hint: "月白底，宜昼", swatches: [["#FBFCFA", "#1B2A2B"], ["#F6E7A8", "#D9BC55"], ["#B8322A", "#B8322A"]] },
  { value: "dark", name: "玄夜", hint: "玄青底，宜夜", swatches: [["#1A2A2C", "#E6EEEA"], ["#3A3212", "#7D6A1E"], ["#E0584E", "#E0584E"]] },
  { value: "system", name: "跟随系统", hint: "随系统昼夜更替", swatches: [["#FBFCFA", "#1B2A2B"], ["#1A2A2C", "#E6EEEA"]] },
];

/** How a provider's effort setting lands, for the hint under the realms. */
function effortNote(provider: Provider | undefined): string {
  if (!provider || provider.protocol === "anthropic" || provider.protocol === "responses") return "";
  switch (provider.protocol.chat) {
    case "deep_seek":
      return "DeepSeek 只分低、高、极三档：筑基和元婴按金丹算。";
    case "kimi":
    case "glm":
      return `${provider.name} 只分低、高、极三档：筑基按金丹算，元婴按化神算。`;
    default:
      return "这个服务不一定认境界，所以不发送，按服务自己的默认推演。";
  }
}

/** A text field that saves when it loses focus or Enter is pressed, not on every keystroke. */
function useDraft(value: string, save: (value: string) => void) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const commit = () => {
    if (draft.trim() !== value) save(draft.trim());
  };
  return {
    value: draft,
    onChange: (event: React.ChangeEvent<HTMLInputElement>) => setDraft(event.target.value),
    onBlur: commit,
    onKeyDown: (event: React.KeyboardEvent<HTMLInputElement>) => event.key === "Enter" && commit(),
  };
}

function KeyLine({ provider, status }: { provider: Provider; status: KeyStatus }) {
  return (
    <>
      {status.source === "saved" && <span className="muted">{status.plaintext ? "密钥存在本机的明文文件里，只显示开头和结尾" : "密钥已存进系统凭据管理器，只显示开头和结尾"}</span>}
      {status.source === "env" && <span className="muted">正在用环境变量 {provider.key_env} 里的密钥</span>}
      {status.source === "none" && <span className="muted">还没有密钥{provider.key_env ? `，也可以在启动前设置环境变量 ${provider.key_env}` : ""}</span>}
      {status.preview && <span className="mono">{status.preview}</span>}
    </>
  );
}

const NO_KEY: KeyStatus = { source: "none", preview: null, plaintext: false };

interface KeyDialogProps {
  /** The provider whose key and address are set; null to add one of `choices`. */
  provider: Provider | null;
  choices: Provider[];
  view: SettingsView;
  onSave: (settings: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onClose: () => void;
}

/** The window that takes a provider's key and address: to add a provider to the list, or to change one in it. */
function KeyDialog({ provider, choices, view, onSave, onSaveKey, onClose }: KeyDialogProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  // Shown as a modal, which keeps the keyboard inside it and closes on Escape.
  useEffect(() => {
    dialog.current?.showModal();
    // The window gives the keyboard to its first field, which would open that field's list. The key is what is typed here.
    dialog.current?.querySelector<HTMLInputElement>("#key-value")?.focus();
  }, []);
  const { settings } = view;
  const [picked, setPicked] = useState(provider?.id ?? choices[0]?.id ?? "");
  const chosen = provider ?? choices.find((choice) => choice.id === picked);
  const status = view.keys[picked] ?? NO_KEY;
  const [key, setKey] = useState("");
  const [address, setAddress] = useState(settings.base_urls[picked] ?? "");
  const [wrong, setWrong] = useState<string | null>(null);
  if (!chosen) return null;
  const takesAddress = chosen.protocol !== "anthropic";
  const save = async () => {
    const url = takesAddress ? address.trim() : "";
    if (!key.trim() && status.source === "none") return setWrong("先把密钥粘贴进来。");
    if (takesAddress && !chosen.base_url && !url) return setWrong("这个宗门没有默认的接口地址，要填上。");
    // One after the other: each answer carries the whole of the settings, and the last one to arrive is what is shown.
    if (key.trim()) await onSaveKey(chosen.id, key);
    if (url !== (settings.base_urls[chosen.id] ?? "")) {
      const { [chosen.id]: _, ...others } = settings.base_urls;
      await onSave({ ...settings, base_urls: url ? { ...others, [chosen.id]: url } : others });
    }
    onClose();
  };
  return (
    <dialog ref={dialog} className="dialog" aria-labelledby="key-dialog-title" onClose={onClose}>
      <form
        method="dialog"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <h2 id="key-dialog-title">{provider ? `${provider.name} 的密钥` : "新增宗门"}</h2>
        {!provider && (
          <div className="field">
            <label htmlFor="key-provider">哪个宗门</label>
            <Combo
              id="key-provider"
              value={picked}
              options={choices.map((choice) => ({ value: choice.id, text: choice.name }))}
              editable={false}
              onSave={(id) => {
                setPicked(id);
                setAddress(settings.base_urls[id] ?? "");
                setWrong(null);
              }}
            />
          </div>
        )}
        <div className="field">
          <label htmlFor="key-value">{status.source === "saved" ? "换一个密钥（不换就留空）" : "API 密钥"}</label>
          <input id="key-value" className="input" type="password" autoComplete="off" autoFocus placeholder="粘贴密钥" value={key} onChange={(event) => setKey(event.target.value)} />
          <p className="hint key-line">
            <KeyLine provider={chosen} status={status} />
          </p>
        </div>
        {takesAddress && (
          <div className="field">
            <label htmlFor="key-address">{chosen.base_url ? "接口地址（留空用默认）" : "接口地址（必填）"}</label>
            <input id="key-address" className="input mono" placeholder={chosen.base_url || "https://example.com/v1"} value={address} onChange={(event) => setAddress(event.target.value)} />
            {chosen.other_urls.length > 0 && (
              <p className="hint">
                {chosen.other_urls.map(([url, what]) => (
                  <span key={url}>
                    {what}：<span className="mono">{url}</span>{" "}
                  </span>
                ))}
              </p>
            )}
          </div>
        )}
        {wrong && (
          <p className="hint failed" role="alert">
            {wrong}
          </p>
        )}
        <div className="actions">
          <button type="submit" className="button primary">
            保存
          </button>
          <button type="button" className="button text" onClick={onClose}>
            算了
          </button>
        </div>
      </form>
    </dialog>
  );
}

/**
 * The providers that are set up: those with a key, or an address of their own, and the one the default 分身 uses,
 * so that a missing key shows. They are what the list holds, and what the default can be picked from.
 */
function setUp(view: SettingsView): Provider[] {
  const { settings } = view;
  return view.providers.filter((provider) => (view.keys[provider.id] ?? NO_KEY).source !== "none" || !!settings.base_urls[provider.id] || provider.id === settings.provider);
}

interface KeyListProps {
  view: SettingsView;
  onSave: (settings: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onDeleteKey: (provider: string) => unknown;
}

/**
 * The providers that are set up, one to a line: which has a key, and where its requests go. One is added, or
 * changed, in a window of its own. The provider new 分身 use is always listed, so a missing key shows.
 */
function KeyList({ view, onSave, onSaveKey, onDeleteKey }: KeyListProps) {
  const { settings } = view;
  /** The window that is open: for the provider with this id, or `new` to add one. */
  const [editing, setEditing] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const statusOf = (provider: Provider) => view.keys[provider.id] ?? NO_KEY;
  const listed = setUp(view);
  const rest = view.providers.filter((provider) => !listed.includes(provider));
  /** Take a provider out of the list: its saved key, and the address set for it. A key from the environment is not ours to remove. */
  const remove = async (provider: Provider) => {
    setRemoving(null);
    if (statusOf(provider).source === "saved") await onDeleteKey(provider.id);
    if (settings.base_urls[provider.id]) {
      const { [provider.id]: _, ...others } = settings.base_urls;
      await onSave({ ...settings, base_urls: others });
    }
  };
  return (
    <section>
      <div className="section-head">
        <h2>宗门</h2>
        <span className="spacer" />
        <button type="button" className="button primary compact" disabled={rest.length === 0} title={rest.length === 0 ? "每个宗门都在列表里了" : "添一个宗门（模型供应商），填上它的密钥"} onClick={() => setEditing("new")}>
          <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
            <path d="M8 3v10M3 8h10" />
          </svg>
          新增宗门
        </button>
      </div>
      <ul className="key-list">
        {listed.map((provider) => {
          const status = statusOf(provider);
          const address = settings.base_urls[provider.id];
          const removable = status.source === "saved" || !!address;
          // Where its requests go, when that can be set: Anthropic's address comes from the environment.
          const shown = provider.protocol === "anthropic" ? "" : address || "默认地址";
          return (
            <li key={provider.id}>
              <div className="key-who">
                <strong>{provider.name}</strong>
                {provider.id === settings.provider && (
                  <span className="chip busy" title="默认分身出自这个宗门，在下面「默认分身」里换">
                    默认
                  </span>
                )}
              </div>
              <div className={`key-what ${status.source === "none" ? "missing" : ""} ${shown ? "" : "wide"}`}>
                {status.source === "saved" && <span className="mono">{status.preview}</span>}
                {status.source === "env" && (
                  <>
                    环境变量 <span className="mono">{provider.key_env}</span>
                  </>
                )}
                {status.source === "none" && "未填密钥"}
              </div>
              {shown && (
                <div className="key-where mono" title={address || provider.base_url || undefined}>
                  {shown}
                </div>
              )}
              {removing === provider.id ? (
                <span className="undo-confirm">
                  <button type="button" className="button outline red small" onClick={() => void remove(provider)}>
                    确认删除
                  </button>
                  <button type="button" className="button text small" onClick={() => setRemoving(null)}>
                    算了
                  </button>
                </span>
              ) : (
                <span className="key-acts">
                  <button type="button" className="icon-button" title={`改 ${provider.name} 的密钥或地址`} onClick={() => setEditing(provider.id)}>
                    <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M10.6 2.9 13.1 5.4 5.6 12.9 2.6 13.4 3.1 10.4Z" />
                      <path d="m9.2 4.3 2.5 2.5" />
                    </svg>
                    <span className="sr-only">改 {provider.name}</span>
                  </button>
                  <button
                    type="button"
                    className="icon-button danger"
                    disabled={!removable}
                    title={removable ? `删掉 ${provider.name} 存着的密钥和改过的地址` : status.source === "env" ? "密钥来自环境变量，这里删不了" : "没有可删的"}
                    onClick={() => setRemoving(provider.id)}
                  >
                    <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.1 4.3l.6 8.9h6.6l.6-8.9M6.6 6.8v4M9.4 6.8v4" />
                    </svg>
                    <span className="sr-only">删除 {provider.name}</span>
                  </button>
                </span>
              )}
            </li>
          );
        })}
      </ul>
      <p className="muted">
        密钥存进系统的凭据管理器（Windows 凭据管理器、macOS 钥匙串、Linux 的 Secret Service），只显示开头和结尾。凭据管理器用不了时，才退回 {view.home} 目录里的明文文件。
      </p>
      {editing && (
        <KeyDialog
          provider={editing === "new" ? null : (view.providers.find((provider) => provider.id === editing) ?? null)}
          choices={rest}
          view={view}
          onSave={onSave}
          onSaveKey={onSaveKey}
          onClose={() => setEditing(null)}
        />
      )}
    </section>
  );
}

const MCP_STATES: Record<McpServer["state"], string> = { connecting: "正在连接", ready: "已连上", failed: "没连上" };

/** A kept grant as the user reads it: a command's opening words, or an MCP server's tool. */
function grantLabel(grant: string): string {
  const borrowed = grant.startsWith("mcp:") ? mcpTool(grant.slice("mcp:".length)) : null;
  if (grant === "search") return "向天机阁打探（联网搜索）";
  if (grant.startsWith("web:")) return `网站 ${grant.slice("web:".length)}`;
  return borrowed ? `法器 ${borrowed.server} 的 ${borrowed.tool}` : grant.replace(/^command:/, "");
}

interface McpSectionProps {
  config: string;
  servers: McpServer[];
  /** Resolves with what is wrong with the text, or null once it is saved. */
  onSave: (config: string) => Promise<string | null>;
  onRefresh: () => void;
}

/** The MCP server list as text to edit, and how each server stands. */
function McpSection({ config, servers, onSave, onRefresh }: McpSectionProps) {
  const [draft, setDraft] = useState(config);
  const [problem, setProblem] = useState<string | null>(null);
  useEffect(() => setDraft(config), [config]);
  // Servers connect in the background: look again until none is still connecting.
  const connecting = servers.some((server) => server.state === "connecting");
  useEffect(() => {
    if (!connecting) return;
    const timer = setInterval(onRefresh, 1500);
    return () => clearInterval(timer);
  }, [connecting, onRefresh]);
  return (
    <section>
      <h2>身外法器（MCP 服务）</h2>
      <div className="field">
        <label htmlFor="mcp-config">服务清单</label>
        <textarea id="mcp-config" className="input mono config" rows={5} spellCheck={false} value={draft} onChange={(event) => setDraft(event.target.value)} />
        <p className="hint">
          用各家通用的 mcpServers 写法，MCP 服务说明里给的配置片段可以直接贴进来。本机程序写 command 和 args，远程服务写 url（需要的话加 headers）。保存后立即连接；连上的服务提供的工具，分身每次动用前都要请示。
        </p>
        {problem && (
          <p className="hint failed" role="alert">
            清单没有保存：{problem}
          </p>
        )}
        <div className="actions">
          <button type="button" className="button primary" disabled={draft === config} onClick={() => onSave(draft).then(setProblem)}>
            保存并连接
          </button>
        </div>
      </div>
      {servers.map((server) => (
        <div className="provider-card" key={server.name}>
          <div className="who">
            <strong className="mono">{server.name}</strong>
            <span className={server.state === "failed" ? "failed" : "muted"}>
              {MCP_STATES[server.state]}
              {server.state === "ready" && `，${server.tools.length} 件法器`}
            </span>
          </div>
          {server.error && <p className="hint failed">{server.error}</p>}
          {server.tools.length > 0 && <p className="hint mono">{server.tools.join("、")}</p>}
        </div>
      ))}
    </section>
  );
}

/** Who says how large a model's context window is. */
const WINDOW_SOURCES: Record<SettingsView["context_window"]["source"], string> = {
  set: "本尊自己填的",
  provider: "宗门自己报的",
  documented: "照宗门文档记的",
  unknown: "宗门没报，表里也没有，按保守值估的",
};

/** The pages the settings are sorted into: what a 分身 runs on, what it may do, what it can reach, and how the app looks. */
export type SettingsTab = "model" | "rules" | "tools" | "look";

const TABS: { id: SettingsTab; name: string; hint: string; icon: ReactNode }[] = [
  {
    id: "model",
    name: "功法",
    hint: "各个宗门和它们的密钥，默认分身修哪门功法、推演多深，前情留多长",
    icon: <path d="M4.5 13.5A1.5 1.5 0 0 1 3 12V4a1.5 1.5 0 0 1 1.5-1.5H13v8.5H4.5A1.5 1.5 0 0 0 3 12.5M13 11v2.5H4.5M6 5.5h4" />,
  },
  { id: "rules", name: "禁制", hint: "分身不问就能做什么，结界（沙箱），护法（模型审核），发出去的通行符（长期许可）", icon: <path d="M8 1.8 13 3.6v4.2c0 3-2.1 5.3-5 6.4-2.9-1.1-5-3.4-5-6.4V3.6z" /> },
  { id: "tools", name: "法器", hint: "天机阁（联网搜索）、技能、身外法器（MCP 服务）", icon: <path d="M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3" /> },
  {
    id: "look",
    name: "外观",
    hint: "白昼还是玄夜",
    icon: (
      <>
        <circle cx="8" cy="8" r="5.5" />
        <path d="M8 2.5v11a5.5 5.5 0 0 0 0-11z" fill="currentColor" />
      </>
    ),
  },
];

interface Props {
  view: SettingsView;
  /** The models a provider serves, asked from the provider itself. */
  listModels: (provider: string) => Promise<[string, string][]>;
  onSaveMcp: (config: string) => Promise<string | null>;
  onRefresh: () => void;
  onRevoke: (workspace: string, grant: string) => void;
  onSave: (settings: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onDeleteKey: (provider: string) => unknown;
}

export function SettingsPage({ view, onSave, onSaveKey, onDeleteKey, onRevoke, onSaveMcp, onRefresh, listModels }: Props) {
  const [tab, setTab] = useState<SettingsTab>("model");
  const grants = Object.entries(view.grants);
  const { settings } = view;
  const chosen = view.providers.find((provider) => provider.id === settings.provider);
  const note = effortNote(chosen);
  // What the chosen provider says it serves, asked again when the provider or its key changes.
  const [served, setServed] = useState<{ models: [string, string][]; why: string | null }>({ models: [], why: null });
  const keyed = view.keys[settings.provider]?.source ?? "none";
  const [asked, setAsked] = useState(0);
  const [asking, setAsking] = useState(false);
  useEffect(() => {
    let current = true;
    setServed({ models: [], why: null });
    if (keyed === "none") return;
    setAsking(true);
    listModels(settings.provider)
      .then(
        (models) => {
          if (!current) return;
          setServed({ models, why: null });
          // The provider may have said how large its models' context windows are: read what is now known.
          onRefresh();
        },
        (error) => current && setServed({ models: [], why: String(error) }),
      )
      .finally(() => current && setAsking(false));
    return () => {
      current = false;
      setAsking(false);
    };
    // `asked` counts the times the user asked for the list again.
  }, [settings.provider, keyed, listModels, asked]);
  // The provider's own list first; the built-in suggestions it does not name after it.
  const suggestions = [...served.models, ...(chosen?.models ?? []).filter(([id]) => !served.models.some(([listed]) => listed === id))];
  /** The most the limit can be for the model new 分身 use: four fifths of its context window. */
  const ceiling = Math.floor(view.context_window.tokens / 5) * 4;
  const timeout = useDraft(String(settings.approval_timeout), (value) => {
    const seconds = Number.parseInt(value, 10);
    if (Number.isFinite(seconds) && seconds >= 0) onSave({ ...settings, approval_timeout: seconds });
  });
  const windowKey = `${settings.provider}/${settings.model}`;
  /** Whether the field that corrects the model's window is open, and what is typed in it. */
  const [editingWindow, setEditingWindow] = useState(false);
  const [windowDraft, setWindowDraft] = useState("");
  // Another model has another figure: the field is put away, and starts from that model's own.
  useEffect(() => {
    setEditingWindow(false);
    setWindowDraft(settings.context_windows[windowKey] ? String(settings.context_windows[windowKey]) : "");
  }, [windowKey, settings.context_windows[windowKey]]);
  /** Keep the window typed for this model, or with nothing typed leave it to be found out; then put the field away. */
  const saveWindow = (value: string) => {
    setEditingWindow(false);
    const { [windowKey]: kept, ...others } = settings.context_windows;
    const tokens = Number.parseInt(value, 10);
    const next = Number.isFinite(tokens) && tokens > 0 ? tokens : undefined;
    if (next !== kept) onSave({ ...settings, context_windows: next ? { ...others, [windowKey]: next } : others });
  };
  const limit = useDraft(String(settings.context_limit), (value) => {
    const tokens = Number.parseInt(value, 10);
    if (Number.isFinite(tokens)) onSave({ ...settings, context_limit: tokens });
  });
  return (
    <div className="settings">
      {/* Inside what scrolls, and kept at its top: the strip then lines up with the column under it, whatever the scrollbar takes. */}
      <header className="settings-tabs">
        <div className="settings-tabs-inner">
        {TABS.map((item) => (
          <button type="button" className="tab" key={item.id} aria-pressed={tab === item.id} title={item.hint} onClick={() => setTab(item.id)}>
            <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
              {item.icon}
            </svg>
            {item.name}
          </button>
        ))}
        </div>
      </header>
      <div className="settings-inner">
        {tab === "model" && (
          <>
        <KeyList view={view} onSave={onSave} onSaveKey={onSaveKey} onDeleteKey={onDeleteKey} />
        <section>
          <h2>默认分身</h2>
          <p className="muted">新化出的分身照这里的来。已经化出的不受影响，要换功法在它输入框下方换。</p>
          <div className="field">
            <label htmlFor="default-provider">宗门（模型供应商）</label>
            <div className="inline model-row">
              {/* Only the 宗门 in the list above: one that is not set up could not answer. */}
              <Combo
                id="default-provider"
                value={settings.provider}
                options={setUp(view).map((provider) => ({ value: provider.id, text: provider.name, note: view.keys[provider.id]?.source === "none" ? "未填密钥" : "" }))}
                editable={false}
                onSave={(id) => onSave({ ...settings, provider: id, model: view.providers.find((provider) => provider.id === id)?.models[0]?.[0] ?? "" })}
              />
            </div>
            <p className="hint">只列上面已有的宗门。要用别家，先点「新增宗门」把它添进来。</p>
          </div>
          <div className="field">
            <label htmlFor="default-model">功法（模型）</label>
            <div className="inline model-row">
              <Combo
                id="default-model"
                value={settings.model}
                options={suggestions.map(([id, name]) => ({ value: id, text: id, note: name === id ? "" : name }))}
                editable
                mono
                placeholder="填写模型 ID"
                onSave={(value) => onSave({ ...settings, model: value })}
              />
              <button
                type="button"
                className={`icon-button ${asking ? "turning" : ""}`}
                disabled={asking || keyed === "none"}
                title={keyed === "none" ? "先给这个宗门填上密钥，才问得到它的功法" : asking ? "正在问……" : `再向 ${chosen?.name ?? settings.provider} 问一遍它有哪些功法`}
                onClick={() => setAsked((times) => times + 1)}
              >
                <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M13.2 8a5.2 5.2 0 0 1-9.3 3.2M2.8 8a5.2 5.2 0 0 1 9.3-3.2" />
                  <path d="M12.6 2.2v2.9H9.7M3.4 13.8v-2.9h2.9" />
                </svg>
                <span className="sr-only">刷新功法列表</span>
              </button>
            </div>
            <p className="hint">
              {asking
                ? `正在向 ${chosen?.name ?? settings.provider} 问它有哪些功法……`
                : served.models.length > 0
                ? `已向 ${chosen?.name ?? settings.provider} 问到它提供的 ${served.models.length} 门功法，可以从列表里选，也可以直接填。`
                : served.why
                  ? `没能向 ${chosen?.name ?? settings.provider} 问到功法列表（${served.why}），先用内置的几门，也可以直接填模型 ID。`
                  : keyed === "none"
                    ? "填上这个宗门的密钥后，会自动向它问有哪些功法。眼下可以从内置的几门里选，或直接填模型 ID。"
                    : "可以从列表里选，也可以直接填模型 ID。"}
            </p>
            {/* How much the model itself can hold, and who says so: a fact about the model, so one line, with the way to correct it kept out of sight. */}
            <p className="hint window-line" id="context-window">
              {settings.model} 的上下文窗口：{shortCount(view.context_window.tokens)} token（{WINDOW_SOURCES[view.context_window.source]}）。
              {!editingWindow && (
                <button type="button" className="inline-link" title="程序查不到或查错了时，自己填一个" onClick={() => setEditingWindow(true)}>
                  改
                </button>
              )}
              {view.context_window.source === "set" && (
                <button type="button" className="inline-link" title="不用自己填的数，回到程序查到的" onClick={() => saveWindow("")}>
                  改回自动
                </button>
              )}
            </p>
          </div>
          {editingWindow && (
            <div className="field">
              <label htmlFor="context-window-set">这门功法一次最多能装多少（token）</label>
              <input
                id="context-window-set"
                className="input mono"
                type="number"
                min={25000}
                step={1000}
                autoFocus
                placeholder={String(view.context_window.tokens)}
                value={windowDraft}
                onChange={(event) => setWindowDraft(event.target.value)}
                onBlur={() => saveWindow(windowDraft)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") saveWindow(windowDraft);
                  if (event.key === "Escape") setEditingWindow(false);
                }}
              />
              <p className="hint">这是功法自己的硬指标，不是想让对话多长：填得比实际大，请求会被宗门拒掉。留空不改。</p>
            </div>
          )}
          <fieldset className="field">
            <legend>境界</legend>
            <div className="realms">
              {view.efforts.map((effort) => (
                <label className="realm" key={effort}>
                  <input type="radio" name="realm" checked={settings.effort === effort} onChange={() => onSave({ ...settings, effort })} />
                  <span className="name">{EFFORT_NAMES[effort] ?? effort}</span>
                </label>
              ))}
            </div>
            <p className="hint">
              境界越高，分身每一步推演得越深，也越慢、越费灵石（额度）。下一次请求起生效。{note}
            </p>
          </fieldset>
        </section>

        <section>
          <h2>灵石</h2>
          <div className="field">
            <span className="field-label" id="context-limit-label">前情上限</span>
            <div className="kinds" role="radiogroup" aria-labelledby="context-limit-label">
              <label className="kind">
                <input type="radio" name="context-limit-kind" checked={settings.context_auto} onChange={() => onSave({ ...settings, context_auto: true })} />
                自动
              </label>
              <label className="kind">
                <input type="radio" name="context-limit-kind" checked={!settings.context_auto} onChange={() => onSave({ ...settings, context_auto: false })} />
                自己定
              </label>
            </div>
            {!settings.context_auto && (
              <>
                <label htmlFor="context-limit" className="sr-only">
                  前情上限（token）
                </label>
                <input id="context-limit" className="input mono" type="number" min={20000} step={10000} {...limit} />
              </>
            )}
            <p className="hint">
              每次请求都要把整段对话发给模型，对话越长越费灵石。对话到上限，分身会把前情凝成一段摘要，之后只凭摘要接着做。
              {settings.context_auto
                ? `自动：按每门功法自己的窗口，用到它的八成，${settings.model} 眼下是 ${shortCount(ceiling)}。窗口大的功法前情留得长，也更费灵石。`
                : "自己定一个数（token，最低 20000），对所有功法都用它；功法装不下这么多时，按它窗口的八成算。"}
            </p>
          </div>
          {/* Said here only when it matters: the model cannot hold as much as the limit asks for. */}
          {!settings.context_auto && settings.context_limit > ceiling && (
            <p className="hint" id="context-capped">
              {settings.model} 的上下文窗口只有 {shortCount(view.context_window.tokens)}，前情上限最多用到它的八成，所以眼下实际是 {shortCount(ceiling)}。
            </p>
          )}
        </section>
          </>
        )}
        {tab === "rules" && (
          <>
        <section>
          <h2>禁制</h2>
          <fieldset className="papers pairs">
            <legend>新分身起始的禁制</legend>
            {MODES.map((option) => (
              <label className="paper" key={option.value}>
                <span className="name">
                  <input type="radio" name="mode" checked={settings.mode === option.value} onChange={() => onSave({ ...settings, mode: option.value })} />
                  {option.name}
                </span>
                <span className="muted">{option.hint}</span>
              </label>
            ))}
          </fieldset>
          <p className="hint">已经化出的分身，在输入框下方或按 Shift+Tab 随时换禁制。无论哪种禁制，碰洞府之外的东西、改 .git 里的文件都要请示。</p>
        </section>

        <section>
          <h2>结界（沙箱）</h2>
          <label className="check">
            <input type="checkbox" checked={settings.sandbox && !!view.sandbox_support.Ok} disabled={!view.sandbox_support.Ok} onChange={(event) => onSave({ ...settings, sandbox: event.target.checked })} />
            <span>
              给洞府布下结界，把分身运行的命令关在里面
              <span className="muted">（命令只能往洞府和临时目录里写；读文件、联网不受限）</span>
            </span>
          </label>
          {view.sandbox_support.Ok ? (
            <p className="hint">
              用的是系统自带的 {view.sandbox_support.Ok}。开了之后，命令（连同它启动的一切，比如构建脚本和测试）改不了洞府之外的文件。禁制照旧：该请示的命令还是请示。
              命令确实要往别处写时，分身会请示出结界跑一次，每次都要本尊点头。下一条命令起生效。
            </p>
          ) : (
            <p className="hint" title={view.sandbox_support.Err}>
              这个系统上还布不了结界，眼下只有 Linux 和 macOS 可以。
            </p>
          )}
          {/* Nothing to add directories to where nothing is confined. */}
          <div className="field" hidden={!view.sandbox_support.Ok}>
            <label htmlFor="sandbox-paths">另外准许写入的目录（一行一个）</label>
            <textarea
              id="sandbox-paths"
              className="input area mono"
              rows={3}
              disabled={!view.sandbox_support.Ok}
              placeholder="比如包管理器的缓存目录"
              key={settings.sandbox_paths.join("\n")}
              defaultValue={settings.sandbox_paths.join("\n")}
              onBlur={(event) => {
                const paths = event.target.value.split("\n").map((line) => line.trim()).filter(Boolean);
                if (paths.join("\n") !== settings.sandbox_paths.join("\n")) onSave({ ...settings, sandbox_paths: paths });
              }}
            />
            <p className="hint">Git 仓库自己的 .git 目录不用填，已经算在内。</p>
          </div>
        </section>

        <section>
          <h2>护法（模型审核）</h2>
          <label className="check">
            <input type="checkbox" checked={settings.guardian} onChange={(event) => onSave({ ...settings, guardian: event.target.checked })} />
            <span>
              放手施为时，请一位护法先审拿不准的命令
              <span className="muted">（护法是另一个模型：它放行的直接跑，它不放行的才请本尊定夺）</span>
            </span>
          </label>
          <p className="hint">
            只管放手施为下规则不放行的命令；别的禁制、改洞府之外的文件、上网、身外法器照旧请示，全权托付不经护法。护法只看得到本尊说过的话、分身动手前说的打算和命令本身，看不到文件和命令输出里的字。
            它会看走眼，也会被骗，不是保险；每审一次花一两千 token 的灵石，慢一两秒。
          </p>
          {settings.guardian && (
            <div className="field">
              <label htmlFor="guardian-model">护法的功法（{chosen?.name ?? settings.provider}）</label>
              <div className="inline model-row">
                <Combo
                  id="guardian-model"
                  value={settings.guardian_models[settings.provider] ?? ""}
                  options={suggestions.map(([id, name]) => ({ value: id, text: id, note: name === id ? "" : name }))}
                  editable
                  mono
                  placeholder="留空就用分身自己的功法"
                  onSave={(model) => {
                    const { [settings.provider]: _, ...others } = settings.guardian_models;
                    onSave({ ...settings, guardian_models: model ? { ...others, [settings.provider]: model } : others });
                  }}
                />
              </div>
              <p className="hint">挑一门又快又便宜的。护法和分身用同一个宗门，这里设的是默认宗门的；别的宗门没设时，用分身自己的功法。</p>
            </div>
          )}
          <div className="field">
            <label htmlFor="approval-timeout">放手施为下，请示等多久（秒）</label>
            <input id="approval-timeout" className="input mono short" type="number" min={0} step={5} {...timeout} />
            <p className="hint">过了这个时间没人定夺，就自动驳回，分身绕开那一步接着做，并在回报里说明。填 0 就一直等。别的禁制下一直等。</p>
          </div>
        </section>

        <section>
          <h2>通行符（长期许可）</h2>
          {grants.length === 0 && <p className="muted">还没有发出过通行符。请示时选"此府永准"，就给那一类命令发了一道：之后同一处洞府里，以同样开头的命令过禁制不再请示。</p>}
          {grants.map(([workspace, kept]) => (
            <div className="provider-card" key={workspace}>
              <div className="who">
                <strong className="mono">{workspace}</strong>
                <span className="muted">这处洞府里，持通行符的不再请示：以下面这些开头的命令、这些法器、这些网站</span>
              </div>
              {kept.map((grant) => (
                <div className="provider" key={grant}>
                  <span className="mono">{grantLabel(grant)}</span>
                  <button type="button" className="button text" onClick={() => onRevoke(workspace, grant)}>
                    收回
                  </button>
                </div>
              ))}
            </div>
          ))}
        </section>
          </>
        )}
        {tab === "tools" && (
          <>
        <section>
          <h2>天机阁（联网搜索）</h2>
          <div className="field">
            <label htmlFor="search-engine">向哪家打探</label>
            <div className="inline model-row">
              <Combo
                id="search-engine"
                value={settings.search_engine}
                options={view.search_engines.map(([id, what]) => ({ value: id, text: what }))}
                editable={false}
                onSave={(engine) => onSave({ ...settings, search_engine: engine })}
              />
            </div>
            <p className="hint">
              搜索走智谱的搜索接口，用的就是 GLM（智谱）那把密钥，换哪家功法都能搜。
              {view.search_ready ? "密钥已有，分身可以搜；每次搜索前要请示，按次花灵石。" : "还没有 GLM 的密钥，所以分身现在不能搜：到「功法」那一页给 GLM 添上密钥就行。"}
              GLM 的接口地址换成国际站 z.ai 时，只有那边自带的一种搜索，这里的选择不起作用。
            </p>
          </div>
        </section>

        <section>
          <h2>技能</h2>
          <p className="muted">
            技能是专项的做事说明，平时不占分身的心神，遇到对得上的事它才去研读。每门技能是一个文件夹，里面放一份 SKILL.md，开头用 --- 围起来写上 name 和 description。放在{" "}
            <span className="mono">{view.skills_directory}</span> 里的对所有分身生效；放在某处洞府的 <span className="mono">.odg/skills</span> 里的只对那处洞府生效，重名时以洞府里的为准。新放进去的，下一轮生效。
          </p>
          {view.skills.length === 0 && <p className="muted">还没有装技能。</p>}
          {view.skills.map((skill) => (
            <div className="provider-card" key={skill.name}>
              <div className="who">
                <strong className="mono">{skill.name}</strong>
                <span className="muted">{skill.description}</span>
              </div>
            </div>
          ))}
        </section>

        <McpSection config={view.mcp_config} servers={view.mcp_servers} onSave={onSaveMcp} onRefresh={onRefresh} />
          </>
        )}
        {tab === "look" && (
          <>
        <section>
          <h2>外观</h2>
          <fieldset className="papers">
            <legend>天色</legend>
            {PAPERS.map((paper) => (
              <label className="paper" key={paper.value}>
                <span className="name">
                  <input type="radio" name="paper" checked={settings.theme === paper.value} onChange={() => onSave({ ...settings, theme: paper.value })} />
                  {paper.name}
                </span>
                <span className="swatches" aria-hidden="true">
                  {paper.swatches.map(([fill, border], index) => (
                    <span key={index} style={{ background: fill, borderColor: border }} />
                  ))}
                </span>
                <span className="muted">{paper.hint}</span>
              </label>
            ))}
          </fieldset>
        </section>
          </>
        )}
      </div>
    </div>
  );
}

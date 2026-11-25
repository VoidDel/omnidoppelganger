import { useEffect, useRef, useState, type ReactNode } from "react";
import { MODES } from "../lib/modes";
import { Combo } from "./Combo";
import { ConfirmBubble } from "./ConfirmBubble";
import { Info } from "./Hint";
import { CLI_INFO, cliOf, enabledProviders, setUp } from "../lib/providers";
import { RuntimeSection } from "./Runtime";
import { LanguageServersSection } from "./LanguageServers";
import { EFFORT_NAMES, effortNote } from "../lib/efforts";
import { mcpTool, shortCount } from "../lib/fold";
import { formatResetIn, quotaWindowLabel, useAntigravityQuota } from "../lib/modelUsage";
import type { KeyStatus, Provider, Settings, SettingsView } from "../lib/types";

const PAPERS: { value: Settings["theme"]; name: string; hint: string; swatches: [string, string][] }[] = [
  { value: "light", name: "白昼", hint: "月白底，宜昼", swatches: [["#FBFCFA", "#1B2A2B"], ["#F6E7A8", "#D9BC55"], ["#B8322A", "#B8322A"]] },
  { value: "dark", name: "玄夜", hint: "玄青底，宜夜", swatches: [["#1A2A2C", "#E6EEEA"], ["#ECD88C", "#B8962E"], ["#E0584E", "#E0584E"]] },
  { value: "system", name: "跟随系统", hint: "随系统昼夜更替", swatches: [["#FBFCFA", "#1B2A2B"], ["#1A2A2C", "#E6EEEA"]] },
];

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

function KeyLine({ provider, status }: { provider: { key_env: string | null; protocol?: Provider["protocol"] }; status: KeyStatus }) {
  const cli = cliOf(provider);
  const cliBin = cli ? CLI_INFO[cli].bin : null;
  return (
    <>
      {status.source === "saved" && <span className="muted">{status.plaintext ? "密钥存在本机的明文文件里，只显示开头和结尾" : "密钥已存进系统凭据管理器，只显示开头和结尾"}</span>}
      {status.source === "env" && <span className="muted">正在用环境变量 {provider.key_env} 里的密钥</span>}
      {status.source === "cli" && <span className="muted">复用本机 {cliBin ?? "CLI"} 登录凭据（无需 API 密钥）</span>}
      {status.source === "none" && (
        <span className="muted">
          {cliBin
            ? `未检测到本机 ${cliBin} CLI，请先安装并在终端运行 ${cliBin} 完成登录`
            : `还没有密钥${provider.key_env ? `，也可以在启动前设置环境变量 ${provider.key_env}` : ""}`}
        </span>
      )}
      {status.preview && <span className="mono">{status.preview}</span>}
    </>
  );
}

const NO_KEY: KeyStatus = { source: "none", preview: null, plaintext: false };

/** A provider put in the list, and the settings as they are after, or null when it could not be. */
type Added = { id: string; view: SettingsView } | null;

interface KeyDialogProps {
  /** The provider whose name, key and address are set; null to add one. */
  provider: Provider | null;
  view: SettingsView;
  onSave: (settings: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onAdd: (kind: string, name: string) => Promise<Added>;
  onClose: () => void;
}

/**
 * Whether the first provider of `kind` is not set up yet (no key, no address, not the default, not named), so that
 * adding one of the kind puts that one in the list rather than another beside it. As the core decides it.
 */
function firstFree(view: SettingsView, kind: string): boolean {
  const { settings } = view;
  return (view.keys[kind] ?? NO_KEY).source === "none" && !settings.base_urls[kind] && settings.provider !== kind && !settings.provider_names[kind];
}

/**
 * The window that takes a provider's name, key and address: to add one, of any kind, as many of a kind as wanted,
 * or to change one in the list.
 */
function KeyDialog({ provider, view, onSave, onSaveKey, onAdd, onClose }: KeyDialogProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  // Shown as a modal, which keeps the keyboard inside it and closes on Escape.
  useEffect(() => {
    dialog.current?.showModal();
    // The window gives the keyboard to its first field, which would open that field's list. The key is what is typed here.
    dialog.current?.querySelector<HTMLInputElement>("#key-value")?.focus();
  }, []);
  const { settings } = view;
  const [picked, setPicked] = useState(provider?.kind ?? view.kinds[0]?.id ?? "");
  const kind = view.kinds.find((one) => one.id === (provider?.kind ?? picked));
  const [name, setName] = useState(provider?.name ?? "");
  const [key, setKey] = useState("");
  const [address, setAddress] = useState(provider ? (settings.base_urls[provider.id] ?? "") : "");
  const [wrong, setWrong] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (!kind) return null;
  // The provider this window sets: the one changed, or the one adding will make, the first of its kind while that is free.
  const target = provider?.id ?? (firstFree(view, kind.id) ? kind.id : null);
  const status = target ? (view.keys[target] ?? NO_KEY) : NO_KEY;
  // Local CLI backends (`agy`, `codex`, `claude`, `cursor-agent`, `grok`) reuse their own CLI login and need neither an API key nor an HTTP base URL.
  const cli = cliOf(kind);
  const isCli = cli !== null;
  const isLocalGateway = kind.id === "magpie";
  const takesAddress = target !== "anthropic" && !isCli;
  const save = async () => {
    const url = takesAddress ? address.trim() : "";
    if (!isCli && !isLocalGateway && !key.trim() && status.source === "none") return setWrong("先把密钥粘贴进来。");
    if (cli && status.source === "none") return setWrong(`未检测到本机 ${CLI_INFO[cli].bin} CLI，请先安装并在终端运行 ${CLI_INFO[cli].bin} 完成登录。`);
    if (takesAddress && !kind.base_url && !url) return setWrong("这个宗门没有默认的接口地址，要填上。");
    setBusy(true);
    let id = provider?.id;
    let base = settings;
    if (!id) {
      const added = await onAdd(kind.id, name.trim());
      if (!added) return setBusy(false);
      ({ id } = added);
      base = added.view.settings;
    }
    // One after the other: each answer carries the whole of the settings, and the last one to arrive is what is shown.
    if (key.trim()) await onSaveKey(id, key);
    const { [id]: _url, ...otherUrls } = base.base_urls;
    const { [id]: _name, ...otherNames } = base.provider_names;
    const renamed = provider && name.trim() && name.trim() !== provider.name;
    // A name the same as its kind's is no name of its own, except for one added, which keeps the name it has.
    const names = renamed ? (name.trim() === kind.name && id === kind.id ? otherNames : { ...otherNames, [id]: name.trim() }) : base.provider_names;
    // For local loopback gateways (Magpie), persist the default URL when left blank so it appears in the configured 宗门 list without requiring an API key.
    const effectiveUrl = url || (isLocalGateway && !key.trim() && status.source === "none" ? kind.base_url : "");
    const urls = effectiveUrl ? { ...otherUrls, [id]: effectiveUrl } : otherUrls;
    if (effectiveUrl !== (base.base_urls[id] ?? "") || names !== base.provider_names) await onSave({ ...base, base_urls: urls, provider_names: names });
    setBusy(false);
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
        <h2 id="key-dialog-title">{provider ? `修改「${provider.name}」` : "新增宗门"}</h2>
        {!provider && (
          <div className="field">
            <label htmlFor="key-provider">哪种宗门</label>
            <Combo
              id="key-provider"
              value={picked}
              options={view.kinds.map((one) => ({ value: one.id, text: one.name, note: firstFree(view, one.id) ? "" : "再加一个" }))}
              editable={false}
              onSave={(id) => {
                setPicked(id);
                setWrong(null);
              }}
            />
          </div>
        )}
        <div className="field">
          <div className="label-line">
            <label htmlFor="key-name">名字</label>
            <Info>在宗门列表和心法菜单里显示的名字。同一种宗门加了几个时，靠它分清；不填就用「{kind.name}」{target === kind.id ? "" : "加上编号"}。</Info>
          </div>
          <input id="key-name" className="input" placeholder={kind.name} value={name} onChange={(event) => setName(event.target.value)} />
        </div>
        <div className="field">
          {!isCli && (
            <>
              <label htmlFor="key-value">
                {status.source === "saved" ? "换一个密钥（不换就留空）" : isLocalGateway ? "API 密钥（可选，本机 Magpie 默认免密）" : "API 密钥"}
              </label>
              <input
                id="key-value"
                className="input"
                type="password"
                autoComplete="off"
                autoFocus
                placeholder={isLocalGateway ? "留空即可；若 Magpie 开启鉴权再填写" : "粘贴密钥"}
                value={key}
                onChange={(event) => setKey(event.target.value)}
              />
            </>
          )}
          <p className="hint key-line">
            <KeyLine provider={{ ...kind, key_env: target === kind.id ? kind.key_env : null }} status={status} />
          </p>
        </div>
        {takesAddress && (
          <div className="field">
            <label htmlFor="key-address">{kind.base_url ? "接口地址（留空用默认）" : "接口地址（必填）"}</label>
            <input
              id="key-address"
              className="input mono"
              placeholder={kind.base_url || (kind.protocol === "anthropic" ? "https://example.com/anthropic" : "https://example.com/v1")}
              value={address}
              onChange={(event) => setAddress(event.target.value)}
            />
            {kind.protocol === "anthropic" && !kind.base_url && <p className="hint">填到 /v1 之前为止，请求会发到「地址/v1/messages」，和 ANTHROPIC_BASE_URL 的写法一样。</p>}
            {kind.other_urls.length > 0 && (
              <p className="hint">
                {kind.other_urls.map(([url, what]) => (
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
          <button type="submit" className="button primary" disabled={busy}>
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

interface KeyListProps {
  view: SettingsView;
  onSave: (settings: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onAdd: (kind: string, name: string) => Promise<Added>;
  onRemove: (provider: string) => unknown;
}

/**
 * The providers that are set up, one to a line: which has a key, and where its requests go. One is added, or
 * changed, in a window of its own. The provider new 分身 use is always listed, so a missing key shows.
 */
function KeyList({ view, onSave, onSaveKey, onAdd, onRemove }: KeyListProps) {
  const { settings } = view;
  /** The window that is open: for the provider with this id, or `new` to add one. */
  const [editing, setEditing] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const statusOf = (provider: Provider) => view.keys[provider.id] ?? NO_KEY;
  const listed = setUp(view);
  /** Take a provider out of the list: its saved key, its address and its name. A key from the environment is not ours to remove. */
  const remove = async (provider: Provider) => {
    setRemoving(null);
    await onRemove(provider.id);
  };
  const toggle = (provider: Provider, on: boolean) => {
    const current = settings.disabled_providers ?? [];
    const next = on ? current.filter((id) => id !== provider.id) : current.includes(provider.id) ? current : [...current, provider.id];
    void onSave({ ...settings, disabled_providers: next });
  };
  return (
    <section>
      <div className="section-head">
        <h2>
          宗门
          <Info>
            宗门是模型供应商。密钥存进系统的凭据管理器（Windows 凭据管理器、macOS 钥匙串、Linux 的 Secret Service），只显示开头和结尾；凭据管理器用不了时，才退回 {view.home} 目录里的明文文件。
          </Info>
        </h2>
        <span className="spacer" />
        <button type="button" className="button primary compact" title="添一个宗门（模型供应商），填上它的密钥；同一种可以加好几个" onClick={() => setEditing("new")}>
          <svg aria-hidden="true" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
            <path d="M8 3v10M3 8h10" />
          </svg>
          新增宗门
        </button>
      </div>
      <ul className="key-list">
        {listed.map((provider) => {
          const status = statusOf(provider);
          const cli = cliOf(provider);
          const isDefault = provider.id === settings.provider;
          const enabled = isDefault || !(settings.disabled_providers ?? []).includes(provider.id);
          const address = settings.base_urls[provider.id];
          const removable = status.source === "saved" || !!address || provider.id !== provider.kind || !!settings.provider_names[provider.id];
          // Where its requests go, when that can be set: the first Anthropic provider's address comes from the environment, and local CLIs have no HTTP URL.
          const shown = provider.id === "anthropic" || cli ? "" : address || (provider.base_url ? "默认地址" : "未填地址");
          return (
            <li key={provider.id} className={enabled ? "" : "off"}>
              <div className="key-who">
                <strong>{provider.name}</strong>
                {isDefault && (
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
                {status.source === "cli" && (
                  <>
                    本机 CLI <span className="mono">{status.preview}</span>
                  </>
                )}
                {status.source === "none" && (cli ? `未装 ${CLI_INFO[cli].bin}` : "未填密钥")}
              </div>
              {shown && (
                <div className="key-where mono" title={address || provider.base_url || undefined}>
                  {shown}
                </div>
              )}
              <span className="key-acts">
                  <label
                    className="mcp-switch"
                    title={
                      isDefault
                        ? "当前默认宗门不能直接停用；先在下面「默认分身」里换一家再停用"
                        : enabled
                          ? `停用 ${provider.name}：保留密钥或 CLI，不在心法菜单中列出`
                          : `启用 ${provider.name}`
                    }
                  >
                    <input type="checkbox" role="switch" checked={enabled} disabled={isDefault} onChange={(event) => toggle(provider, event.target.checked)} />
                    <span className="track" aria-hidden="true" />
                    <span className="sr-only">
                      {enabled ? "停用" : "启用"} {provider.name}
                    </span>
                  </label>
                  <button type="button" className="icon-button" title={`改 ${provider.name} 的密钥或地址`} onClick={() => setEditing(provider.id)}>
                    <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M10.6 2.9 13.1 5.4 5.6 12.9 2.6 13.4 3.1 10.4Z" />
                      <path d="m9.2 4.3 2.5 2.5" />
                    </svg>
                    <span className="sr-only">改 {provider.name}</span>
                  </button>
                  <span className="bubble-anchor">
                    <button
                      type="button"
                      className="icon-button danger"
                      disabled={!removable}
                      aria-expanded={removing === provider.id}
                      title={removable ? `删掉 ${provider.name}：存着的密钥、地址和名字` : status.source === "env" ? "密钥来自环境变量，这里删不了" : "没有可删的"}
                      onClick={() => setRemoving(removing === provider.id ? null : provider.id)}
                    >
                      <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                        <path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.1 4.3l.6 8.9h6.6l.6-8.9M6.6 6.8v4M9.4 6.8v4" />
                      </svg>
                      <span className="sr-only">删除 {provider.name}</span>
                    </button>
                    {removing === provider.id && (
                      <ConfirmBubble label={`删掉 ${provider.name}`} confirm="确认删除" onConfirm={() => void remove(provider)} onCancel={() => setRemoving(null)}>
                        <p>{provider.name} 存着的密钥、地址和名字都会删掉。</p>
                      </ConfirmBubble>
                    )}
                  </span>
                </span>
            </li>
          );
        })}
      </ul>
      {editing && (
        <KeyDialog
          provider={editing === "new" ? null : (view.providers.find((provider) => provider.id === editing) ?? null)}
          view={view}
          onSave={onSave}
          onSaveKey={onSaveKey}
          onAdd={onAdd}
          onClose={() => setEditing(null)}
        />
      )}
    </section>
  );
}

/** A kept grant as the user reads it: a command's opening words, or an MCP server's tool. */
function grantLabel(grant: string): string {
  const borrowed = grant.startsWith("mcp:") ? mcpTool(grant.slice("mcp:".length)) : null;
  if (grant === "search") return "向天机阁打探（联网搜索）";
  if (grant.startsWith("web:")) return `网站 ${grant.slice("web:".length)}`;
  return borrowed ? `法器 ${borrowed.server} 的 ${borrowed.tool}` : grant.replace(/^command:/, "");
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
    name: "心法",
    hint: "各个宗门和它们的密钥，默认分身修哪门心法、推演多深，前情留多长",
    icon: <path d="M4.5 13.5A1.5 1.5 0 0 1 3 12V4a1.5 1.5 0 0 1 1.5-1.5H13v8.5H4.5A1.5 1.5 0 0 0 3 12.5M13 11v2.5H4.5M6 5.5h4" />,
  },
  { id: "rules", name: "禁制", hint: "分身不问就能做什么，结界（沙箱），护法（模型审核），发出去的通行符（长期许可）", icon: <path d="M8 1.8 13 3.6v4.2c0 3-2.1 5.3-5 6.4-2.9-1.1-5-3.4-5-6.4V3.6z" /> },
  { id: "tools", name: "神通", hint: "天机阁（联网搜索）、丹青（生成图片）、文房（文档运行环境）、语言服务（代码导航）。秘籍和身外法器（MCP 服务）在侧栏的「秘籍」「法器」里", icon: <path d="M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3" /> },
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

/**
 * Which image model the 分身 paints with: the one chosen, while its provider has a key; else the first that has one.
 * What is in force is said under the choice, and when nothing can paint, what puts it right.
 */
function PainterSection({ view, onSave }: { view: SettingsView; onSave: (settings: Settings) => unknown }) {
  const { settings } = view;
  const keyed = (provider: string) => (view.keys[provider]?.source ?? "none") !== "none";
  const providerName = (provider: string) => view.kinds.find((kind) => kind.id === provider)?.name ?? provider;
  const name = (id: string | null) => view.painters.find(([painter]) => painter === id)?.[1] ?? "";
  const options = [
    { value: "", text: "自动：哪家有密钥就请哪家" },
    ...view.painters.map(([id, painter, provider]) => ({ value: id, text: keyed(provider) ? painter : `${painter}（还没有 ${providerName(provider)} 的密钥）` })),
  ];
  return (
    <section>
      <h2>丹青（生成图片）</h2>
      <div className="field">
        <div className="label-line">
          <label htmlFor="painter">请哪家作画</label>
          <Info>
            照片、写实渲染、插画这类代码画不出来的，分身请这里的画师（图像模型）作画，用的是那家宗门的密钥和接口地址：OpenAI 的 GPT Image，或者智谱的 GLM-Image、CogView。每张画都先请示本尊。图表、流程图、图标、海报排版这类，分身照「画图」秘籍用代码画，不走这里。
          </Info>
        </div>
        <div className="inline model-row">
          <Combo id="painter" value={settings.painter} options={options} editable={false} onSave={(painter) => onSave({ ...settings, painter })} />
        </div>
        {view.painter_ready === null ? (
          <p className="hint missing">还没有 OpenAI 或 GLM（智谱）的密钥，分身现在只能用代码画图。到「心法」那一页给其中一家添上密钥，就能请画师作画。</p>
        ) : settings.painter && settings.painter !== view.painter_ready ? (
          <p className="hint">{name(settings.painter)}还没有密钥，眼下请 {name(view.painter_ready)} 作画。</p>
        ) : (
          <p className="hint">眼下请 {name(view.painter_ready)} 作画。</p>
        )}
      </div>
    </section>
  );
}

interface Props {
  view: SettingsView;
  /** The models a provider serves, asked from the provider itself. */
  listModels: (provider: string) => Promise<[string, string][]>;
  onRefresh: () => void;
  onRevoke: (workspace: string, grant: string) => void;
  onSave: (settings: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onAddProvider: (kind: string, name: string) => Promise<Added>;
  onRemoveProvider: (provider: string) => unknown;
}

export function SettingsPage({ view, onSave, onSaveKey, onAddProvider, onRemoveProvider, onRevoke, onRefresh, listModels }: Props) {
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
      <header className="settings-tabs" data-tauri-drag-region>
        <div className="settings-tabs-inner" data-tauri-drag-region>
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
        <KeyList view={view} onSave={onSave} onSaveKey={onSaveKey} onAdd={onAddProvider} onRemove={onRemoveProvider} />
        <section>
          <h2>
            默认分身
            <Info>新化出的分身照这里的来。已经化出的不受影响，要换心法在它输入框下方换。</Info>
          </h2>
          <div className="field">
            <div className="label-line">
              <label htmlFor="default-provider">宗门（模型供应商）</label>
              <Info>只列上面已有的宗门。要用别家，先点「新增宗门」把它添进来。</Info>
            </div>
            <div className="inline model-row">
              {/* Only the 宗门 in the list above: one that is not set up could not answer. */}
              <Combo
                id="default-provider"
                value={settings.provider}
                options={enabledProviders(view).map((provider) => ({ value: provider.id, text: provider.name, note: view.keys[provider.id]?.source === "none" ? (cliOf(provider) ? "未装 CLI" : "未填密钥") : "" }))}
                editable={false}
                onSave={(id) => onSave({ ...settings, provider: id, model: view.providers.find((provider) => provider.id === id)?.models[0]?.[0] ?? "" })}
              />
            </div>
          </div>
          <div className="field">
            <div className="label-line">
              <label htmlFor="default-model">心法（模型）</label>
              <Info>可以从列表里选，也可以直接填模型 ID。列表是向这个宗门问来的；问不到时，用内置的几门。</Info>
            </div>
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
                title={keyed === "none" ? "先给这个宗门填上密钥，才问得到它的心法" : asking ? "正在问……" : `再向 ${chosen?.name ?? settings.provider} 问一遍它有哪些心法`}
                onClick={() => setAsked((times) => times + 1)}
              >
                <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M13.2 8a5.2 5.2 0 0 1-9.3 3.2M2.8 8a5.2 5.2 0 0 1 9.3-3.2" />
                  <path d="M12.6 2.2v2.9H9.7M3.4 13.8v-2.9h2.9" />
                </svg>
                <span className="sr-only">刷新心法列表</span>
              </button>
            </div>
            {/* How asking the provider for its models stands: said in a line, since it is what the list above holds. */}
            {(asking || served.models.length > 0 || served.why || keyed === "none") && (
              <p className="hint">
                {asking
                  ? `正在向 ${chosen?.name ?? settings.provider} 问它有哪些心法……`
                  : served.models.length > 0
                    ? `已向 ${chosen?.name ?? settings.provider} 问到它提供的 ${served.models.length} 门心法。`
                    : served.why
                      ? `没能向 ${chosen?.name ?? settings.provider} 问到心法列表（${served.why}），先用内置的几门，也可以直接填模型 ID。`
                      : "这个宗门还没填密钥，问不到它的心法列表：先用内置的几门，或直接填模型 ID。"}
              </p>
            )}
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
              <div className="label-line">
                <label htmlFor="context-window-set">这门心法一次最多能装多少（token）</label>
                <Info>这是心法自己的硬指标，不是想让对话多长：填得比实际大，请求会被宗门拒掉。留空不改。</Info>
              </div>
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
            </div>
          )}
          <fieldset className="field">
            <legend>
              境界
              <Info>境界越高，分身每一步推演得越深，也越慢、越费灵石（额度）。这是新分身的默认，下一次请求起生效；单个分身可以在输入框下方另换境界，换过的就不再跟着这里。</Info>
            </legend>
            <div className="realms">
              {view.efforts.map((effort) => (
                <label className="realm" key={effort}>
                  <input type="radio" name="realm" checked={settings.effort === effort} onChange={() => onSave({ ...settings, effort })} />
                  <span className="name">{EFFORT_NAMES[effort] ?? effort}</span>
                </label>
              ))}
            </div>
            {/* What this provider makes of the realms, where it does not take all five: it changes what a pick does. */}
            {note && <p className="hint">{note}</p>}
          </fieldset>
        </section>

        <section>
          <h2>灵石</h2>
          <div className="field">
            <div className="label-line">
              <span className="field-label" id="context-limit-label">
                前情上限
              </span>
              <Info>
                每次请求都要把整段对话发给模型，对话越长越费灵石。对话到上限，分身会把前情凝成一段摘要，之后只凭摘要接着做。「自动」按每门心法自己的窗口，用到它的八成：窗口大的心法前情留得长，也更费灵石。「自己定」是一个数（token，最低
                20000），对所有心法都用它；心法装不下这么多时，按它窗口的八成算。
              </Info>
            </div>
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
            {/* What 自动 comes to for the model in use: the figure that the field would otherwise show. */}
            {settings.context_auto && (
              <p className="hint">
                按每门心法自己的窗口，用到它的八成，{settings.model} 眼下是 {shortCount(ceiling)}。
              </p>
            )}
          </div>
          {/* Said here only when it matters: the model cannot hold as much as the limit asks for. */}
          {!settings.context_auto && settings.context_limit > ceiling && (
            <p className="hint" id="context-capped">
              {settings.model} 的上下文窗口只有 {shortCount(view.context_window.tokens)}，前情上限最多用到它的八成，所以眼下实际是 {shortCount(ceiling)}。
            </p>
          )}
        </section>

        {view.keys.antigravity?.source === "cli" && <ModelUsageSection hasAgy />}
          </>
        )}
        {tab === "rules" && (
          <>
        <section>
          <h2>
            禁制
            <Info>已经化出的分身，在输入框下方或按 Shift+Tab 随时换禁制。无论哪种禁制，碰洞府之外的东西、改 .git 里的文件都要请示。</Info>
          </h2>
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
        </section>

        <section>
          <h2>结界（沙箱）</h2>
          <div className="label-line">
            <label className="check">
              <input type="checkbox" checked={settings.sandbox && !!view.sandbox_support.Ok} disabled={!view.sandbox_support.Ok} onChange={(event) => onSave({ ...settings, sandbox: event.target.checked })} />
              <span>给洞府布下结界，把分身运行的命令关在里面</span>
            </label>
            <Info>
              <p>命令只能往洞府和临时目录里写；读文件、联网不受限。连同它启动的一切，比如构建脚本和测试，都改不了洞府之外的文件。</p>
              <p>禁制照旧：该请示的命令还是请示。命令确实要往别处写时，分身会请示出结界跑一次，每次都要本尊点头。下一条命令起生效。</p>
              {view.sandbox_support.Ok && <p>用的是系统自带的 {view.sandbox_support.Ok}。</p>}
            </Info>
          </div>
          {/* Why the box cannot be ticked, where it cannot: that is how things stand, so it is said in the open. */}
          {!view.sandbox_support.Ok && (
            <p className="hint" title={view.sandbox_support.Err}>
              这个系统上还布不了结界，眼下只有 Linux 和 macOS 可以。
            </p>
          )}
          {/* Nothing to add directories to where nothing is confined. */}
          <div className="field" hidden={!view.sandbox_support.Ok}>
            <div className="label-line">
              <label htmlFor="sandbox-paths">另外准许写入的目录（一行一个）</label>
              <Info>结界之外还准命令写的地方，比如包管理器的缓存目录。Git 仓库自己的 .git 目录不用填，已经算在内。</Info>
            </div>
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
          </div>
        </section>

        <section>
          <h2>护法（模型审核）</h2>
          <div className="label-line">
            <label className="check">
              <input type="checkbox" checked={settings.guardian} onChange={(event) => onSave({ ...settings, guardian: event.target.checked })} />
              <span>放手施为时，请一位护法先审拿不准的命令</span>
            </label>
            <Info>
              <p>护法是另一个模型：它放行的直接跑，它不放行的才请本尊定夺。只管放手施为下规则不放行的命令；别的禁制、改洞府之外的文件、上网、身外法器照旧请示，全权托付不经护法。</p>
              <p>护法只看得到本尊说过的话、分身动手前说的打算和命令本身，看不到文件和命令输出里的字。它会看走眼，也会被骗，不是保险；每审一次花一两千 token 的灵石，慢一两秒。</p>
            </Info>
          </div>
          {settings.guardian && (
            <div className="field">
              <div className="label-line">
                <label htmlFor="guardian-model">护法的心法（{chosen?.name ?? settings.provider}）</label>
                <Info>挑一门又快又便宜的。护法和分身用同一个宗门，这里设的是默认宗门的；别的宗门没设时，用分身自己的心法。</Info>
              </div>
              <div className="inline model-row">
                <Combo
                  id="guardian-model"
                  value={settings.guardian_models[settings.provider] ?? ""}
                  options={suggestions.map(([id, name]) => ({ value: id, text: id, note: name === id ? "" : name }))}
                  editable
                  mono
                  placeholder="留空就用分身自己的心法"
                  onSave={(model) => {
                    const { [settings.provider]: _, ...others } = settings.guardian_models;
                    onSave({ ...settings, guardian_models: model ? { ...others, [settings.provider]: model } : others });
                  }}
                />
              </div>
            </div>
          )}
          <div className="field">
            <div className="label-line">
              <label htmlFor="approval-timeout">放手施为下，请示等多久（秒）</label>
              <Info>过了这个时间没人定夺，就自动驳回，分身绕开那一步接着做，并在回报里说明。填 0 就一直等。别的禁制下一直等。</Info>
            </div>
            <input id="approval-timeout" className="input mono short" type="number" min={0} step={5} {...timeout} />
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
            <div className="label-line">
              <label htmlFor="search-engine">向哪家打探</label>
              <Info>
                搜索走智谱的搜索接口，用的就是 GLM（智谱）那把密钥，换哪家心法都能搜；每次搜索前要请示，按次花灵石。GLM 的接口地址换成国际站 z.ai 时，只有那边自带的一种搜索，这里的选择不起作用。没有 GLM 密钥时，用 Anthropic 或 OpenAI 心法的分身改由那一家自带的搜索来搜：每次多发一次模型请求，搜索费和这次请求的灵石都算在那一家，同样先请示。
              </Info>
            </div>
            <div className="inline model-row">
              <Combo
                id="search-engine"
                value={settings.search_engine}
                options={view.search_engines.map(([id, what]) => ({ value: id, text: what }))}
                editable={false}
                onSave={(engine) => onSave({ ...settings, search_engine: engine })}
              />
            </div>
            {/* Searching does not work yet: said in the open, with what puts it right. */}
            {!view.search_ready && (
              <p className="hint missing">还没有 GLM 的密钥：用 Anthropic 或 OpenAI 心法的分身改由那一家自带的搜索来搜，用别家心法的分身搜不了。到「心法」那一页给 GLM 添上密钥，或者到侧栏的「法器」里启用 Exa，它不用密钥。</p>
            )}
          </div>
        </section>
        <PainterSection view={view} onSave={onSave} />
        <RuntimeSection />
        <LanguageServersSection />
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

/** Antigravity subscription quota overview in Settings. */
function ModelUsageSection({ hasAgy }: { hasAgy: boolean }) {
  const agyQuota = useAntigravityQuota(hasAgy);

  if (!hasAgy) return null;

  return (
    <section>
      <div className="usage-head">
        <h2>
          订阅额度
          <Info>本机装有 agy 时自动查询 Antigravity 5 小时与周限额。</Info>
        </h2>
        <div className="usage-actions">
          <button
            type="button"
            className="button text small"
            disabled={agyQuota.loading}
            onClick={() => agyQuota.refresh(true)}
          >
            {agyQuota.loading ? "查询额度中…" : "刷新"}
          </button>
        </div>
      </div>
      <div className="quota-panel">
        <div className="quota-panel-head">
          <span className="field-label">Antigravity 订阅额度</span>
          {agyQuota.error && <span className="file-note failed">{agyQuota.error}</span>}
          {!agyQuota.quota && agyQuota.loading && <span className="muted">正在通过 agy /usage 查询…</span>}
        </div>
        {agyQuota.quota && (
          <div className="quota-groups">
            {agyQuota.quota.groups.map((group) => (
              <div className="quota-group" key={group.name}>
                <div className="quota-group-title" title={group.description ?? undefined}>
                  {group.name}
                </div>
                <div className="quota-buckets">
                  {group.buckets.map((bucket) => {
                    const resetIn = formatResetIn(bucket.reset_time, agyQuota.nowMs);
                    const tone = bucket.remaining_percent < 20 ? "low" : bucket.remaining_percent < 50 ? "mid" : "high";
                    return (
                      <div className="quota-bucket" key={bucket.id || bucket.window}>
                        <div className="quota-bucket-row">
                          <span className="quota-bucket-name">{quotaWindowLabel(bucket.window, bucket.name)}</span>
                          <span className={`quota-bucket-pct ${tone}`}>{bucket.remaining_percent}%</span>
                        </div>
                        <div className="quota-bar" aria-hidden="true">
                          <span className={`quota-fill ${tone}`} style={{ width: `${bucket.remaining_percent}%` }} />
                        </div>
                        <div className="quota-bucket-meta">{resetIn ? `${resetIn} 后刷新` : "即将刷新"}</div>
                      </div>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

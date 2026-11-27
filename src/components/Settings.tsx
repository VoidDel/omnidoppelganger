import "../styles/pages.css";
import { Suspense, useCallback, useEffect, useRef, useState } from "react";
import { LOCALE_OPTIONS, resolveLocale, useI18n } from "../i18n";
import { api } from "../lib/api";
import { effortName, effortNote } from "../lib/efforts";
import { mcpTool, shortCount } from "../lib/fold";
import { modesFor } from "../lib/modes";
import { formatResetIn, quotaWindowLabel, useAntigravityQuota, useKimiQuota } from "../lib/modelUsage";
import { CLI_INFO, cliOf, enabledProviders, setUp } from "../lib/providers";
import type { KeyStatus, KimiOAuthStart, KimiRegion, Provider, RawListedModel, Settings, SettingsView, UpdateStatus } from "../lib/types";
import { Combo } from "./Combo";
import { ConfirmBubble } from "./ConfirmBubble";
import { lazyWithRetry } from "./Guard";
import { Info } from "./Hint";
import { Icon } from "./Icon";
import { LanguageServersSection } from "./LanguageServers";
import { normalizeListedModel } from "./Pickers";
import { RuntimeSection } from "./Runtime";
import { UsagePricingSection } from "./UsagePricingSection";

const SkillsPage = lazyWithRetry(() => import("./Skills").then((m) => ({ default: m.SkillsPage })));
const McpPage = lazyWithRetry(() => import("./Mcp").then((m) => ({ default: m.McpPage })));

/**
 * A text field that tracks whether the user has edited it (`dirty`), ignores unrelated concurrent external
 * re-renders while `dirty`, and commits on blur or Enter.
 */
function useDraft(value: string, save: (value: string) => void) {
  const [draft, setDraft] = useState(value);
  const [dirty, setDirty] = useState(false);
  const prevPropRef = useRef(value);

  useEffect(() => {
    if (value !== prevPropRef.current) {
      prevPropRef.current = value;
      setDraft(value);
      setDirty(false);
    } else if (!dirty) {
      setDraft(value);
    }
  }, [value, dirty]);

  const commit = () => {
    const trimmed = draft.trim();
    setDirty(false);
    if (trimmed !== value) {
      save(trimmed);
    } else {
      setDraft(value);
    }
  };

  return {
    value: draft,
    onChange: (event: React.ChangeEvent<HTMLInputElement>) => {
      setDraft(event.target.value);
      setDirty(true);
    },
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
      {status.source === "oauth" && <span className="muted">已通过 Kimi OAuth 账号登录（自动刷新令牌）</span>}
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
  onSavePatch: (patch: Partial<Settings>, baseOverride?: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onAdd: (kind: string, name: string) => Promise<Added>;
  onRefresh: () => void;
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
function KeyDialog({ provider, view, onSavePatch, onSaveKey, onAdd, onRefresh, onClose }: KeyDialogProps) {
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
  const [kimiRegion, setKimiRegion] = useState<KimiRegion>(() =>
    (provider ? (settings.base_urls[provider.id] ?? "") : "").includes(".ai") ? "global" : "mainland-cn",
  );
  const [oauthFlow, setOauthFlow] = useState<KimiOAuthStart | null>(null);
  const [oauthBusy, setOauthBusy] = useState(false);
  const [oauthMsg, setOauthMsg] = useState<string | null>(null);
  const activeFlowRef = useRef<string | null>(null);
  activeFlowRef.current = oauthFlow?.flow_id ?? null;

  useEffect(() => {
    return () => {
      if (activeFlowRef.current) {
        void api.kimiOAuthCancel(activeFlowRef.current);
      }
    };
  }, []);

  useEffect(() => {
    if (!oauthFlow) return;
    let cancelled = false;
    let timer: number | undefined;
    const schedule = (delaySec: number) => {
      timer = window.setTimeout(async () => {
        if (cancelled) return;
        try {
          const res = await api.kimiOAuthPoll(oauthFlow.flow_id);
          if (cancelled) return;
          if (res.authenticated) {
            setOauthFlow(null);
            setOauthMsg(res.nickname ? `已登录 Kimi 账号：${res.nickname}` : "Kimi OAuth 登录成功");
            onRefresh();
            onClose();
            return;
          }
          if (res.ended) {
            setOauthFlow(null);
            setWrong(
              res.ended === "expired"
                ? "授权码已过期，请重新点击登录。"
                : res.ended === "denied"
                  ? "已在浏览器中取消 Kimi 授权。"
                  : null,
            );
            return;
          }
          schedule(Math.max(res.interval || oauthFlow.interval || 5, 2));
        } catch (err) {
          if (!cancelled) {
            setOauthFlow(null);
            setWrong(String(err));
          }
        }
      }, delaySec * 1000);
    };
    schedule(Math.max(oauthFlow.interval || 5, 2));
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [oauthFlow, onRefresh, onClose]);

  if (!kind) return null;
  // The provider this window sets: the one changed, or the one adding will make, the first of its kind while that is free.
  const target = provider?.id ?? (firstFree(view, kind.id) ? kind.id : null);
  const status = target ? (view.keys[target] ?? NO_KEY) : NO_KEY;
  // Local CLI backends (`agy`, `codex`, `claude`, `cursor-agent`, `grok`) reuse their own CLI login and need neither an API key nor an HTTP base URL.
  const cli = cliOf(kind);
  const isCli = cli !== null;
  const isKimi = kind.id === "moonshot";
  const isLocalGateway = kind.id === "magpie" || kind.id === "ollama";
  const takesAddress = target !== "anthropic" && !isCli;

  const startKimiOAuth = async () => {
    setWrong(null);
    setOauthMsg(null);
    setOauthBusy(true);
    try {
      const started = await api.kimiOAuthStart(kimiRegion, provider?.id ?? null, name.trim() || null);
      setOauthFlow(started);
      void api.openExternal(started.verification_uri).catch(() => {
        window.open(started.verification_uri, "_blank", "noopener,noreferrer");
      });
    } catch (err) {
      setWrong(String(err));
    } finally {
      setOauthBusy(false);
    }
  };

  const cancelKimiOAuth = async () => {
    const id = oauthFlow?.flow_id ?? null;
    setOauthFlow(null);
    if (id) {
      await api.kimiOAuthCancel(id).catch(() => {});
    }
  };

  const logoutKimiOAuth = async () => {
    if (!target) return;
    setOauthBusy(true);
    setWrong(null);
    try {
      await api.kimiOAuthLogout(target);
      onRefresh();
    } catch (err) {
      setWrong(String(err));
    } finally {
      setOauthBusy(false);
    }
  };
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
    if (effectiveUrl !== (base.base_urls[id] ?? "") || names !== base.provider_names) {
      await onSavePatch({ base_urls: urls, provider_names: names }, id === provider?.id ? undefined : base);
    }
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
                setAddress("");
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
        {isKimi && (
          <div className="field">
            <div className="label-line">
              <label htmlFor="kimi-oauth-region">Kimi 账号 OAuth 登录（免填 API 密钥，支持自动刷新与订阅额度）</label>
            </div>
            <div className="inline">
              <Combo
                id="kimi-oauth-region"
                value={kimiRegion}
                options={[
                  { value: "mainland-cn", text: "中国大陆站", note: "kimi.com" },
                  { value: "global", text: "国际站", note: "kimi.ai" },
                ]}
                editable={false}
                onSave={(v) => setKimiRegion(v === "global" ? "global" : "mainland-cn")}
              />
              {!oauthFlow ? (
                <button
                  type="button"
                  className="button primary compact"
                  disabled={oauthBusy || busy}
                  onClick={() => void startKimiOAuth()}
                >
                  {oauthBusy ? "请求授权中…" : status.source === "oauth" ? "重新 OAuth 登录" : "OAuth 账号登录"}
                </button>
              ) : (
                <button type="button" className="button text compact" onClick={() => void cancelKimiOAuth()}>
                  取消授权
                </button>
              )}
              {status.source === "oauth" && !oauthFlow && (
                <button type="button" className="button text compact" disabled={oauthBusy} onClick={() => void logoutKimiOAuth()}>
                  退出登录
                </button>
              )}
            </div>
            {oauthFlow && (
              <div className="hint" role="status">
                <span>
                  设备验证码：<strong className="mono">{oauthFlow.user_code}</strong>{" "}
                </span>
                <button
                  type="button"
                  className="inline-link"
                  onClick={() =>
                    void api.openExternal(oauthFlow.verification_uri).catch(() => {
                      window.open(oauthFlow.verification_uri, "_blank", "noopener,noreferrer");
                    })
                  }
                >
                  打开浏览器授权
                </button>
                <span className="muted">（等待浏览器确认授权…）</span>
              </div>
            )}
            {oauthMsg && <p className="hint">{oauthMsg}</p>}
          </div>
        )}
        <div className="field">
          {!isCli && (
            <>
              <label htmlFor="key-value">
                {status.source === "saved"
                  ? "换一个密钥（不换就留空）"
                  : isKimi
                    ? "或使用 API 密钥（可选）"
                    : isLocalGateway
                      ? "API 密钥（可选，本机服务默认免密）"
                      : "API 密钥"}
              </label>
              <input
                id="key-value"
                className="input"
                type="password"
                autoComplete="off"
                autoFocus
                placeholder={isLocalGateway ? "留空即可；若开启了鉴权再填写" : "粘贴密钥"}
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
            {kind.other_urls.length > 0 ? (
              <Combo
                id="key-address"
                value={address}
                options={[
                  ...(kind.base_url ? [{ value: kind.base_url, text: kind.base_url, note: "默认" }] : []),
                  ...kind.other_urls.map(([url, what]) => ({ value: url, text: url, note: what })),
                ]}
                editable={true}
                mono
                placeholder={kind.base_url || (kind.protocol === "anthropic" ? "https://example.com/anthropic" : "https://example.com/v1")}
                onSave={(v) => setAddress(v)}
                onDraftChange={(v) => setAddress(v)}
              />
            ) : (
              <input
                id="key-address"
                className="input mono"
                placeholder={kind.base_url || (kind.protocol === "anthropic" ? "https://example.com/anthropic" : "https://example.com/v1")}
                value={address}
                onChange={(event) => setAddress(event.target.value)}
              />
            )}
            {kind.protocol === "anthropic" && !kind.base_url && <p className="hint">填到 /v1 之前为止，请求会发到「地址/v1/messages」，和 ANTHROPIC_BASE_URL 的写法一样。</p>}
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
  onSavePatch: (patch: Partial<Settings>, baseOverride?: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onAdd: (kind: string, name: string) => Promise<Added>;
  onRemove: (provider: string) => unknown;
  onRefresh: () => void;
}

/**
 * The providers that are set up, one to a line: which has a key, and where its requests go. One is added, or
 * changed, in a window of its own. The provider new 分身 use is always listed, so a missing key shows.
 */
function KeyList({ view, onSavePatch, onSaveKey, onAdd, onRemove, onRefresh }: KeyListProps) {
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
    void onSavePatch({ disabled_providers: next });
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
          <Icon name="plus-lg" size={13} />
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
          const removable = status.source === "saved" || status.source === "oauth" || !!address || provider.id !== provider.kind || !!settings.provider_names[provider.id];
          // Where its requests go, when that can be set: the first Anthropic provider's address comes from the environment, and local CLIs have no HTTP URL.
          const presetLabel = address
            ? address === provider.base_url
              ? "默认地址"
              : (provider.other_urls.find(([u]) => u === address)?.[1] ?? address)
            : provider.base_url
              ? "默认地址"
              : "未填地址";
          const shown = provider.id === "anthropic" || cli ? "" : presetLabel;
          const shownIsUrl = shown.startsWith("http://") || shown.startsWith("https://");
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
                {status.source === "oauth" && (
                  <>
                    OAuth{status.preview ? <> · <span className="mono">{status.preview}</span></> : ""}
                  </>
                )}
                {status.source === "none" && (cli ? `未装 ${CLI_INFO[cli].bin}` : "未填密钥")}
              </div>
              {shown && (
                <div className={`key-where ${shownIsUrl ? "mono" : ""}`} title={address || provider.base_url || undefined}>
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
                    <Icon name="pencil" size={15} />
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
                      <Icon name="trash3" size={15} />
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
          onSavePatch={onSavePatch}
          onSaveKey={onSaveKey}
          onAdd={onAdd}
          onRefresh={onRefresh}
          onClose={() => setEditing(null)}
        />
      )}
    </section>
  );
}

/** A kept grant as the user reads it: a command's opening words, or an MCP server's tool. */
function grantLabel(grant: string): string {
  const borrowed = grant.startsWith("mcp:") ? mcpTool(grant.slice("mcp:".length)) : null;
  if (grant === "project-trust") return "信任此洞府的项目规矩与秘籍（AGENTS.md / .odg）";
  if (grant === "search") return "向天机阁打探（联网搜索）";
  if (grant.startsWith("web:")) return `网站 ${grant.slice("web:".length)}`;
  if (grant.startsWith("mcp-resource:")) return `读取法器 ${grant.slice("mcp-resource:".length)} 的资源`;
  if (grant.startsWith("lsp:")) return `启动语言服务 ${grant.slice("lsp:".length)}`;
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
export type SettingsTab = "model" | "ledger" | "skills" | "mcp" | "rules" | "tools" | "look" | "about";

/**
 * Which image model the 分身 paints with: the one chosen, while its provider has a key; else the first that has one.
 * What is in force is said under the choice, and when nothing can paint, what puts it right.
 */
function PainterSection({ view, onSavePatch }: { view: SettingsView; onSavePatch: (patch: Partial<Settings>) => unknown }) {
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
          <Combo id="painter" value={settings.painter} options={options} editable={false} onSave={(painter) => onSavePatch({ painter })} />
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

function UpdateSection({ version }: { version?: string }) {
  const { locale, t } = useI18n();
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [linkingCli, setLinkingCli] = useState(false);
  const [cliLinked, setCliLinked] = useState<string | null>(null);
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const shownVersion = status?.current_version ?? version ?? __APP_VERSION__;
  const isWindowsClient = typeof navigator !== "undefined" && /Win/i.test(navigator.userAgent);

  const checkNow = () => {
    setChecking(true);
    setError(null);
    api
      .checkAppUpdate()
      .then(
        (res) => setStatus(res),
        (err) => setError(String(err)),
      )
      .finally(() => setChecking(false));
  };

  const installNow = () => {
    setInstalling(true);
    setError(null);
    api
      .installAppUpdate()
      .then(
        (installed) => {
          if (!installed && status?.html_url) {
            void api.openExternal(status.html_url);
          }
        },
        (err) => setError(String(err)),
      )
      .finally(() => setInstalling(false));
  };

  const linkCli = () => {
    setLinkingCli(true);
    setError(null);
    setCliLinked(null);
    api
      .installCliLink()
      .then(
        (linkedPath) => setCliLinked(linkedPath),
        (err) => setError(String(err)),
      )
      .finally(() => setLinkingCli(false));
  };

  return (
    <section>
      <div className="usage-head">
        <h2>{t("settings.updateHeading")}</h2>
        <div className="usage-actions">
          {!isWindowsClient && (
            <button type="button" className="button outline small" disabled={linkingCli} onClick={linkCli}>
              {t("settings.cliLinkBtn")}
            </button>
          )}
          <button type="button" className="button outline small" disabled={checking || installing} onClick={checkNow}>
            {checking ? t("settings.updateChecking") : t("settings.updateCheckBtn")}
          </button>
        </div>
      </div>
      <div className="provider-card">
        <div className="who">
          <span>
            {t("settings.updateCurrent")} <strong className="mono">v{shownVersion}</strong>
            {!status && <span className="muted">{t("settings.updateCheckHint")}</span>}
          </span>
          {error && <span className="hint failed">{error}</span>}
          {cliLinked && <span className="hint">{t("settings.cliLinkedNote", { path: cliLinked })}</span>}
          {status && !status.has_update && (
            <span className="muted">{t("settings.updateUpToDate", { version: status.latest_version })}</span>
          )}
          {status?.has_update && (
            <span className="muted">
              {t("settings.updateAvailable")} <strong className="mono">v{status.latest_version}</strong>
              {status.published_at
                ? ` (${new Date(status.published_at).toLocaleDateString(locale === "en" ? "en-US" : "zh-CN")})`
                : ""}
            </span>
          )}
        </div>
        {status?.has_update && (
          <>
            {status.notes && (
              <pre className="mono muted" style={{ whiteSpace: "pre-wrap", fontSize: "12px", margin: "8px 0", maxHeight: "180px", overflowY: "auto" }}>
                {status.notes}
              </pre>
            )}
            <div className="actions" style={{ marginTop: "6px", flexWrap: "wrap" }}>
              <button type="button" className="button primary small" disabled={installing} onClick={installNow}>
                {installing
                  ? t("settings.updateInstalling")
                  : t("settings.updateInstallBtn", { version: status.latest_version })}
              </button>
              <button type="button" className="button outline small" onClick={() => void api.openExternal(status.html_url)}>
                {t("settings.updateDownloadBtn", { version: status.latest_version })}
              </button>
            </div>
          </>
        )}
      </div>
    </section>
  );
}

const REPO_URL = "https://github.com/VoidDel/omnidoppelganger";

function HelpSection() {
  const { t } = useI18n();
  const links = [
    { label: t("settings.helpGuide"), hint: t("settings.helpGuideHint"), url: `${REPO_URL}/blob/main/doc/user-guide.md` },
    { label: t("settings.helpFaq"), hint: t("settings.helpFaqHint"), url: `${REPO_URL}/blob/main/doc/faq.md` },
    { label: t("settings.helpPrivacy"), hint: t("settings.helpPrivacyHint"), url: `${REPO_URL}/blob/main/doc/privacy.md` },
    { label: t("settings.helpIssues"), hint: t("settings.helpIssuesHint"), url: `${REPO_URL}/issues/new/choose` },
  ];
  return (
    <section>
      <h2>{t("settings.helpHeading")}</h2>
      <div className="provider-card">
        <div className="who">
          <span className="muted">{t("settings.helpLead")}</span>
        </div>
        <div className="actions" style={{ marginTop: "6px", flexWrap: "wrap" }}>
          {links.map((item) => (
            <button
              key={item.url}
              type="button"
              className="button outline small"
              title={item.hint}
              onClick={() => void api.openExternal(item.url)}
            >
              {item.label}
            </button>
          ))}
        </div>
      </div>
    </section>
  );
}

interface Props {
  view: SettingsView;
  /** The models a provider serves, asked from the provider itself. */
  listModels: (provider: string) => Promise<RawListedModel[]>;
  onRefresh: () => void;
  onRevoke: (workspace: string, grant: string) => void;
  onSave: (settings: Settings) => unknown;
  onSaveKey: (provider: string, key: string) => unknown;
  onAddProvider: (kind: string, name: string) => Promise<Added>;
  onRemoveProvider: (provider: string) => unknown;
  onSaveMcp?: (config: string) => Promise<string | null>;
  initialTab?: SettingsTab;
  onTabChange?: (tab: SettingsTab) => void;
}

export function SettingsPage({
  view,
  onSave,
  onSaveKey,
  onAddProvider,
  onRemoveProvider,
  onRevoke,
  onRefresh,
  listModels,
  onSaveMcp,
  initialTab,
  onTabChange,
}: Props) {
  const { locale, setLocale, t } = useI18n();
  const [tab, setTabState] = useState<SettingsTab>(initialTab ?? "model");
  useEffect(() => {
    if (initialTab) setTabState(initialTab);
  }, [initialTab]);
  const setTab = (next: SettingsTab) => {
    setTabState(next);
    onTabChange?.(next);
  };
  const grants = Object.entries(view.grants);
  const { settings } = view;

  // Dirty-field patch tracking: always merge user-modified keys onto the latest `view.settings` snapshot
  // so concurrent external changes (e.g., top-bar model/mode switches) are never reverted by stale drafts.
  const latestSettingsRef = useRef<Settings>(settings);
  const prevSettingsRef = useRef<Settings>(settings);
  const dirtyPatchRef = useRef<Partial<Settings>>({});

  if (prevSettingsRef.current !== settings) {
    const prev = prevSettingsRef.current;
    for (const key of Object.keys(dirtyPatchRef.current) as (keyof Settings)[]) {
      if (
        JSON.stringify(settings[key]) === JSON.stringify(dirtyPatchRef.current[key]) ||
        JSON.stringify(prev[key]) !== JSON.stringify(settings[key])
      ) {
        delete dirtyPatchRef.current[key];
      }
    }
    prevSettingsRef.current = settings;
  }
  latestSettingsRef.current = settings;

  const savePatch = useCallback(
    (patch: Partial<Settings>, baseOverride?: Settings) => {
      dirtyPatchRef.current = { ...dirtyPatchRef.current, ...patch };
      const base = baseOverride ?? latestSettingsRef.current;
      return onSave({ ...base, ...dirtyPatchRef.current });
    },
    [onSave],
  );

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
        (rawModels) => {
          if (!current) return;
          const models: [string, string][] = rawModels.map((entry) => {
            const m = normalizeListedModel(entry);
            return [m.id, m.name];
          });
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
    if (Number.isFinite(seconds) && seconds >= 0) savePatch({ approval_timeout: seconds });
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
    const { [windowKey]: kept, ...others } = latestSettingsRef.current.context_windows;
    const tokens = Number.parseInt(value, 10);
    const next = Number.isFinite(tokens) && tokens > 0 ? tokens : undefined;
    if (next !== kept) savePatch({ context_windows: next ? { ...others, [windowKey]: next } : others });
  };
  const limit = useDraft(String(settings.context_limit), (value) => {
    const tokens = Number.parseInt(value, 10);
    if (Number.isFinite(tokens)) savePatch({ context_limit: tokens });
  });

  const tabs: { id: SettingsTab; name: string; hint: string; icon: string }[] = [
    { id: "model", name: t("settings.tabModel"), hint: t("settings.tabModelHint"), icon: "journal-text" },
    { id: "ledger", name: t("settings.tabLedger"), hint: t("settings.tabLedgerHint"), icon: "bar-chart-fill" },
    { id: "skills", name: t("nav.skills"), hint: t("nav.skillsTitle"), icon: "book" },
    { id: "mcp", name: t("nav.mcp"), hint: t("nav.mcpTitle"), icon: "bell" },
    { id: "rules", name: t("settings.tabRules"), hint: t("settings.tabRulesHint"), icon: "shield-check" },
    { id: "tools", name: t("settings.tabTools"), hint: t("settings.tabToolsHint"), icon: "plug" },
    { id: "look", name: t("settings.tabLook"), hint: t("settings.tabLookHint"), icon: "circle-half" },
    { id: "about", name: t("settings.tabAbout"), hint: t("settings.tabAboutHint"), icon: "info-circle" },
  ];
  const papers: { value: Settings["theme"]; name: string; hint: string; swatches: [string, string][] }[] = [
    { value: "light", name: t("settings.themeLight"), hint: t("settings.themeLightHint"), swatches: [["#FBFCFA", "#1B2A2B"], ["#F6E7A8", "#D9BC55"], ["#B8322A", "#B8322A"]] },
    { value: "dark", name: t("settings.themeDark"), hint: t("settings.themeDarkHint"), swatches: [["#1A2A2C", "#E6EEEA"], ["#ECD88C", "#B8962E"], ["#E0584E", "#E0584E"]] },
    { value: "system", name: t("settings.themeSystem"), hint: t("settings.themeSystemHint"), swatches: [["#FBFCFA", "#1B2A2B"], ["#1A2A2C", "#E6EEEA"]] },
  ];
  const modeOptions = modesFor(locale);
  const currentLocale = resolveLocale(settings.locale ?? locale);
  const localeCards = LOCALE_OPTIONS.map((opt) => ({
    value: opt.value,
    name:
      opt.value === "zh-CN"
        ? t("settings.localeZhCN")
        : opt.value === "zh-plain"
          ? t("settings.localeZhPlain")
          : t("settings.localeEn"),
    hint:
      opt.value === "zh-CN"
        ? t("settings.localeZhCNHint")
        : opt.value === "zh-plain"
          ? t("settings.localeZhPlainHint")
          : t("settings.localeEnHint"),
  }));

  return (
    <div className="settings settings-shell">
      <aside className="settings-nav" aria-label={t("nav.settings")} data-tauri-drag-region>
        <div className="settings-nav-list">
          {tabs.map((item) => (
            <button type="button" className="tab" key={item.id} aria-pressed={tab === item.id} title={item.hint} onClick={() => setTab(item.id)}>
              <Icon name={item.icon} size={15} />
              <span>{item.name}</span>
            </button>
          ))}
        </div>
      </aside>
      <div className="settings-content">
      <div className="settings-inner">
        {tab === "model" && (
          <>
        <KeyList view={view} onSavePatch={savePatch} onSaveKey={onSaveKey} onAdd={onAddProvider} onRemove={onRemoveProvider} onRefresh={onRefresh} />
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
                onSave={(id) => savePatch({ provider: id, model: view.providers.find((provider) => provider.id === id)?.models[0]?.[0] ?? "" })}
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
                onSave={(value) => savePatch({ model: value })}
              />
              <button
                type="button"
                className={`icon-button ${asking ? "turning" : ""}`}
                disabled={asking || keyed === "none"}
                title={keyed === "none" ? "先给这个宗门填上密钥，才问得到它的心法" : asking ? "正在问……" : `再向 ${chosen?.name ?? settings.provider} 问一遍它有哪些心法`}
                onClick={() => setAsked((times) => times + 1)}
              >
                <Icon name="arrow-repeat" size={16} />
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
                  <input type="radio" name="realm" checked={settings.effort === effort} onChange={() => savePatch({ effort })} />
                  <span className="name">{effortName(effort, locale)}</span>
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
                <input type="radio" name="context-limit-kind" checked={settings.context_auto} onChange={() => savePatch({ context_auto: true })} />
                自动
              </label>
              <label className="kind">
                <input type="radio" name="context-limit-kind" checked={!settings.context_auto} onChange={() => savePatch({ context_auto: false })} />
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

          </>
        )}
        {tab === "ledger" && (
          <>
            <UsagePricingSection />
            {(view.keys.antigravity?.source === "cli" ||
              view.providers.some((p) => p.kind === "moonshot" && (view.keys[p.id]?.source ?? "none") !== "none")) && (
              <ModelUsageSection
                hasAgy={view.keys.antigravity?.source === "cli"}
                kimiProviderId={
                  view.providers.find((p) => p.kind === "moonshot" && (view.keys[p.id]?.source ?? "none") !== "none")?.id ?? null
                }
              />
            )}
          </>
        )}
        {tab === "skills" && (
          <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
            <SkillsPage embedded onChanged={onRefresh} />
          </Suspense>
        )}
        {tab === "mcp" && (
          <Suspense fallback={<div className="viewer-placeholder muted">…</div>}>
            <McpPage
              embedded
              config={view.mcp_config}
              servers={view.mcp_servers}
              home={view.home}
              onRefresh={onRefresh}
              onSave={onSaveMcp ?? ((cfg) => api.saveMcpConfig(cfg).then(() => (onRefresh(), null), (e) => String(e)))}
            />
          </Suspense>
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
            {modeOptions.map((option) => (
              <label className="paper" key={option.value}>
                <span className="name">
                  <input type="radio" name="mode" checked={settings.mode === option.value} onChange={() => savePatch({ mode: option.value })} />
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
              <input type="checkbox" checked={settings.sandbox && !!view.sandbox_support.Ok} disabled={!view.sandbox_support.Ok} onChange={(event) => savePatch({ sandbox: event.target.checked })} />
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
                if (paths.join("\n") !== latestSettingsRef.current.sandbox_paths.join("\n")) savePatch({ sandbox_paths: paths });
              }}
            />
          </div>
        </section>

        <section>
          <h2>护法（模型审核）</h2>
          <div className="label-line">
            <label className="check">
              <input type="checkbox" checked={settings.guardian} onChange={(event) => savePatch({ guardian: event.target.checked })} />
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
                    const curProvider = latestSettingsRef.current.provider;
                    const { [curProvider]: _, ...others } = latestSettingsRef.current.guardian_models;
                    savePatch({ guardian_models: model ? { ...others, [curProvider]: model } : others });
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
                onSave={(engine) => savePatch({ search_engine: engine })}
              />
            </div>
            {/* Searching does not work yet: said in the open, with what puts it right. */}
            {!view.search_ready && (
              <p className="hint missing">还没有 GLM 的密钥：用 Anthropic 或 OpenAI 心法的分身改由那一家自带的搜索来搜，用别家心法的分身搜不了。到「心法」那一页给 GLM 添上密钥，或者到侧栏的「法器」里启用 Exa，它不用密钥。</p>
            )}
          </div>
        </section>
        <PainterSection view={view} onSavePatch={savePatch} />
        <RuntimeSection />
        <section>
          <div className="label-line">
            <label className="check">
              <input
                id="auto-update-runtime"
                type="checkbox"
                checked={!!settings.auto_update_runtime}
                onChange={(event) => savePatch({ auto_update_runtime: event.target.checked })}
              />
              <span>{t("settings.autoUpdateRuntime")}</span>
            </label>
          </div>
        </section>
        <LanguageServersSection />
          </>
        )}
        {tab === "look" && (
          <>
        <section>
          <h2>{t("settings.appearance")}</h2>
          <fieldset className="papers">
            <legend>{t("settings.themeLegend")}</legend>
            {papers.map((paper) => (
              <label className="paper" key={paper.value}>
                <span className="name">
                  <input type="radio" name="paper" checked={settings.theme === paper.value} onChange={() => savePatch({ theme: paper.value })} />
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
        <section>
          <h2>{t("settings.languageHeading")}</h2>
          <fieldset className="papers">
            <legend>{t("settings.languageLegend")}</legend>
            {localeCards.map((card) => (
              <label className="paper" key={card.value}>
                <span className="name">
                  <input
                    type="radio"
                    name="locale"
                    checked={currentLocale === card.value}
                    onChange={() => {
                      setLocale(card.value);
                      savePatch({ locale: card.value });
                    }}
                  />
                  {card.name}
                </span>
                <span className="muted">{card.hint}</span>
              </label>
            ))}
          </fieldset>
        </section>
          </>
        )}
        {tab === "about" && (
          <>
        <UpdateSection version={view.version} />
        <HelpSection />
          </>
        )}
      </div>
      </div>
    </div>
  );
}

/** Subscription quota overview in Settings (Antigravity & Kimi Code Coding Plan). */
function ModelUsageSection({ hasAgy, kimiProviderId }: { hasAgy: boolean; kimiProviderId: string | null }) {
  const agyQuota = useAntigravityQuota(hasAgy);
  const kimiQuota = useKimiQuota(kimiProviderId);

  if (!hasAgy && !kimiProviderId) return null;

  const loading = agyQuota.loading || kimiQuota.loading;
  const refreshAll = () => {
    if (hasAgy) agyQuota.refresh(true);
    if (kimiProviderId) kimiQuota.refresh(true);
  };

  return (
    <section>
      <div className="usage-head">
        <h2>
          订阅额度
          <Info>自动查询 Antigravity（agy）与 Kimi Code 订阅（/coding/v1/usages）的 5 小时与周限额。</Info>
        </h2>
        <div className="usage-actions">
          <button type="button" className="button text small" disabled={loading} onClick={refreshAll}>
            {loading ? "查询额度中…" : "刷新"}
          </button>
        </div>
      </div>
      {hasAgy && (
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
      )}
      {kimiProviderId && (
        <div className="quota-panel">
          <div className="quota-panel-head">
            <span className="field-label">Kimi Code 订阅额度</span>
            {kimiQuota.error && <span className="file-note failed">{kimiQuota.error}</span>}
            {!kimiQuota.quota && kimiQuota.loading && <span className="muted">正在查询 Kimi 订阅额度…</span>}
          </div>
          {kimiQuota.quota && (
            <div className="quota-groups">
              {kimiQuota.quota.groups.map((group) => (
                <div className="quota-group" key={group.name}>
                  <div className="quota-group-title" title={group.description ?? undefined}>
                    {group.name}
                  </div>
                  <div className="quota-buckets">
                    {group.buckets.map((bucket) => {
                      const resetIn = formatResetIn(bucket.reset_time, kimiQuota.nowMs);
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
      )}
    </section>
  );
}

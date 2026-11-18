import { useEffect, useState } from "react";
import { MODES } from "../lib/modes";
import { mcpTool } from "../lib/fold";
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

interface ProviderRowProps {
  provider: Provider;
  status: KeyStatus;
  settings: Settings;
  onSave: (settings: Settings) => void;
  onSaveKey: (provider: string, key: string) => void;
  onDeleteKey: (provider: string) => void;
}

function ProviderRow({ provider, status, settings, onSave, onSaveKey, onDeleteKey }: ProviderRowProps) {
  const [key, setKey] = useState("");
  const saved = settings.base_urls[provider.id] ?? "";
  const address = useDraft(saved, (url) => onSave({ ...settings, base_urls: { ...settings.base_urls, [provider.id]: url } }));
  const saveKey = () => {
    if (!key.trim()) return;
    onSaveKey(provider.id, key);
    setKey("");
  };
  const keyId = `key-${provider.id}`;
  const urlId = `url-${provider.id}`;
  return (
    <div className="provider-card">
      <div className="provider">
        <div className="who">
          <strong>{provider.name}</strong>
          <KeyLine provider={provider} status={status} />
        </div>
        {status.source === "saved" && (
          <button type="button" className="button text" onClick={() => onDeleteKey(provider.id)}>
            删除密钥
          </button>
        )}
      </div>
      <div className="provider-fields">
        <div className="field" style={{ flex: "1 1 260px" }}>
          <label htmlFor={keyId}>{status.source === "saved" ? "换一个密钥" : "API 密钥"}</label>
          <div className="inline">
            <input
              id={keyId}
              className="input"
              type="password"
              autoComplete="off"
              placeholder="粘贴密钥"
              value={key}
              onChange={(event) => setKey(event.target.value)}
              onKeyDown={(event) => event.key === "Enter" && saveKey()}
            />
            <button type="button" className="button primary" disabled={!key.trim()} onClick={saveKey}>
              保存
            </button>
          </div>
        </div>
        {provider.protocol !== "anthropic" && (
          <div className="field" style={{ flex: "1 1 260px" }}>
            <label htmlFor={urlId}>{provider.base_url ? "接口地址（留空用默认）" : "接口地址（必填）"}</label>
            <input id={urlId} className="input" placeholder={provider.base_url || "https://example.com/v1"} {...address} />
            {provider.other_urls.length > 0 && (
              <p className="hint">
                {provider.other_urls.map(([url, what]) => (
                  <span key={url}>
                    {what}：<span className="mono">{url}</span>{" "}
                  </span>
                ))}
              </p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

const MCP_STATES: Record<McpServer["state"], string> = { connecting: "正在连接", ready: "已连上", failed: "没连上" };

/** A kept grant as the user reads it: a command's opening words, or an MCP server's tool. */
function grantLabel(grant: string): string {
  const borrowed = grant.startsWith("mcp:") ? mcpTool(grant.slice("mcp:".length)) : null;
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
      <h2>外来法器（MCP 服务）</h2>
      <div className="field">
        <label htmlFor="mcp-config">服务清单</label>
        <textarea id="mcp-config" className="input mono config" rows={8} spellCheck={false} value={draft} onChange={(event) => setDraft(event.target.value)} />
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

interface Props {
  view: SettingsView;
  onSaveMcp: (config: string) => Promise<string | null>;
  onRefresh: () => void;
  onRevoke: (workspace: string, grant: string) => void;
  onSave: (settings: Settings) => void;
  onSaveKey: (provider: string, key: string) => void;
  onDeleteKey: (provider: string) => void;
}

export function SettingsPage({ view, onSave, onSaveKey, onDeleteKey, onRevoke, onSaveMcp, onRefresh }: Props) {
  const grants = Object.entries(view.grants);
  const { settings } = view;
  const chosen = view.providers.find((provider) => provider.id === settings.provider);
  const model = useDraft(settings.model, (value) => onSave({ ...settings, model: value }));
  const note = effortNote(chosen);
  const limit = useDraft(String(settings.context_limit), (value) => {
    const tokens = Number.parseInt(value, 10);
    if (Number.isFinite(tokens)) onSave({ ...settings, context_limit: tokens });
  });
  return (
    <div className="settings">
      <div className="settings-inner">
        <section>
          <h1>功法</h1>
          <div className="field">
            <label htmlFor="default-provider">新分身的功法出处（模型供应商）</label>
            <select
              id="default-provider"
              className="input"
              value={settings.provider}
              onChange={(event) => {
                const next = view.providers.find((provider) => provider.id === event.target.value);
                onSave({ ...settings, provider: event.target.value, model: next?.models[0]?.[0] ?? "" });
              }}
            >
              {view.providers.map((provider) => (
                <option key={provider.id} value={provider.id}>
                  {provider.name}
                  {view.keys[provider.id]?.source === "none" ? "（未填密钥）" : ""}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor="default-model">修哪门功法（模型）</label>
            <input id="default-model" className="input mono" list="model-suggestions" placeholder="填写模型 ID" {...model} />
            <datalist id="model-suggestions">
              {chosen?.models.map(([id, name]) => (
                <option key={id} value={id}>
                  {name}
                </option>
              ))}
            </datalist>
            <p className="hint">
              {chosen?.models.length ? "可以从列表里选，也可以直接填供应商新出的模型 ID。" : "填写这个服务提供的模型 ID。"}
              已经化出的分身，继续修它当时的功法。
            </p>
          </div>
          <fieldset className="field">
            <legend>分身境界</legend>
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
            <label htmlFor="context-limit">前情上限（token）</label>
            <input id="context-limit" className="input mono" type="number" min={20000} step={10000} {...limit} />
            <p className="hint">
              每次请求都要把整段对话发给模型，对话越长越费灵石。对话到这个数，分身会把前情凝成一段摘要，之后只凭摘要接着做。模型自己装不下这么多时，按它的八成算。最低
              20000。
            </p>
          </div>
        </section>

        <section>
          <h2>禁制</h2>
          <fieldset className="papers">
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
          <h2>长期许可</h2>
          {grants.length === 0 && <p className="muted">还没有哪条命令被"永远都准"。审批命令时可以选它，之后同一处洞府里以同样开头的命令不再请示。</p>}
          {grants.map(([workspace, kept]) => (
            <div className="provider-card" key={workspace}>
              <div className="who">
                <strong className="mono">{workspace}</strong>
                <span className="muted">这处洞府里，以下面这些开头的命令、这些法器不再请示</span>
              </div>
              {kept.map((grant) => (
                <div className="provider" key={grant}>
                  <span className="mono">{grantLabel(grant)}</span>
                  <button type="button" className="button text" onClick={() => onRevoke(workspace, grant)}>
                    撤销
                  </button>
                </div>
              ))}
            </div>
          ))}
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

        <section>
          <h2>功法出处</h2>
          {view.providers.map((provider) => (
            <ProviderRow
              key={provider.id}
              provider={provider}
              status={view.keys[provider.id] ?? { source: "none", preview: null, plaintext: false }}
              settings={settings}
              onSave={onSave}
              onSaveKey={onSaveKey}
              onDeleteKey={onDeleteKey}
            />
          ))}
          <p className="muted">
            密钥存进系统的凭据管理器（Windows 凭据管理器、macOS 钥匙串、Linux 的 Secret Service），不会显示完整内容。凭据管理器用不了时，才退回 {view.home}
            目录里的明文文件。
          </p>
        </section>

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
      </div>
    </div>
  );
}

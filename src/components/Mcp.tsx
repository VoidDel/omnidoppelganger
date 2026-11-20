import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import {
  BUILTINS,
  EMPTY_FORM,
  NEEDS,
  SERVER_NAME,
  entryProblem,
  fromForm,
  launchLine,
  parseServers,
  readPasted,
  toForm,
  withServer,
  type Builtin,
  type ServerEntry,
  type ServerForm,
} from "../lib/mcp";
import type { McpServer } from "../lib/types";
import { FormDialog, Pairs, Row, Segmented } from "./FormDialog";
import { Hint } from "./Hint";

const STATES: Record<McpServer["state"], string> = { connecting: "正在连接", ready: "已连上", failed: "没连上" };

/**
 * A lamp beside a 法器's name, for how it stands at a glance: jade once connected, gold breathing while it
 * connects, cinnabar when it would not, and dark while it is off. Its words are there for a reader and on hover.
 */
function Lamp({ on, server, builtin }: { on: boolean; server: McpServer | undefined; builtin?: Builtin }) {
  const state = !on ? "off" : (server?.state ?? "connecting");
  // Why it did not connect is told here, on hover, rather than across the card.
  const said =
    state === "off"
      ? "没启用"
      : state === "failed"
        ? `没连上：${failure(server?.error ?? "", builtin)}${server?.error ? `\n\n${server.error}` : ""}`
        : state === "ready"
          ? `已连上，${server?.tools.length ?? 0} 个工具${server?.tools.length ? `：\n${server.tools.join("\n")}` : ""}`
          : STATES[state];
  return <span className={`mcp-lamp ${state}`} role="img" aria-label={said} title={said} />;
}

/** Why a server did not connect, in the user's words where it is something they can put right. */
function failure(error: string, builtin?: Builtin): string {
  if (error.startsWith("cannot start `npx`")) return "启动不了：这台电脑上找不到 npx。先装好 Node.js（nodejs.org），再关掉程序重开。";
  if (error.startsWith("cannot start `uvx`")) return "启动不了：这台电脑上找不到 uvx。先装好 uv（docs.astral.sh/uv），再关掉程序重开。";
  if (builtin?.key && /401|unauthori[sz]ed|bad credentials/i.test(error)) return `${builtin.key.label}不对，或者已经过期：关掉再打开，填一个新的。`;
  if (error.startsWith("cannot reach the server")) return "连不上这个服务：网络不通，或者它暂时不在。过一会儿关掉再打开试试。";
  if (error.startsWith("the server did not answer")) return "等了太久没有回应。本机法器第一次启动要先下载，可以关掉再打开一次。";
  return error;
}

/** How a server stands, under its name: connecting, or its tools. Why one did not connect is on its lamp. */
function Status({ server }: { server: McpServer | undefined }) {
  if (!server || server.state === "failed") return null;
  return (
    <p className={`mcp-status ${server.state}`} title={server.tools.length ? server.tools.join("\n") : undefined}>
      {STATES[server.state]}
      {server.state === "ready" && `，${server.tools.length} 个工具`}
    </p>
  );
}

interface CardProps {
  builtin: Builtin;
  home: string;
  on: boolean;
  server: McpServer | undefined;
  /** Switching is held while something else is being saved, or while the list below has edits not saved. */
  held: boolean;
  onSwitch: (on: boolean, key?: string) => Promise<string | null>;
}

interface KeyDialogProps {
  builtin: Builtin;
  /** What it asks for. */
  secret: NonNullable<Builtin["key"]>;
  held: boolean;
  onSwitch: CardProps["onSwitch"];
  onClose: () => void;
}

/**
 * The window that asks for the secret a 法器 needs before it is switched on. Nothing is written until it is filled
 * and confirmed; closed, the switch goes back off.
 */
function KeyDialog({ builtin, secret, held, onSwitch, onClose }: KeyDialogProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [key, setKey] = useState("");
  const [wrong, setWrong] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // Shown as a modal, which keeps the keyboard inside it and closes on Escape.
  useEffect(() => {
    dialog.current?.showModal();
  }, []);
  const id = `mcp-key-${builtin.id}`;
  return (
    <dialog ref={dialog} className="dialog" aria-labelledby={`${id}-title`} onClose={onClose}>
      <form
        method="dialog"
        onSubmit={(event) => {
          event.preventDefault();
          if (!key.trim()) return setWrong(`先把 ${secret.label}粘贴进来。`);
          setSaving(true);
          onSwitch(true, key.trim()).then((error) => {
            setSaving(false);
            if (error) return setWrong(error);
            dialog.current?.close();
          });
        }}
      >
        <h2 id={`${id}-title`}>启用 {builtin.name}</h2>
        <p className="lead">它要一个 {secret.label}才能用。</p>
        <div className="field">
          <label htmlFor={id}>{secret.label}</label>
          <input id={id} className="input mono" type="password" autoFocus autoComplete="off" placeholder={secret.placeholder} value={key} onChange={(event) => setKey(event.target.value)} />
          <p className="hint">
            {secret.help}
            {secret.where && (
              <button type="button" className="button text small" title={secret.where} onClick={() => api.openExternal(secret.where!).catch((error) => console.error(error))}>
                去生成一个
              </button>
            )}
          </p>
        </div>
        {wrong && (
          <p className="hint failed" role="alert">
            {wrong}
          </p>
        )}
        <div className="actions">
          <button type="submit" className="button primary" disabled={held || saving}>
            {saving ? "正在启用…" : "启用"}
          </button>
          <button type="button" className="button text" onClick={() => dialog.current?.close()}>
            算了
          </button>
        </div>
      </form>
    </dialog>
  );
}

/** One 法器 that comes with the app: what it does, how it runs, and the switch that puts it in the list. */
function Card({ builtin, home, on, server, held, onSwitch }: CardProps) {
  const [asking, setAsking] = useState(false);
  const [wrong, setWrong] = useState<string | null>(null);
  const shown: ServerEntry = builtin.entry(home, "…");
  const lit = on && server?.state === "ready";
  const flip = (next: boolean) => {
    // One that needs a key asks for it before it is put in the list.
    if (next && builtin.key) return setAsking(true);
    onSwitch(next).then(setWrong);
  };
  return (
    <div className={`mcp-card ${on ? "on" : ""} ${on && server?.state === "failed" ? "failed" : ""}`}>
      {/* The seal: gold once it is connected, grey while it is off. */}
      <span className={`seal mcp-seal ${lit ? "lit" : ""}`} aria-hidden="true">
        {builtin.seal}
      </span>
      <div className="mcp-body">
        <div className="mcp-head">
          <strong>{builtin.name}</strong>
          <span className="mcp-runs">{builtin.runs === "remote" ? "远程服务" : NEEDS[builtin.needs ?? "node"]}</span>
          <button type="button" className="button text small" title={builtin.site} onClick={() => api.openExternal(builtin.site).catch((error) => console.error(error))}>
            说明
          </button>
        </div>
        <p className="mcp-summary" title={builtin.summary}>
          {builtin.summary}
        </p>
        <p className="mcp-launch mono" title={launchLine(shown)}>
          {launchLine(shown)}
        </p>
        {on && <Status server={server} />}
        {wrong && (
          <p className="hint failed" role="alert">
            {wrong}
          </p>
        )}
      </div>
      {asking && builtin.key && <KeyDialog builtin={builtin} secret={builtin.key} held={held} onSwitch={onSwitch} onClose={() => setAsking(false)} />}
      {/* At the card's right: how it stands, then the switch that changes it. */}
      <div className="mcp-side">
        <Lamp on={on} server={server} builtin={builtin} />
        <label className="mcp-switch" title={held ? "下面的清单还有没保存的改动，先保存或撤销" : on ? `停用 ${builtin.name}` : `启用 ${builtin.name}`}>
          <input type="checkbox" role="switch" checked={on || asking} disabled={held} onChange={(event) => (asking ? setAsking(false) : flip(event.target.checked))} />
          <span className="track" aria-hidden="true" />
          <span className="sr-only">{on ? "停用" : "启用"} {builtin.name}</span>
        </label>
      </div>
    </div>
  );
}

interface ServerDialogProps {
  /** The text of `mcp.json`, which can be read. */
  config: string;
  /** The server being changed, by name; null to add one. */
  editing: string | null;
  /** Resolves with what went wrong, or null once the list is saved. */
  onSave: (config: string) => Promise<string | null>;
  onClose: () => void;
}

const RUNS: [ServerForm["runs"], string][] = [
  ["command", "本机命令"],
  ["url", "远程地址"],
];

/**
 * The window for adding a 法器 of the user's own, or changing one: a form to fill, or the JSON an MCP server's
 * page gives, pasted straight in. JSON pasted into the form fills it.
 */
function ServerDialog({ config, editing, onSave, onClose }: ServerDialogProps) {
  const listed = parseServers(config) as Record<string, ServerEntry>;
  const original = editing ? listed[editing] : undefined;
  const [mode, setMode] = useState<"form" | "json">("form");
  const [form, setForm] = useState<ServerForm>(editing && original ? toForm(editing, original) : EMPTY_FORM);
  const [json, setJson] = useState("");
  const [wrong, setWrong] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [removing, setRemoving] = useState(false);
  const update = (patch: Partial<ServerForm>) => {
    setForm((form) => ({ ...form, ...patch }));
    setWrong(null);
  };
  /** Fill the form from JSON read, or say why it cannot be. */
  const fill = (read: NonNullable<ReturnType<typeof readPasted>>): boolean => {
    if ("entry" in read) return (setForm((form) => toForm(form.name, read.entry)), true);
    const named = Object.entries(read.servers);
    if (named.length !== 1) return false;
    setForm(toForm(named[0][0], named[0][1]));
    return true;
  };
  const switchMode = (next: "form" | "json") => {
    setWrong(null);
    if (next === "json") {
      // A form not yet filled leaves the JSON side empty, for its example to show and a paste to go in.
      const entry = fromForm(form);
      const name = form.name.trim();
      setJson("error" in entry ? "" : `${JSON.stringify(name ? { [name]: entry } : entry, null, 2)}\n`);
      return setMode("json");
    }
    // Nothing written there, or an empty object, is nothing to bring back.
    if (json.trim() && !/^\{\s*\}$/.test(json.trim())) {
      const read = readPasted(json);
      if (!read) return setWrong("这段 JSON 读不懂，改好了再切回表单。");
      if (!fill(read)) return setWrong("这段 JSON 里有好几个服务，表单一次只填一个；就在 JSON 里保存吧。");
    }
    setMode("form");
  };
  /** The servers to put in the list, by name, or what is wrong. */
  const gathered = (): [string, ServerEntry][] | string => {
    if (mode === "form") {
      const entry = fromForm(form);
      return "error" in entry ? entry.error : [[form.name.trim(), entry]];
    }
    const read = readPasted(json);
    if (!read) return '读不懂这段 JSON：要像 {"名称": {"command": …}} 或 {"mcpServers": {…}} 这样。';
    if ("entry" in read) return form.name.trim() ? [[form.name.trim(), read.entry]] : "这段 JSON 只有服务本身，没有名称：切到表单填上名称再保存。";
    return Object.entries(read.servers);
  };
  const save = (close: () => void) => {
    const servers = gathered();
    if (typeof servers === "string") return setWrong(servers);
    for (const [name, entry] of servers) {
      if (!name) return setWrong("填上名称。");
      if (!SERVER_NAME.test(name)) return setWrong(`名称「${name}」只能用字母、数字、- 和 _。`);
      if (BUILTINS.some((builtin) => builtin.id === name)) return setWrong(`「${name}」是内置法器用的名字，换一个。`);
      if (name !== editing && name in listed) return setWrong(`清单里已经有「${name}」了，换个名字，或者去改那一个。`);
      const problem = entryProblem(entry);
      if (problem) return setWrong(`「${name}」${problem}。`);
    }
    let text: string | { error: string } = config;
    // Renamed, it is put in under its new name and the old taken out; switched off, it stays off.
    if (editing && !servers.some(([name]) => name === editing)) text = withServer(text, editing, null);
    for (const [name, entry] of servers) {
      if (typeof text !== "string") break;
      text = withServer(text, name, editing && original?.disabled && servers.length === 1 ? { ...entry, disabled: true } : entry);
    }
    if (typeof text !== "string") return setWrong(text.error);
    setSaving(true);
    onSave(text).then((error) => {
      setSaving(false);
      if (error) return setWrong(`没有保存：${error}`);
      close();
    });
  };
  const remove = (close: () => void) => {
    const text = withServer(config, editing ?? "", null);
    if (typeof text !== "string") return setWrong(text.error);
    setSaving(true);
    onSave(text).then((error) => {
      setSaving(false);
      if (error) return setWrong(`没有删掉：${error}`);
      close();
    });
  };
  return (
    <FormDialog
      seal="器"
      title={editing ? `修改「${editing}」` : "新的法器"}
      modes={[
        ["form", "表单"],
        ["json", "JSON"],
      ]}
      mode={mode}
      onMode={switchMode}
      aside={mode === "form" ? "或直接在这里粘贴它的 JSON" : undefined}
      wrong={wrong}
      onClose={onClose}
      onSubmit={save}
      onPaste={(event) => {
        // JSON pasted anywhere in the form fills the form; any other text is pasted as usual.
        if (mode !== "form") return;
        const read = readPasted(event.clipboardData.getData("text"));
        if (!read) return;
        event.preventDefault();
        // Several servers at once are more than the form holds: they go to the JSON side, to be saved together.
        if (!fill(read) && "servers" in read) {
          setJson(`${JSON.stringify({ mcpServers: read.servers }, null, 2)}\n`);
          setMode("json");
        }
        setWrong(null);
      }}
      actions={(close) => (
        <>
          <button type="submit" className="button primary" disabled={saving}>
            {saving ? "正在保存…" : "保存并连接"}
          </button>
          <button type="button" className="button text" onClick={close}>
            算了
          </button>
          {editing && (
            <span className="form-dialog-end">
              {removing ? (
                <>
                  从清单里删掉它？
                  <button type="button" className="button outline red small" disabled={saving} onClick={() => remove(close)}>
                    删掉
                  </button>
                  <button type="button" className="button text small" onClick={() => setRemoving(false)}>
                    留着
                  </button>
                </>
              ) : (
                <button type="button" className="button text small danger-text" onClick={() => setRemoving(true)}>
                  删掉这件法器
                </button>
              )}
            </span>
          )}
        </>
      )}
    >
      {mode === "form" ? (
        <>
          <Row label="名称" htmlFor="server-name" hint="字母、数字、- 和 _。分身用这个名字称呼它带来的工具。">
            <input
              id="server-name"
              className="input mono"
              autoFocus
              autoComplete="off"
              spellCheck={false}
              placeholder="例如 github"
              value={form.name}
              onChange={(event) => update({ name: event.target.value })}
            />
          </Row>
          <Row label="运行方式">
            <Segmented label="运行方式" options={RUNS} value={form.runs} onChange={(runs) => update({ runs })} />
          </Row>
          {form.runs === "command" ? (
            <>
              <Row label="命令" htmlFor="server-line" hint="命令和参数，就像在终端里输入的那样；带空格的路径用引号括起来。">
                <input
                  id="server-line"
                  className="input mono"
                  spellCheck={false}
                  placeholder="npx -y @modelcontextprotocol/server-github"
                  value={form.line}
                  onChange={(event) => update({ line: event.target.value })}
                />
              </Row>
              <Row label="环境变量" hint="交给这个程序的环境变量，比如它要的令牌。和清单一起明文存在 mcp.json 里。">
                <Pairs rows={form.env} onChange={(env) => update({ env })} keyHolder="名称，如 GITHUB_TOKEN" valueHolder="值" what="环境变量" />
              </Row>
            </>
          ) : (
            <>
              <Row label="地址" htmlFor="server-url" hint="MCP 服务的 HTTP 地址。">
                <input
                  id="server-url"
                  className="input mono"
                  spellCheck={false}
                  placeholder="https://example.com/mcp"
                  value={form.url}
                  onChange={(event) => update({ url: event.target.value })}
                />
              </Row>
              <Row label="请求头" hint="每次请求都带上的头，比如 Authorization。和清单一起明文存在 mcp.json 里。">
                <Pairs rows={form.headers} onChange={(headers) => update({ headers })} keyHolder="Authorization" valueHolder="Bearer …" what="请求头" />
              </Row>
            </>
          )}
        </>
      ) : (
        <Row label="JSON" htmlFor="server-json" hint='MCP 服务说明里给的配置片段直接贴进来：整份 {"mcpServers": {…}}，或者 {"名称": {…}}，一次可以加好几个。'>
          <textarea
            id="server-json"
            className="input mono config"
            rows={12}
            autoFocus
            spellCheck={false}
            placeholder={'{\n  "github": {\n    "command": "npx",\n    "args": ["-y", "@modelcontextprotocol/server-github"]\n  }\n}'}
            value={json}
            onChange={(event) => {
              setJson(event.target.value);
              setWrong(null);
            }}
          />
        </Row>
      )}
    </FormDialog>
  );
}

interface Props {
  /** The text of `mcp.json`. */
  config: string;
  /** How each server it lists stands. */
  servers: McpServer[];
  /** The app's home folder, where a 法器 that keeps something keeps it. */
  home: string;
  /** Resolves with what is wrong with the text, or null once it is saved and the servers are connecting. */
  onSave: (config: string) => Promise<string | null>;
  onRefresh: () => void;
}

/** The 法器 page: MCP servers that come with the app, each off until switched on, and the list as text for any other. */
export function McpPage({ config, servers, home, onSave, onRefresh }: Props) {
  const [draft, setDraft] = useState(config);
  const [problem, setProblem] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  /** The add or change window, open on the server being changed, or on none to add one. */
  const [dialog, setDialog] = useState<{ editing: string | null } | null>(null);
  useEffect(() => setDraft(config), [config]);
  // Servers connect in the background: look again until none is still connecting.
  const connecting = servers.some((server) => server.state === "connecting");
  useEffect(() => {
    if (!connecting) return;
    const timer = setInterval(onRefresh, 1500);
    return () => clearInterval(timer);
  }, [connecting, onRefresh]);

  const listed = parseServers(config);
  const unreadable = typeof listed.error === "string" ? (listed.error as string) : null;
  const names = unreadable ? [] : Object.keys(listed);
  const own = names.filter((name) => !BUILTINS.some((builtin) => builtin.id === name));
  const entries = unreadable ? {} : (listed as Record<string, ServerEntry>);
  /** Whether a server in the list is in use: one switched off by `disabled` is kept there, and not. */
  const working = (name: string) => names.includes(name) && !entries[name]?.disabled;
  const dirty = draft !== config;
  const save = (text: string) => {
    setSaving(true);
    return onSave(text).then((error) => {
      setSaving(false);
      return error;
    });
  };
  const switchTo = (builtin: Builtin, on: boolean, key = "") => {
    const next = withServer(config, builtin.id, on ? builtin.entry(home, key) : null);
    if (typeof next !== "string") return Promise.resolve(`下面的清单读不懂，先把它改好：${next.error}`);
    return save(next);
  };
  /** One of the user's own, switched off or on again: its entry stays as written, with or without the flag. */
  const switchOwn = (name: string, on: boolean) => {
    const { disabled: _, ...entry } = entries[name] ?? {};
    const next = withServer(config, name, on ? entry : { ...entry, disabled: true });
    if (typeof next !== "string") return;
    void save(next);
  };
  const card = (builtin: Builtin) => (
    <Card
      key={builtin.id}
      builtin={builtin}
      home={home}
      on={working(builtin.id)}
      server={servers.find((server) => server.name === builtin.id)}
      held={saving || dirty || !!unreadable}
      onSwitch={(on, key) => switchTo(builtin, on, key)}
    />
  );
  return (
    <div className="settings">
      <div className="settings-inner">
        <section>
          <h1>法器</h1>
          <p className="lead">
            法器是 MCP 服务：给分身添上内置工具之外的本事，查文档、搜网页、操控浏览器。启用后，它带来的工具交给每个分身；分身每次动用前都要请示，可以选之后都准。
          </p>
        </section>

        <section>
          <h2>远程法器</h2>
          <Hint>连到网上的服务，这台电脑上不用装东西。分身用它们时，要查的内容会发给这些服务。</Hint>
          <div className="mcp-list">{BUILTINS.filter((builtin) => builtin.runs === "remote").map(card)}</div>
        </section>

        <section>
          <h2>本机法器</h2>
          <Hint>在这台电脑上启动的程序，要先装好 Node.js 或 uv。第一次启用时要先下载，可能要等一两分钟。</Hint>
          <div className="mcp-list">{BUILTINS.filter((builtin) => builtin.runs === "local").map(card)}</div>
        </section>

        <section>
          <div className="section-head">
            <h2>自定义法器</h2>
            <button
              type="button"
              className="button outline small"
              disabled={saving || dirty || !!unreadable}
              title={unreadable ? "下面的清单读不懂，先把它改好" : dirty ? "下面的清单还有没保存的改动，先保存或撤销" : "加一件别的 MCP 服务"}
              onClick={() => setDialog({ editing: null })}
            >
              ＋ 添加法器
            </button>
          </div>
          {own.length === 0 && !unreadable && <p className="muted">还没有。点「添加法器」填一件，MCP 服务说明里给的 JSON 也可以直接贴进去。</p>}
          {own.length > 0 && (
            <div className="mcp-list">
              {own.map((name) => {
                const on = working(name);
                const server = servers.find((one) => one.name === name);
                return (
                  <div className={`mcp-card ${on ? "on" : ""} ${on && server?.state === "failed" ? "failed" : ""}`} key={name}>
                    <span className={`seal mcp-seal ${on && server?.state === "ready" ? "lit" : ""}`} aria-hidden="true">
                      器
                    </span>
                    <div className="mcp-body">
                      <div className="mcp-head">
                        <strong className="mono">{name}</strong>
                        <button type="button" className="button text small" disabled={saving || dirty} onClick={() => setDialog({ editing: name })}>
                          修改
                        </button>
                      </div>
                      <p className="mcp-launch mono" title={launchLine(entries[name] ?? {})}>
                        {launchLine(entries[name] ?? {})}
                      </p>
                      {on && <Status server={server} />}
                    </div>
                    <div className="mcp-side">
                      <Lamp on={on} server={server} />
                      {/* Off keeps it in the list, ready to be switched on again; to be rid of it, take it out of the list below. */}
                      <label className="mcp-switch" title={saving || dirty ? "下面的清单还有没保存的改动，先保存或撤销" : on ? `停用 ${name}：留在清单里，不再连接` : `启用 ${name}`}>
                        <input type="checkbox" role="switch" checked={on} disabled={saving || dirty} onChange={(event) => switchOwn(name, event.target.checked)} />
                        <span className="track" aria-hidden="true" />
                        <span className="sr-only">
                          {on ? "停用" : "启用"} {name}
                        </span>
                      </label>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
          <details className="raw-config" open={unreadable || dirty ? true : undefined}>
            <summary>直接改 mcp.json</summary>
            <div className="field">
              <label htmlFor="mcp-config">服务清单（mcp.json）</label>
              <textarea id="mcp-config" className="input mono config" rows={8} spellCheck={false} value={draft} onChange={(event) => setDraft(event.target.value)} />
              <Hint>
                用各家通用的 mcpServers 写法，MCP 服务说明里给的配置片段可以直接贴进来：本机程序写 command 和 args，远程服务写 url（需要的话加
                headers）。上面启用的法器也写在这份清单里。保存后立即连接。
              </Hint>
              {unreadable && !dirty && (
                <p className="hint failed" role="alert">
                  这份清单读不懂：{unreadable}。改好之前，上面的开关先停着。
                </p>
              )}
              {problem && (
                <p className="hint failed" role="alert">
                  清单没有保存：{problem}
                </p>
              )}
              <div className="actions">
                <button type="button" className="button primary" disabled={!dirty || saving} onClick={() => save(draft).then(setProblem)}>
                  保存并连接
                </button>
                {dirty && (
                  <button type="button" className="button text" onClick={() => setDraft(config)}>
                    撤销改动
                  </button>
                )}
              </div>
            </div>
          </details>
        </section>
      </div>
      {/* Its close comes a moment after it shuts: it lets go only of the window it opened, not one opened since. */}
      {dialog && <ServerDialog key={dialog.editing ?? ""} config={config} editing={dialog.editing} onSave={save} onClose={() => setDialog((now) => (now === dialog ? null : now))} />}
    </div>
  );
}

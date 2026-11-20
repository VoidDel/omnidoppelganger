import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { BUILTINS, NEEDS, launchLine, parseServers, withServer, type Builtin, type ServerEntry } from "../lib/mcp";
import type { McpServer } from "../lib/types";
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
          <h2>自定义法器</h2>
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
          <div className="field">
            <label htmlFor="mcp-config">服务清单（mcp.json）</label>
            <textarea id="mcp-config" className="input mono config" rows={8} spellCheck={false} value={draft} onChange={(event) => setDraft(event.target.value)} />
            <Hint>
              用各家通用的 mcpServers 写法，MCP 服务说明里给的配置片段可以直接贴进来：本机程序写 command 和 args，远程服务写 url（需要的话加 headers）。上面启用的法器也写在这份清单里。保存后立即连接。
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
        </section>
      </div>
    </div>
  );
}

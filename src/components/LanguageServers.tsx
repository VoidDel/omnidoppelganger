import { useEffect, useState } from "react";
import { api } from "../lib/api";
import type { LanguageServerView } from "../lib/types";
import { ConfirmBubble } from "./ConfirmBubble";
import { Info } from "./Hint";

/**
 * The language servers `code_nav` reads code with, in the settings: for each language, whether its server is on this
 * machine, was downloaded for the app, or is missing; the way to download the one the app can, and to take it away.
 */
export function LanguageServersSection() {
  const [servers, setServers] = useState<LanguageServerView[] | null>(null);
  /** The language being downloaded or taken away. */
  const [busy, setBusy] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [wrong, setWrong] = useState<string | null>(null);
  useEffect(() => {
    api.languageServers().then(setServers, (error) => setWrong(String(error)));
  }, []);
  const act = (key: string, doing: (key: string) => Promise<LanguageServerView[]>) => {
    setRemoving(null);
    setBusy(key);
    setWrong(null);
    doing(key).then(
      (next) => {
        setServers(next);
        setBusy(null);
      },
      (error) => {
        setWrong(String(error));
        setBusy(null);
      },
    );
  };
  if (!servers) return null;
  return (
    <section>
      <h2>
        语言服务（代码导航）
        <Info>
          <p>
            分身用 code_nav 借语言服务看代码：查定义、查引用、看类型、看大纲、查错。先找这台电脑上装好的（PATH 里能运行的那个），没有再用为本应用下载的。rust-analyzer 可以由本应用下载，按事先定好的校验码核对后放在 ~/.odg/lsp；别的语言请自行安装。
          </p>
        </Info>
      </h2>
      <div className="runtime-card">
        {servers.map((server) => {
          const doing = busy === server.key;
          return (
            <div className="runtime-row" key={server.key}>
              <span className={`mcp-lamp ${server.state !== "missing" ? "ready" : doing ? "connecting" : "off"}`} aria-hidden="true" />
              <div className="runtime-what">
                <strong>
                  {server.name}：{server.program}
                </strong>
                {doing ? (
                  <span className="runtime-detail" role="status">
                    {server.state === "missing" ? `正在下载（约 ${server.download_mb} MB）……` : "正在移除……"}
                  </span>
                ) : server.location ? (
                  <span className="runtime-detail mono" title={server.location}>
                    {server.state === "downloaded" ? `为本应用下载的 ${server.version}：` : ""}
                    {server.location}
                  </span>
                ) : server.download_mb ? (
                  <span className="runtime-detail missing">没装。可以下载 {server.version}（约 {server.download_mb} MB），或自行安装：{server.install}</span>
                ) : (
                  <span className="runtime-detail missing">没装。自行安装：{server.install}</span>
                )}
              </div>
              <div className="runtime-acts">
                {server.state === "missing" && server.download_mb !== null && (
                  <button type="button" className="icon-button go" disabled={!!busy} title={`下载 ${server.program} ${server.version}（约 ${server.download_mb} MB）`} onClick={() => act(server.key, api.installLanguageServer)}>
                    <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M8 2.5v7.6M4.8 7 8 10.2 11.2 7M2.8 11.2v1.4a.9.9 0 0 0 .9.9h8.6a.9.9 0 0 0 .9-.9v-1.4" />
                    </svg>
                    <span className="sr-only">下载 {server.program}</span>
                  </button>
                )}
                {server.state === "downloaded" && (
                  <span className="bubble-anchor">
                    <button
                      type="button"
                      className="icon-button danger"
                      disabled={!!busy}
                      aria-expanded={removing === server.key}
                      title={`移除为本应用下载的 ${server.program}`}
                      onClick={() => setRemoving(removing === server.key ? null : server.key)}
                    >
                      <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                        <path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.1 4.3l.6 8.9h6.6l.6-8.9M6.6 6.8v4M9.4 6.8v4" />
                      </svg>
                      <span className="sr-only">移除 {server.program}</span>
                    </button>
                    {removing === server.key && (
                      <ConfirmBubble label={`移除 ${server.program}`} confirm="确认移除" onConfirm={() => act(server.key, api.removeLanguageServer)} onCancel={() => setRemoving(null)}>
                        <p>正在运行的语言服务会先停下。移除后，分身再查 {server.name} 代码时要重新下载约 {server.download_mb}&nbsp;MB。</p>
                      </ConfirmBubble>
                    )}
                  </span>
                )}
              </div>
            </div>
          );
        })}
        {wrong && (
          <p className="runtime-note failed" role="alert">
            {wrong}
          </p>
        )}
      </div>
    </section>
  );
}

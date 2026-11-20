import { useEffect, useState } from "react";
import { api } from "../lib/api";
import type { RuntimeProgress, RuntimeView } from "../lib/types";
import { ConfirmBubble } from "./ConfirmBubble";
import { Info } from "./Hint";

/** What a step of setting up is called, and for the download, how far it has got. */
function stepText(step: RuntimeProgress): string {
  switch (step.step) {
    case "downloading": {
      const done = (step.done / 1048576).toFixed(1);
      return step.total ? `正在下载 Python：${done} / ${(step.total / 1048576).toFixed(1)} MB` : `正在下载 Python：${done} MB`;
    }
    case "unpacking":
      return "正在解开……";
    case "installing":
      return "正在下载并安装 Word、Excel、PPT、PDF 用的库，要一两分钟……";
    case "checking":
      return "正在检查装好的库……";
    case "done":
      return "装好了。";
  }
}

/** The ring that stands where the download icon was while setting up: filling as the download comes in, turning
 * where how long is not known (unpacking, the libraries, the check). */
function Ring({ share }: { share: number | null }) {
  const round = 2 * Math.PI * 10;
  return (
    <span
      className={`runtime-ring ${share === null ? "turning" : ""}`}
      role="progressbar"
      aria-label="正在装文房"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={share === null ? undefined : Math.round(share * 100)}
    >
      <svg aria-hidden="true" width="24" height="24" viewBox="0 0 24 24">
        <circle className="track" cx="12" cy="12" r="10" />
        <circle className="arc" cx="12" cy="12" r="10" strokeDasharray={round} strokeDashoffset={round * (1 - (share ?? 0.28))} />
      </svg>
    </span>
  );
}

/**
 * The document runtime in the settings: whether it is set up, and the way to set it up, again, or take it away;
 * how far setting up has got while it does; and whether there is a LibreOffice to check the documents with.
 */
export function RuntimeSection() {
  const [view, setView] = useState<RuntimeView | null>(null);
  const [step, setStep] = useState<RuntimeProgress | null>(null);
  const [busy, setBusy] = useState<"install" | "remove" | null>(null);
  const [wrong, setWrong] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  useEffect(() => {
    api.runtimeStatus().then(setView, (error) => setWrong(String(error)));
    let stop: (() => void) | undefined;
    let gone = false;
    api.onRuntimeProgress(setStep).then((unlisten) => (gone ? unlisten() : (stop = unlisten)));
    return () => {
      gone = true;
      stop?.();
    };
  }, []);
  const install = () => {
    setBusy("install");
    setWrong(null);
    setStep(null);
    api.installRuntime().then(
      (next) => {
        setView(next);
        setBusy(null);
      },
      (error) => {
        setWrong(String(error));
        setBusy(null);
        setStep(null);
      },
    );
  };
  const remove = () => {
    setRemoving(false);
    setBusy("remove");
    setWrong(null);
    api.removeRuntime().then(
      (next) => {
        setView(next);
        setBusy(null);
        setStep(null);
      },
      (error) => {
        setWrong(String(error));
        setBusy(null);
      },
    );
  };
  if (!view) return null;
  const ready = view.status.state === "ready";
  const outdated = view.status.state === "missing" && view.status.outdated;
  const share = step?.step === "downloading" && step.total ? Math.min(1, step.done / step.total) : null;
  const python = view.status.state === "ready" ? view.status.python : null;
  return (
    <section>
      <h2>
        文房（文档运行环境）
        <Info>
          <p>
            做 Word、Excel、PPT、PDF 的秘籍要用一套 Python 和几个库：python-docx、openpyxl、XlsxWriter、python-pptx、reportlab、pypdf、pandas、Pillow。它们装在
            ~/.odg/runtime，自成一套，不动你电脑上已有的 Python；下载的每个文件都按事先定好的校验码核对，对不上就不装。分身第一次要用时也会请示你下载。
          </p>
          <p>LibreOffice 是可选的：装了它，分身做完文档会转成 PDF 核对版面、把表格公式算一遍；不装也能做文档，只是少这一步核对。</p>
        </Info>
      </h2>
      <div className="runtime-card">
        <div className="runtime-row">
          <span className={`mcp-lamp ${ready ? "ready" : busy === "install" ? "connecting" : "off"}`} aria-hidden="true" />
          <div className="runtime-what">
            <strong>Python {view.python_version} 与文档库</strong>
            {!view.supported ? (
              <span className="runtime-detail missing">这个系统上没有可用的版本</span>
            ) : busy === "install" ? (
              <span className="runtime-detail" role="status">
                {step ? stepText(step) : "正在开始……"}
              </span>
            ) : python ? (
              <span className="runtime-detail mono" title={python}>
                {python}
              </span>
            ) : (
              <span className={`runtime-detail ${outdated ? "missing" : ""}`}>{outdated ? "装的是旧版本，要重新装一次" : `还没装，约下载 ${view.download_mb} MB，装一次所有分身都能用`}</span>
            )}
          </div>
          <div className="runtime-acts">
            {view.supported && !ready && (busy === "install" ? <Ring share={share} /> : (
              <button type="button" className="icon-button go" disabled={!!busy} title={outdated ? `重新下载并安装（约 ${view.download_mb} MB）` : `下载并安装（约 ${view.download_mb} MB）`} onClick={install}>
                <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M8 2.5v7.6M4.8 7 8 10.2 11.2 7M2.8 11.2v1.4a.9.9 0 0 0 .9.9h8.6a.9.9 0 0 0 .9-.9v-1.4" />
                </svg>
                <span className="sr-only">{outdated ? "重新装" : "下载并安装"}</span>
              </button>
            ))}
            {ready && (
              <span className="bubble-anchor">
                <button
                  type="button"
                  className="icon-button danger"
                  disabled={!!busy}
                  aria-expanded={removing}
                  title={busy === "remove" ? "正在移除……" : "移除文档运行环境"}
                  onClick={() => setRemoving(!removing)}
                >
                  <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.1 4.3l.6 8.9h6.6l.6-8.9M6.6 6.8v4M9.4 6.8v4" />
                  </svg>
                  <span className="sr-only">移除</span>
                </button>
                {removing && (
                  <ConfirmBubble label="移除文档运行环境" confirm="确认移除" onConfirm={remove} onCancel={() => setRemoving(false)}>
                    <p>移除后，分身再做文档时要重新下载约 {view.download_mb}&nbsp;MB。</p>
                  </ConfirmBubble>
                )}
              </span>
            )}
          </div>
        </div>
        {wrong && (
          <p className="runtime-note failed" role="alert">
            {wrong}
          </p>
        )}
        <div className="runtime-row">
          <span className={`mcp-lamp ${view.libreoffice ? "ready" : "off"}`} aria-hidden="true" />
          <div className="runtime-what">
            <strong>LibreOffice（可选）</strong>
            {view.libreoffice ? (
              <span className="runtime-detail mono" title={view.libreoffice}>
                {view.libreoffice}
              </span>
            ) : (
              <span className="runtime-detail">没找到：做完的文档不转 PDF 核对版面</span>
            )}
          </div>
          <div className="runtime-acts">
            {!view.libreoffice && (
              <button type="button" className="icon-button" title="去 LibreOffice 官网下载" onClick={() => api.openExternal("https://www.libreoffice.org/download/").catch((error) => console.error(error))}>
                <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M12.5 9v3.6a.9.9 0 0 1-.9.9H3.4a.9.9 0 0 1-.9-.9V4.4a.9.9 0 0 1 .9-.9H7M9.5 2.5h4v4M13.3 2.7 7.6 8.4" />
                </svg>
                <span className="sr-only">去下载</span>
              </button>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

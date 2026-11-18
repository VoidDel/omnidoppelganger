import { useState } from "react";
import type { Settings, SettingsView } from "../lib/types";

// The agent's realm (境界): cultivation stages, lowest to highest, for the API's five effort levels.
const EFFORT_NAMES: Record<string, string> = { low: "练气", medium: "筑基", high: "金丹", xhigh: "元婴", max: "化神" };

const PAPERS: { value: Settings["theme"]; name: string; hint: string; swatches: [string, string][] }[] = [
  { value: "light", name: "原纸", hint: "亮色，宜昼", swatches: [["#FFFFFF", "#1C1842"], ["#FFF4BF", "#E3CB62"], ["#FFDDE4", "#B4234F"]] },
  { value: "dark", name: "碳纸", hint: "暗色，宜夜", swatches: [["#23254C", "#E9E8F8"], ["#332E12", "#7A6A1C"], ["#3F1C2B", "#A8456A"]] },
  { value: "system", name: "跟随系统", hint: "随系统昼夜更替", swatches: [["#FFFFFF", "#1C1842"], ["#23254C", "#E9E8F8"]] },
];

interface Props {
  view: SettingsView;
  onSave: (settings: Settings) => void;
  onSaveKey: (key: string) => void;
  onDeleteKey: () => void;
}

export function SettingsPage({ view, onSave, onSaveKey, onDeleteKey }: Props) {
  const [key, setKey] = useState("");
  const { settings } = view;
  const saveKey = () => {
    if (!key.trim()) return;
    onSaveKey(key);
    setKey("");
  };
  return (
    <div className="settings">
      <div className="settings-inner">
        <section>
          <h1>功法</h1>
          <div className="field">
            <label htmlFor="default-model">新分身默认修哪门功法（模型）</label>
            <select id="default-model" className="input" value={settings.model} onChange={(event) => onSave({ ...settings, model: event.target.value })}>
              {view.models.map((model) => (
                <option key={model.id} value={model.id}>
                  {model.name}
                </option>
              ))}
            </select>
            <p className="hint">已经召出的分身，继续修它当时的功法。</p>
          </div>
          <div className="field">
            <label htmlFor="effort">分身境界</label>
            <select id="effort" className="input" value={settings.effort} onChange={(event) => onSave({ ...settings, effort: event.target.value })}>
              {view.efforts.map((effort) => (
                <option key={effort} value={effort}>
                  {EFFORT_NAMES[effort] ?? effort}
                </option>
              ))}
            </select>
            <p className="hint">境界越高，分身每一步推演得越深，也越慢、越费灵石（额度）。下一次请求起生效。</p>
          </div>
        </section>

        <section>
          <h2>功法出处</h2>
          <div className="provider">
            <div className="who">
              <strong>Anthropic</strong>
              {view.key.source === "saved" && <span className="muted">密钥已保存，只显示开头和结尾</span>}
              {view.key.source === "env" && <span className="muted">正在用环境变量 ANTHROPIC_API_KEY 里的密钥</span>}
              {view.key.source === "none" && <span className="muted">还没有密钥，填上分身才能出关</span>}
              {view.key.preview && <span className="mono">{view.key.preview}</span>}
            </div>
            {view.key.source === "saved" && (
              <button type="button" className="button text" onClick={onDeleteKey}>
                删除密钥
              </button>
            )}
          </div>
          <div className="provider">
            <div className="field" style={{ flex: "1 1 280px" }}>
              <label htmlFor="api-key">{view.key.source === "saved" ? "换一个密钥" : "API 密钥"}</label>
              <input
                id="api-key"
                className="input"
                style={{ maxWidth: "none" }}
                type="password"
                autoComplete="off"
                placeholder="粘贴密钥"
                value={key}
                onChange={(event) => setKey(event.target.value)}
                onKeyDown={(event) => event.key === "Enter" && saveKey()}
              />
            </div>
            <button type="button" className="button primary" disabled={!key.trim()} onClick={saveKey}>
              保存密钥
            </button>
          </div>
          <p className="muted">密钥保存在本机的 {view.home} 目录里，不会显示完整内容。</p>
        </section>

        <section>
          <h2>外观</h2>
          <fieldset className="papers">
            <legend>纸张</legend>
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

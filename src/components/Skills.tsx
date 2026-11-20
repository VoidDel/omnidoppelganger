import { useEffect, useState } from "react";
import { api } from "../lib/api";
import type { ListedSkill, SkillsView } from "../lib/types";
import { Hint } from "./Hint";

/** How each 秘籍 that comes with the app is called and sealed. */
const LOOKS: Record<string, { title: string; seal: string }> = {
  "commit-message": { title: "提交说明", seal: "记" },
  "code-review": { title: "审查改动", seal: "审" },
  debugging: { title: "系统排错", seal: "究" },
  "writing-tests": { title: "编写测试", seal: "验" },
  refactoring: { title: "安全重构", seal: "理" },
  "writing-docs": { title: "撰写文档", seal: "述" },
  "frontend-design": { title: "界面设计", seal: "绘" },
  "release-notes": { title: "发布说明", seal: "告" },
  "security-review": { title: "安全审查", seal: "防" },
};

interface CardProps {
  skill: ListedSkill;
  /** Something else is being changed: the switch waits. */
  held: boolean;
  /** Switch one of the app's on or off, or take one of the user's own away; resolves with what went wrong, or null. */
  onChange: (skill: ListedSkill, on: boolean, force: boolean) => Promise<string | null>;
}

/** One 秘籍: its seal and names, what it is for, and its switch, or for one of the user's own, a way to take it away. */
function Card({ skill, held, onChange }: CardProps) {
  /** A change that loses something waits here to be confirmed: switching off one the user changed, or taking one away. */
  const [confirming, setConfirming] = useState(false);
  const [wrong, setWrong] = useState<string | null>(null);
  const look = LOOKS[skill.name];
  const change = (on: boolean, force = false) =>
    onChange(skill, on, force).then((error) => {
      if (error === "edited") return setConfirming(true);
      setConfirming(false);
      setWrong(error);
    });
  return (
    <div className={`mcp-card ${skill.installed ? "on" : ""}`}>
      <span className={`seal mcp-seal ${skill.installed ? "lit" : ""}`} aria-hidden="true">
        {look?.seal ?? "籍"}
      </span>
      <div className="mcp-body">
        <div className="mcp-head">
          <strong>{look?.title ?? skill.name}</strong>
          <span className="mcp-runs mono">/{skill.name}</span>
          {skill.edited && <span className="chip pending">改过</span>}
        </div>
        <p className="mcp-summary">{skill.description}</p>
        {skill.installed && (
          <p className="mcp-launch mono" title="收在这里，对所有分身生效">
            {skill.directory}
          </p>
        )}
        {wrong && (
          <p className="hint failed" role="alert">
            {wrong}
          </p>
        )}
        {confirming && (
          <p className="undo-confirm">
            {skill.builtin ? "这部秘籍改过，关掉会连改动一起删掉。" : "删掉这部秘籍，连同它文件夹里的东西。"}
            <button type="button" className="button outline red small" onClick={() => change(false, true)}>
              {skill.builtin ? "仍然关掉" : "确认删除"}
            </button>
            <button type="button" className="button text small" onClick={() => setConfirming(false)}>
              算了
            </button>
          </p>
        )}
      </div>
      {skill.builtin ? (
        <label className="mcp-switch" title={skill.installed ? `收起「${look?.title ?? skill.name}」` : `收入「${look?.title ?? skill.name}」：所有分身都能研读它`}>
          <input type="checkbox" role="switch" checked={skill.installed} disabled={held} onChange={(event) => change(event.target.checked)} />
          <span className="track" aria-hidden="true" />
          <span className="sr-only">
            {skill.installed ? "收起" : "收入"} {look?.title ?? skill.name}
          </span>
        </label>
      ) : (
        <button type="button" className="icon-button danger" title="删掉这部秘籍" disabled={held} onClick={() => setConfirming(true)}>
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
            <path d="M2.8 4.3h10.4M6.2 4.3V2.8h3.6v1.5M4.1 4.3l.6 8.9h6.6l.6-8.9M6.6 6.8v4M9.4 6.8v4" />
          </svg>
          <span className="sr-only">删掉 {skill.name}</span>
        </button>
      )}
    </div>
  );
}

/**
 * The 秘籍 page: the skills that come with the app, each kept out of the 分身's sight until switched on, and the
 * user's own, in the user's skills directory.
 */
export function SkillsPage({ onChanged }: { onChanged: () => void }) {
  const [view, setView] = useState<SkillsView | null>(null);
  const [wrong, setWrong] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    api.listSkills().then(setView, (error) => setWrong(String(error)));
  }, []);
  const change = (skill: ListedSkill, on: boolean, force: boolean): Promise<string | null> => {
    setSaving(true);
    const request = skill.builtin ? api.switchSkill(skill.name, on, force) : api.removeSkill(skill.name);
    return request.then(
      (next) => {
        setSaving(false);
        setView(next);
        onChanged();
        return null;
      },
      (error) => {
        setSaving(false);
        return String(error);
      },
    );
  };
  const builtIn = view?.skills.filter((skill) => skill.builtin) ?? [];
  const own = view?.skills.filter((skill) => !skill.builtin) ?? [];
  return (
    <div className="settings">
      <div className="settings-inner">
        <section>
          <h1>秘籍</h1>
          <p className="lead">
            秘籍是专项的做事说明。收入的秘籍，分身只记得它的名字和一句说明，不占心神；遇到对得上的事才翻出来研读，照着做。也可以在输入框打 /秘籍名，叫分身照某一部办事。
          </p>
          {wrong && (
            <p className="hint failed" role="alert">
              {wrong}
            </p>
          )}
        </section>

        <section>
          <h2>内置秘籍</h2>
          <Hint>随程序带来的，默认都不收入。打开就写进你的秘籍目录，对所有分身生效，下一轮起列给分身；关掉就收起。收入后可以照自己的习惯改它，改过的关掉前会先问。</Hint>
          <div className="mcp-list">
            {builtIn.map((skill) => (
              <Card key={skill.name} skill={skill} held={saving} onChange={change} />
            ))}
          </div>
        </section>

        <section>
          <h2>自己的秘籍</h2>
          {view && own.length === 0 && (
            <p className="muted">
              还没有。可以叫分身装一部（它会用 install_skill，从文件夹或 Git 仓库装，先请示你），或者把带 SKILL.md 的文件夹放进 <span className="mono">{view.folder}</span>。
            </p>
          )}
          <div className="mcp-list">
            {own.map((skill) => (
              <Card key={skill.directory} skill={skill} held={saving} onChange={change} />
            ))}
          </div>
        </section>

        <section>
          <h2>怎么写一部秘籍</h2>
          <p className="muted">
            每部秘籍是一个文件夹，里面一份 SKILL.md，开头用 --- 围起来写上 name 和 description，后面是做事说明；说明里提到的脚本、模板放在同一个文件夹里。放在{" "}
            <span className="mono">{view?.folder ?? "~/.odg/skills"}</span> 的对所有分身生效；放在某处洞府的 <span className="mono">.odg/skills</span>{" "}
            里的只对那处洞府生效，重名时以洞府里的为准。
          </p>
        </section>
      </div>
    </div>
  );
}

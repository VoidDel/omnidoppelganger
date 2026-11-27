import "../styles/pages.css";
import { useEffect, useState } from "react";
import { getCurrentLocale, type Locale, useI18n } from "../i18n";
import { api } from "../lib/api";
import { type LocalizedSkillError, localizeSkillError } from "../lib/errorCode";
import type { ListedSkill, SkillsView } from "../lib/types";
import { ConfirmBubble } from "./ConfirmBubble";
import { FormDialog, Row } from "./FormDialog";
import { Icon } from "./Icon";
import { Info } from "./Hint";

/** How each 秘籍 that comes with the app is called and sealed. */
const LOOKS: Record<string, { title: string; seal: string }> = {
  "commit-message": { title: "提交说明", seal: "记" },
  "code-review": { title: "审查改动", seal: "审" },
  debugging: { title: "系统排错", seal: "究" },
  "writing-tests": { title: "编写测试", seal: "验" },
  "e2e-verification": { title: "端到端验收", seal: "测" },
  refactoring: { title: "安全重构", seal: "理" },
  "writing-docs": { title: "撰写文档", seal: "述" },
  "frontend-design": { title: "界面设计", seal: "绘" },
  "release-notes": { title: "发布说明", seal: "告" },
  "security-review": { title: "安全审查", seal: "防" },
  docx: { title: "Word 文档", seal: "文" },
  xlsx: { title: "Excel 表格", seal: "表" },
  pptx: { title: "PPT 演示", seal: "演" },
  pdf: { title: "PDF 文件", seal: "印" },
  image: { title: "画图", seal: "绘" },
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
        <p className="mcp-summary" title={skill.description}>
          {skill.description}
        </p>
        {skill.installed && (
          <p className="mcp-launch mono" title={`收在这里，对所有分身生效：${skill.directory}`}>
            {skill.directory}
          </p>
        )}
        {wrong && (
          <p className="hint failed" role="alert">
            {wrong}
          </p>
        )}
      </div>
      {/* What loses something asks in a bubble from the switch or the icon that asked for it. */}
      <span className="bubble-anchor">
        {skill.builtin ? (
          <label className="mcp-switch" title={skill.installed ? `收起「${look?.title ?? skill.name}」` : `收入「${look?.title ?? skill.name}」：所有分身都能研读它`}>
            <input type="checkbox" role="switch" checked={skill.installed} disabled={held} onChange={(event) => change(event.target.checked)} />
            <span className="track" aria-hidden="true" />
            <span className="sr-only">
              {skill.installed ? "收起" : "收入"} {look?.title ?? skill.name}
            </span>
          </label>
        ) : (
          <button type="button" className="icon-button danger" aria-expanded={confirming} title="删掉这部秘籍" disabled={held} onClick={() => setConfirming(!confirming)}>
            <Icon name="trash3" size={14} />
            <span className="sr-only">删掉 {skill.name}</span>
          </button>
        )}
        {confirming && (
          <ConfirmBubble label={skill.builtin ? "关掉改过的秘籍" : "删掉这部秘籍"} confirm={skill.builtin ? "仍然关掉" : "确认删除"} onConfirm={() => change(false, true)} onCancel={() => setConfirming(false)}>
            <p>{skill.builtin ? "这部秘籍改过，关掉会连改动一起删掉。" : "删掉这部秘籍，连同它文件夹里的东西。"}</p>
          </ConfirmBubble>
        )}
      </span>
    </div>
  );
}

type Way = "write" | "folder" | "git";

const WAYS: [Way, string][] = [
  ["write", "自己写"],
  ["folder", "从文件夹"],
  ["git", "从 Git 仓库"],
];

/** A skill's name as skills are named everywhere: lower-case letters, digits and `-`. */
const SKILL_NAME = /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;

/** What went wrong putting a skill in, parsed from structured backend error codes (`[ERR_*]` / `{ code }`) and localized via `i18n`. */
export function skillProblem(error: unknown, locale: Locale = getCurrentLocale()): LocalizedSkillError {
  return localizeSkillError(error, locale);
}

/** The window for adding a 秘籍 of the user's own: written here, or put in from a folder or a Git repository. */
function SkillDialog({ onDone, onClose }: { onDone: (view: SkillsView) => void; onClose: () => void }) {
  const { locale, t } = useI18n();
  const [way, setWay] = useState<Way>("write");
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [body, setBody] = useState("");
  const [folder, setFolder] = useState("");
  const [address, setAddress] = useState("");
  const [within, setWithin] = useState("");
  const [wrong, setWrong] = useState<LocalizedSkillError | null>(null);
  const [saving, setSaving] = useState(false);
  const typed =
    <T,>(set: (value: T) => void) =>
    (value: T) => {
      set(value);
      setWrong(null);
    };
  const put = (close: () => void, replace = false, inside?: string) => {
    const missing =
      way === "write"
        ? !SKILL_NAME.test(name.trim())
          ? t("err.skillInvalidName")
          : !description.trim()
            ? "写一句说明：分身靠它决定什么时候翻开这部秘籍。"
            : !body.trim()
              ? "写上做法：分身翻开秘籍时照着做的就是这些。"
              : null
        : way === "folder"
          ? !folder.trim() && "选一个带 SKILL.md 的文件夹。"
          : !/^(https?:\/\/|git@|ssh:\/\/)/.test(address.trim()) && !address.trim().endsWith(".git") && t("err.skillInvalidSource");
    if (missing) return setWrong({ text: missing });
    setSaving(true);
    setWrong(null);
    const request =
      way === "write"
        ? api.writeSkill(name.trim(), description, body, replace)
        : api.installSkill(way === "folder" ? folder.trim() : address.trim(), (inside ?? within).trim() || null, replace);
    request.then(
      (view) => {
        setSaving(false);
        onDone(view);
        close();
      },
      (error) => {
        setSaving(false);
        setWrong(skillProblem(error, locale));
      },
    );
  };
  return (
    <FormDialog
      seal="籍"
      title="新的秘籍"
      modes={WAYS}
      mode={way}
      onMode={(next) => {
        setWay(next);
        setWrong(null);
      }}
      wrong={
        wrong &&
        ((close) => (
          <>
            {wrong.text}
            {wrong.replace && (
              <button type="button" className="button outline red small" disabled={saving} onClick={() => put(close, true)}>
                替换它
              </button>
            )}
            {wrong.choices && (
              <span className="skill-choices">
                {wrong.choices.map((choice) => (
                  <button
                    key={choice}
                    type="button"
                    className="button outline small mono"
                    disabled={saving}
                    onClick={() => {
                      const inside = within.trim() ? `${within.trim().replace(/\/+$/, "")}/${choice}` : choice;
                      setWithin(inside);
                      put(close, false, inside);
                    }}
                  >
                    {choice}
                  </button>
                ))}
              </span>
            )}
          </>
        ))
      }
      onClose={onClose}
      onSubmit={(close) => put(close)}
      actions={(close) => (
        <>
          <button type="submit" className="button primary" disabled={saving}>
            {saving ? (way === "git" ? "正在下载…" : "正在收入…") : "收入"}
          </button>
          <button type="button" className="button text" onClick={close}>
            算了
          </button>
        </>
      )}
    >
      {way === "write" && (
        <>
          <Row label="名称" htmlFor="skill-name" hint="小写字母、数字和 -。在输入框打 /名称，就能叫分身照它办事。">
            <input
              id="skill-name"
              className="input mono"
              autoFocus
              autoComplete="off"
              spellCheck={false}
              placeholder="例如 weekly-report"
              value={name}
              onChange={(event) => typed(setName)(event.target.value)}
            />
          </Row>
          <Row label="说明" htmlFor="skill-description" hint="一句话：它管什么事、什么时候该用。分身平时只看得到这一句，靠它决定要不要翻开。">
            <input
              id="skill-description"
              className="input"
              placeholder="例如：写周报时用，汇总这周合进主干的改动，按模块分组"
              value={description}
              onChange={(event) => typed(setDescription)(event.target.value)}
            />
          </Row>
          <Row label="做法" htmlFor="skill-body" hint="照着做的说明，用 Markdown 写。分身翻开这部秘籍时读的就是这些。">
            <textarea
              id="skill-body"
              className="input config"
              rows={10}
              placeholder={"## 步骤\n\n1. 用 git log 列出这周合进主干的提交\n2. 按模块分组，每组一两句话\n3. 末尾写下周要做的"}
              value={body}
              onChange={(event) => typed(setBody)(event.target.value)}
            />
          </Row>
        </>
      )}
      {way === "folder" && (
        <Row label="文件夹" htmlFor="skill-folder" hint="带 SKILL.md 的文件夹，会整个复制进你的秘籍目录，原来的文件夹不动。">
          <div className="inline wide">
            <input
              id="skill-folder"
              className="input mono"
              autoFocus
              spellCheck={false}
              placeholder="D:\skills\pdf"
              value={folder}
              onChange={(event) => typed(setFolder)(event.target.value)}
            />
            <button
              type="button"
              className="button outline"
              onClick={() =>
                api.pickFolder("选一个带 SKILL.md 的文件夹").then((picked) => {
                  if (picked) typed(setFolder)(picked);
                })
              }
            >
              选择…
            </button>
          </div>
        </Row>
      )}
      {way === "git" && (
        <>
          <Row label="地址" htmlFor="skill-address" hint="仓库的地址，或 GitHub 上某个文件夹的地址。要装有 Git，只取最新的一次提交。">
            <input
              id="skill-address"
              className="input mono"
              autoFocus
              spellCheck={false}
              placeholder="https://github.com/anthropics/skills/tree/main/skills/pdf"
              value={address}
              onChange={(event) => typed(setAddress)(event.target.value)}
            />
          </Row>
          <Row label="子文件夹" htmlFor="skill-within" hint="可选。秘籍不在仓库顶上时，填它所在的文件夹。">
            <input
              id="skill-within"
              className="input mono"
              spellCheck={false}
              placeholder="例如 skills/pdf"
              value={within}
              onChange={(event) => typed(setWithin)(event.target.value)}
            />
          </Row>
        </>
      )}
    </FormDialog>
  );
}

/**
 * The 秘籍 page: the skills that come with the app, each kept out of the 分身's sight until switched on, and the
 * user's own, in the user's skills directory.
 */
export function SkillsPage({ onChanged, embedded = false }: { onChanged: () => void; embedded?: boolean }) {
  const [view, setView] = useState<SkillsView | null>(null);
  const [wrong, setWrong] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  /** The add window while it is open; a fresh one each time it is opened. */
  const [adding, setAdding] = useState<object | null>(null);
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
  const body = (
    <>
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
          <h2>
            内置秘籍
            <Info>随程序带来的，默认都不收入。打开就写进你的秘籍目录，对所有分身生效，下一轮起列给分身；关掉就收起。收入后可以照自己的习惯改它，改过的关掉前会先问。</Info>
          </h2>
          <div className="mcp-list">
            {builtIn.map((skill) => (
              <Card key={skill.name} skill={skill} held={saving} onChange={change} />
            ))}
          </div>
        </section>

        <section>
          <div className="section-head">
            <h2>
              自己的秘籍
              <Info label="怎么写一部秘籍">
                每部秘籍是一个文件夹，里面一份 SKILL.md，开头用 --- 围起来写上 name 和 description，后面是做事说明；说明里提到的脚本、模板放在同一个文件夹里。放在{" "}
                <span className="mono">{view?.folder ?? "~/.odg/skills"}</span> 的对所有分身生效；放在某处洞府的 <span className="mono">.odg/skills</span>{" "}
                里的只对那处洞府生效，重名时以洞府里的为准。
              </Info>
            </h2>
            <button type="button" className="button outline small" disabled={saving} onClick={() => setAdding({})}>
              ＋ 添加秘籍
            </button>
          </div>
          {view && own.length === 0 && <p className="muted">还没有。点「添加秘籍」自己写一部，或者从文件夹、Git 仓库收入；也可以叫分身装一部，它会先请示你。</p>}
          <div className="mcp-list">
            {own.map((skill) => (
              <Card key={skill.directory} skill={skill} held={saving} onChange={change} />
            ))}
          </div>
        </section>
    </>
  );
  const modal = adding && (
    <SkillDialog
      onDone={(next) => {
        setView(next);
        onChanged();
      }}
      // Its close comes a moment after it shuts: it lets go only of the window it opened, not one opened since.
      onClose={() => setAdding((now) => (now === adding ? null : now))}
    />
  );
  if (embedded) {
    return (
      <>
        {body}
        {modal}
      </>
    );
  }
  return (
    <div className="settings">
      <div className="settings-inner">{body}</div>
      {modal}
    </div>
  );
}

import "../styles/pages.css";
import { useEffect, useState } from "react";
import { MODES, modeOf } from "../lib/modes";
import { Combo } from "./Combo";
import { ConfirmBubble } from "./ConfirmBubble";
import { Icon } from "./Icon";
import { WEEKDAYS, blank, clock, describeWhen, draftOf, scheduleOf, type Draft } from "../lib/schedule";
import type { Mode, Schedule, SessionItem } from "../lib/types";
import { ahead } from "../lib/when";

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;

const KINDS: { value: Draft["kind"]; name: string }[] = [
  { value: "daily", name: "每天或每周几天" },
  { value: "every", name: "每隔一阵" },
  { value: "once", name: "只一次" },
];

interface FormProps {
  draft: Draft;
  /** Folders that sessions already work in, offered for the folder field. */
  folders: string[];
  onChange: (draft: Draft) => void;
  onPick: () => Promise<string | null>;
  /** Resolves with what went wrong, or null once it is saved. */
  onSave: (schedule: Schedule) => Promise<string | null>;
  onCancel: () => void;
}

function Form({ draft, folders, onChange, onPick, onSave, onCancel }: FormProps) {
  const [wrong, setWrong] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const set = (change: Partial<Draft>) => onChange({ ...draft, ...change });
  const save = () => {
    const schedule = scheduleOf(draft);
    if (typeof schedule === "string") return setWrong(schedule);
    setSaving(true);
    onSave(schedule).then((error) => {
      setSaving(false);
      setWrong(error);
    });
  };
  return (
    <form
      className="schedule-form"
      aria-label={draft.id ? "改这门功课" : "添一门功课"}
      onSubmit={(event) => {
        event.preventDefault();
        save();
      }}
    >
      <div className="field">
        <label htmlFor="schedule-prompt">交办的事</label>
        <textarea id="schedule-prompt" className="input area" rows={4} placeholder="到时辰要分身办什么，写清楚：它只看得到这段话。" value={draft.prompt} onChange={(event) => set({ prompt: event.target.value })} />
      </div>
      <div className="field">
        <label htmlFor="schedule-name">功课名（可不填）</label>
        <input id="schedule-name" className="input" placeholder="不填就用交办的事的第一行" value={draft.name} onChange={(event) => set({ name: event.target.value })} />
      </div>
      <div className="field">
        <label htmlFor="schedule-folder">洞府</label>
        <div className="inline wide">
          <input id="schedule-folder" className="input mono" list="schedule-folders" placeholder="文件夹的路径" value={draft.workspace} onChange={(event) => set({ workspace: event.target.value })} />
          <datalist id="schedule-folders">
            {folders.map((folder) => (
              <option key={folder} value={folder} />
            ))}
          </datalist>
          <button type="button" className="button outline" onClick={() => onPick().then((folder) => folder && set({ workspace: folder }))}>
            选文件夹
          </button>
        </div>
      </div>
      <fieldset className="field">
        <legend>时辰</legend>
        <div className="kinds">
          {KINDS.map((kind) => (
            <label className="kind" key={kind.value}>
              <input type="radio" name="schedule-kind" checked={draft.kind === kind.value} onChange={() => set({ kind: kind.value })} />
              {kind.name}
            </label>
          ))}
        </div>
        {draft.kind === "daily" && (
          <div className="when-row">
            <label htmlFor="schedule-time" className="sr-only">
              几点
            </label>
            <input id="schedule-time" className="input short" type="time" value={draft.time} onChange={(event) => set({ time: event.target.value })} />
            <div className="days" role="group" aria-label="每周哪几天">
              {WEEKDAYS.map((day) => (
                <label className="day" key={day.value} title={`周${day.name}`}>
                  <input
                    type="checkbox"
                    aria-label={`周${day.name}`}
                    checked={draft.days.includes(day.value)}
                    onChange={(event) => set({ days: event.target.checked ? [...draft.days, day.value] : draft.days.filter((kept) => kept !== day.value) })}
                  />
                  <span aria-hidden="true">{day.name}</span>
                </label>
              ))}
            </div>
            <span className="muted">{draft.days.length === 0 ? "一天不选，就是每天" : ""}</span>
          </div>
        )}
        {draft.kind === "every" && (
          <div className="when-row">
            <span>每隔</span>
            <label htmlFor="schedule-gap" className="sr-only">
              隔多久
            </label>
            <input id="schedule-gap" className="input short mono" type="number" min={1} step={1} value={draft.gap} onChange={(event) => set({ gap: event.target.value })} />
            <label htmlFor="schedule-unit" className="sr-only">
              单位
            </label>
            <Combo
              id="schedule-unit"
              value={draft.unit}
              options={[
                { value: "minutes", text: "分钟" },
                { value: "hours", text: "小时" },
              ]}
              editable={false}
              onSave={(unit) => set({ unit: unit as Draft["unit"] })}
            />
            <span className="muted">从保存时算起；上一回还没办完，这一回就跳过</span>
          </div>
        )}
        {draft.kind === "once" && (
          <div className="when-row">
            <label htmlFor="schedule-at" className="sr-only">
              哪天几点
            </label>
            <input id="schedule-at" className="input" type="datetime-local" value={draft.at} onChange={(event) => set({ at: event.target.value })} />
            <span className="muted">办过一次就停课</span>
          </div>
        )}
      </fieldset>
      <div className="field">
        <label htmlFor="schedule-mode">禁制</label>
        <Combo id="schedule-mode" value={draft.mode} options={MODES.map((mode) => ({ value: mode.value, text: mode.name }))} editable={false} onSave={(mode) => set({ mode: mode as Mode })} />
        <p className="hint">{modeOf(draft.mode).hint}。到时辰本尊多半不在：要请示的事会一直候旨，等本尊回来定夺。</p>
      </div>
      <label className="check">
        <input type="checkbox" checked={draft.isolated} onChange={(event) => set({ isolated: event.target.checked })} />
        <span>
          每回给它一份独立的工作树
          <span className="muted">（洞府得是 Git 仓库；它改它的，不碰洞府里的文件，本尊看过再并回）</span>
        </span>
      </label>
      {wrong && (
        <p className="hint failed" role="alert">
          {wrong}
        </p>
      )}
      <div className="actions">
        <button type="submit" className="button primary" disabled={saving}>
          {draft.id ? "保存" : "定下这门功课"}
        </button>
        <button type="button" className="button text" onClick={onCancel}>
          算了
        </button>
      </div>
    </form>
  );
}

interface Props {
  schedules: Schedule[];
  sessions: SessionItem[];
  /** The mode a new schedule starts with: the one new sessions start in. */
  mode: Mode;
  /** The folder a new schedule starts with. */
  workspace: string;
  onOpen: (id: string) => void;
  onPick: () => Promise<string | null>;
  /** Each resolves with what went wrong, or null once it is done. */
  onSave: (schedule: Schedule) => Promise<string | null>;
  onDelete: (id: string) => Promise<string | null>;
  onRun: (id: string) => Promise<string | null>;
  onCleanSessions?: (ids: string[]) => void;
}

/** The 功课 page: the tasks set to start by themselves, and the form that sets one. */
export function SchedulesPage({ schedules, sessions, mode, workspace, onOpen, onPick, onSave, onDelete, onRun, onCleanSessions }: Props) {
  /** The form that is open: for a new schedule, or for the one with its id. */
  const [draft, setDraft] = useState<Draft | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [cleaning, setCleaning] = useState(false);
  const idleScheduledIds = sessions.filter((s) => !!s.schedule && s.status === "idle").map((s) => s.id);
  const [wrong, setWrong] = useState<string | null>(null);
  // The times shown ("下次") are read against the clock, so the page looks again as it moves.
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 30_000);
    return () => clearInterval(timer);
  }, []);
  const folders = [...new Set(sessions.filter((session) => !session.parent).map((session) => session.worktree?.origin ?? session.workspace))];
  const act = (request: Promise<string | null>) => request.then(setWrong);
  const form = (current: Draft) => (
    <Form
      draft={current}
      folders={folders}
      onChange={setDraft}
      onPick={onPick}
      onSave={(schedule) =>
        onSave(schedule).then((error) => {
          if (!error) setDraft(null);
          return error;
        })
      }
      onCancel={() => setDraft(null)}
    />
  );
  return (
    <div className="settings">
      <div className="settings-inner">
        <section>
          <h1>功课</h1>
          <p className="lead">
            给分身定下功课：到了时辰，自行化出一个分身去办，办完留下回报，本尊回来再看。只在这个程序开着的时候看时辰；关着时错过的，下次打开补办一回。
          </p>
          {wrong && (
            <p className="hint failed" role="alert">
              {wrong}
            </p>
          )}
          {schedules.length === 0 && !draft && <p className="muted">还没有功课。</p>}
          {schedules.map((schedule) => {
            const started = sessions.find((session) => session.id === schedule.last?.session);
            return draft?.id === schedule.id ? (
              <div className="provider-card" key={schedule.id}>
                {form(draft)}
              </div>
            ) : (
              <div className={`schedule-card schedule ${schedule.enabled ? "" : "off"}`} key={schedule.id}>
                <div className="schedule-head">
                  {/* 课 in gold while it keeps its hours, grey while it is stopped. */}
                  <span className="seal schedule-seal" aria-hidden="true">
                    课
                  </span>
                  <strong>{schedule.name}</strong>
                  {!schedule.enabled && <span className="chip pending">已停课</span>}
                  <span className="spacer" />
                  <>
                      {/* Pictures, each named in its title and for a screen reader. */}
                      <button type="button" className="icon-button" title="现在就办：不等时辰，现在就化一个分身去办；下次的时辰照旧" onClick={() => act(onRun(schedule.id))}>
                        <Icon name="play-fill" size={15} />
                        <span className="sr-only">现在就办</span>
                      </button>
                      <button
                        type="button"
                        className="icon-button"
                        title={schedule.enabled ? "停课：先停下，留着不删" : "复课：接着按时辰办"}
                        onClick={() => act(onSave({ ...schedule, enabled: !schedule.enabled }))}
                      >
                        <Icon name={schedule.enabled ? "pause-fill" : "arrow-clockwise"} size={15} />
                        <span className="sr-only">{schedule.enabled ? "停课" : "复课"}</span>
                      </button>
                      <button type="button" className="icon-button" title="改这门功课" onClick={() => setDraft(draftOf(schedule, new Date()))}>
                        <Icon name="pencil" size={14} />
                        <span className="sr-only">改</span>
                      </button>
                      <span className="bubble-anchor">
                        <button
                          type="button"
                          className="icon-button danger"
                          aria-expanded={deleting === schedule.id}
                          title="删除这门功课"
                          onClick={() => setDeleting(deleting === schedule.id ? null : schedule.id)}
                        >
                          <Icon name="trash3" size={14} />
                          <span className="sr-only">删除</span>
                        </button>
                        {deleting === schedule.id && (
                          <ConfirmBubble
                            label="删除这门功课"
                            confirm="确认删除"
                            onConfirm={() => {
                              setDeleting(null);
                              act(onDelete(schedule.id));
                            }}
                            onCancel={() => setDeleting(null)}
                          >
                            <p>这门功课就此作罢；它化出过的分身还留着。</p>
                          </ConfirmBubble>
                        )}
                      </span>
                  </>
                </div>
                <p className="schedule-task">{schedule.prompt}</p>
                {/* When it next comes due, large, and how far off that is; or why it will not. */}
                <div className="schedule-next">
                  {schedule.next ? (
                    <>
                      <span className="next-label">下次</span>
                      <span className="next-when">{clock(schedule.next)}</span>
                      <span className="next-ahead">{ahead(schedule.next, now)}</span>
                    </>
                  ) : (
                    <span className="next-none">{schedule.when.kind === "once" && schedule.last ? "办过了，不会再办" : "停着，不会自己开始"}</span>
                  )}
                </div>
                <p className="schedule-facts">
                  <span>{describeWhen(schedule.when)}</span>
                  <span title={schedule.workspace}>洞府 {baseName(schedule.workspace)}</span>
                  <span>禁制「{modeOf(schedule.mode).name}」</span>
                  {schedule.isolated && <span>独立工作树</span>}
                  {schedule.last && (
                    <span>
                      上次：{clock(schedule.last.at)}
                      {started && (
                        <>
                          ，
                          <button type="button" className="link" onClick={() => onOpen(started.id)}>
                            看那个分身
                          </button>
                        </>
                      )}
                    </span>
                  )}
                </p>
                {schedule.last?.error && <p className="hint failed">上次没办成：{schedule.last.error}</p>}
              </div>
            );
          })}
          {draft && !draft.id ? (
            <div className="provider-card">{form(draft)}</div>
          ) : (
            <div className="actions">
              <button type="button" className="button primary" onClick={() => setDraft(blank(workspace, mode, new Date()))}>
                添一门功课
              </button>
              {onCleanSessions && idleScheduledIds.length > 0 && (
                <span className="bubble-anchor">
                  <button
                    type="button"
                    className="button outline"
                    aria-expanded={cleaning}
                    onClick={() => setCleaning(!cleaning)}
                  >
                    清扫已办分身（{idleScheduledIds.length}）
                  </button>
                  {cleaning && (
                    <ConfirmBubble
                      label="清扫已办功课分身"
                      confirm="确认清扫"
                      onConfirm={() => {
                        setCleaning(false);
                        onCleanSessions(idleScheduledIds);
                      }}
                      onCancel={() => setCleaning(false)}
                    >
                      <p>将删掉 {idleScheduledIds.length} 个已收功的功课分身及其快照；功课本身的设置不受影响。</p>
                    </ConfirmBubble>
                  )}
                </span>
              )}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}

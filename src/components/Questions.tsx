import { useEffect, useRef, useState } from "react";
import type { ToolItem } from "../lib/fold";
import { type Draft, blank, choose, isAnswered, replyOf, sayReply } from "../lib/questions";
import type { Reply } from "../lib/types";

/** The seconds left until `expiresAt`, counted down while the page is open; null when there is no end. */
export function useSecondsLeft(expiresAt: string | undefined, frozen: boolean): number | null {
  const left = () => (expiresAt ? Math.max(0, Math.ceil((new Date(expiresAt).getTime() - Date.now()) / 1000)) : null);
  const [seconds, setSeconds] = useState(left);
  useEffect(() => {
    if (!expiresAt || frozen) return;
    setSeconds(left());
    const timer = setInterval(() => setSeconds(left()), 250);
    return () => clearInterval(timer);
  }, [expiresAt, frozen]);
  return frozen ? null : seconds;
}

interface Props {
  tool: ToolItem;
  /** The page shows the past: the questions are shown as they stood, with nothing to press. */
  frozen: boolean;
  /** One reply per question, in their order; `null` leaves it to the 分身. */
  onAnswer: (toolUseId: string, answers: Reply[] | null) => void;
}

/** What the 分身 asked 本尊 with `ask_user`: a form while it waits, and once it is over, what came of it. */
export function QuestionSheet(props: Props) {
  const { tool } = props;
  return tool.question?.answered || tool.result ? <Asked tool={tool} /> : <AskSheet {...props} />;
}

/** The form: a tab for each question, its options or a field to write in, and the answers handed in together. */
function AskSheet({ tool, frozen, onAnswer }: Props) {
  const { questions, expiresAt } = tool.question!;
  const [drafts, setDrafts] = useState<Draft[]>(() => questions.map(blank));
  const [at, setAt] = useState(0);
  /** Handed in: nothing more is pressed while the answer goes to the 分身. */
  const [sent, setSent] = useState(false);
  const seconds = useSecondsLeft(expiresAt, frozen);
  const advance = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(advance.current), []);
  const question = questions[at];
  const draft = drafts[at];
  const replies = questions.map((each, index) => replyOf(each, drafts[index]));
  const last = at === questions.length - 1;
  const locked = frozen || sent;
  const update = (next: Draft) => setDrafts(drafts.map((known, index) => (index === at ? next : known)));
  const pick = (label: string) => {
    update(choose(question, draft, label));
    // One choice made, the next question comes up by itself, a moment later so the choice is seen.
    if (question.kind === "single" && !last) {
      window.clearTimeout(advance.current);
      advance.current = window.setTimeout(() => setAt(at + 1), 220);
    }
  };
  const hand = (answers: Reply[] | null) => {
    setSent(true);
    onAnswer(tool.id, answers);
  };
  const many = questions.length > 1;
  return (
    <div className={`sign-sheet ask-sheet ${frozen ? "frozen" : ""}`}>
      <div className="sign-head">
        <h2>{many ? `分身有 ${questions.length} 件事请教本尊` : "分身有事请教本尊"}</h2>
        {/* With nobody answering in time, the 分身 decides for itself. */}
        {seconds !== null && (
          <span className={`sign-countdown ${seconds <= 10 ? "urgent" : ""}`} role="timer" title="放手施为下不久等：到时无人作答，分身自己拿主意，并说明选了什么">
            <strong>{seconds}</strong> 秒后交给分身自定
          </span>
        )}
      </div>
      {many && (
        <div className="ask-tabs" role="tablist" aria-label="问题">
          {questions.map((each, index) => (
            <button
              type="button"
              role="tab"
              key={index}
              id={`${tool.id}-tab-${index}`}
              aria-selected={index === at}
              aria-controls={`${tool.id}-panel`}
              className={`ask-tab ${index === at ? "on" : ""} ${isAnswered(replies[index]) ? "done" : ""}`}
              onClick={() => setAt(index)}
            >
              <span className="ask-tab-mark" aria-hidden="true">
                {isAnswered(replies[index]) ? "✓" : index + 1}
              </span>
              {each.header}
            </button>
          ))}
        </div>
      )}
      <div className="ask-panel" role={many ? "tabpanel" : undefined} id={`${tool.id}-panel`} aria-labelledby={many ? `${tool.id}-tab-${at}` : undefined}>
        <p className="ask-question" id={`${tool.id}-question-${at}`}>
          {question.question}
          {question.kind === "multiple" && <span className="ask-hint">可多选</span>}
        </p>
        {question.kind === "text" ? (
          <textarea
            className="ask-text"
            rows={3}
            value={draft.text}
            disabled={locked}
            aria-labelledby={`${tool.id}-question-${at}`}
            placeholder="写下本尊的意思"
            onChange={(event) => update({ ...draft, text: event.target.value })}
          />
        ) : (
          <div className="ask-options" role={question.kind === "single" ? "radiogroup" : "group"} aria-labelledby={`${tool.id}-question-${at}`}>
            {(question.options ?? []).map((option) => {
              const on = draft.chosen.includes(option.label) && !(question.kind === "single" && draft.other);
              return (
                <label key={option.label} className={`ask-option ${on ? "on" : ""}`}>
                  <input
                    type={question.kind === "single" ? "radio" : "checkbox"}
                    name={`${tool.id}-${at}`}
                    checked={on}
                    disabled={locked}
                    onChange={() => pick(option.label)}
                  />
                  <span className="ask-label">{option.label}</span>
                  {option.description && <span className="ask-description">{option.description}</span>}
                </label>
              );
            })}
            {/* Not among the options, or more than they say: in 本尊's own words. */}
            <label className={`ask-option ask-other ${question.kind === "single" && draft.other ? "on" : ""}`}>
              {question.kind === "single" ? (
                <input type="radio" name={`${tool.id}-${at}`} checked={draft.other} disabled={locked} onChange={() => update({ ...draft, other: true })} />
              ) : (
                <span className="ask-other-gap" aria-hidden="true" />
              )}
              <span className="ask-label">{question.kind === "single" ? "其他" : "补充"}</span>
              <input
                type="text"
                className="ask-other-text"
                value={draft.text}
                disabled={locked}
                placeholder={question.kind === "single" ? "都不合意，自己写" : "还有要说的，写在这里"}
                aria-label={question.kind === "single" ? "其他：自己写" : "补充"}
                onFocus={() => question.kind === "single" && !draft.other && update({ ...draft, other: true })}
                onChange={(event) => update({ ...draft, text: event.target.value, other: question.kind === "single" ? true : draft.other })}
              />
            </label>
          </div>
        )}
      </div>
      {frozen && <p className="muted">当时分身停在这里，候本尊作答。</p>}
      <div className="actions" hidden={frozen}>
        {many && !last ? (
          <button type="button" className="button outline" disabled={sent} onClick={() => setAt(at + 1)}>
            下一题
          </button>
        ) : null}
        <button type="button" className="button solid" disabled={sent} onClick={() => hand(replies)}>
          {replies.every(isAnswered) ? "呈上答复" : many && replies.some(isAnswered) ? "呈上，没答的交给分身" : "呈上答复"}
        </button>
        <button type="button" className="button text" disabled={sent} onClick={() => hand(null)}>
          不答，交给分身自定
        </button>
      </div>
    </div>
  );
}

/** The questions once they are over: each with 本尊's answer, or how the 分身 came to decide it. */
function Asked({ tool }: { tool: ToolItem }) {
  const { questions, answered } = tool.question!;
  const whole = !answered ? "没等到答复，这一轮就停了" : answered.timedOut ? "无人作答，交给分身自定" : answered.declined || answered.answers.length === 0 ? "本尊没有作答，交给分身自定" : "";
  return (
    <div className="asked">
      <div className="asked-head">
        <span className="asked-seal" aria-hidden="true">
          问
        </span>
        <span>分身请教了本尊</span>
        {whole && <span className="asked-whole">{whole}</span>}
      </div>
      <dl className="asked-list">
        {questions.map((question, index) => {
          const reply = answered?.answers[index];
          return (
            <div key={index} className="asked-item">
              <dt>
                <span className="asked-header">{question.header}</span>
                {question.question}
              </dt>
              {!whole && <dd className={isAnswered(reply ?? {}) ? "" : "muted"}>{isAnswered(reply ?? {}) ? sayReply(reply) : "没答，交给分身自定"}</dd>}
            </div>
          );
        })}
      </dl>
    </div>
  );
}

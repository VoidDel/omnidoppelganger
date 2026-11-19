import { useEffect, useState } from "react";
import { clock } from "../lib/schedule";
import type { Schedule, SessionItem } from "../lib/types";
import { ago, ahead } from "../lib/when";
import { Light } from "./Light";

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
const homeOf = (session: SessionItem) => session.worktree?.origin ?? session.workspace;
const titleOf = (session: SessionItem) => session.title || (session.parent ? "子分身" : "新分身");

interface DeskProps {
  sessions: SessionItem[];
  schedules: Schedule[];
  onOpen: (id: string) => void;
  onSchedules: () => void;
}

/**
 * What stands on the desk when no 分身 is open: those waiting for 本尊, those at work, the 功课 coming due,
 * and the 分身 last at work. Each is a way in. A part with nothing in it is left out; a desk with nothing on it
 * shows nothing.
 */
export function Desk({ sessions, schedules, onOpen, onSchedules }: DeskProps) {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 30_000);
    return () => clearInterval(timer);
  }, []);
  const asking = sessions.filter((session) => session.status === "waiting_approval");
  const working = sessions.filter((session) => session.status === "running");
  const coming = schedules
    .filter((schedule) => schedule.enabled && schedule.next)
    .sort((a, b) => Date.parse(a.next!) - Date.parse(b.next!))
    .slice(0, 3);
  // The list is already newest first; a 子分身 is reached from its 分身.
  const recent = sessions.filter((session) => session.status === "idle" && !session.parent).slice(0, 4);
  if (asking.length + working.length + coming.length + recent.length === 0) return null;
  const where = (session: SessionItem) => <span className="desk-where">{baseName(homeOf(session))}</span>;
  return (
    <div className="desk">
      {asking.length > 0 && (
        <section className="desk-part asking" aria-label="候本尊定夺">
          <h2>候本尊定夺</h2>
          {asking.map((session, at) => (
            <button type="button" key={session.id} className="desk-item await-slip" style={{ animationDelay: `${at * 70}ms` }} onClick={() => onOpen(session.id)}>
              <span className="seal small" aria-hidden="true">
                候
              </span>
              <span className="desk-title">{titleOf(session)}</span>
              {where(session)}
            </button>
          ))}
        </section>
      )}
      {working.length > 0 && (
        <section className="desk-part" aria-label="运功中">
          <h2>运功中</h2>
          {working.map((session, at) => (
            <button type="button" key={session.id} className="desk-item busy" style={{ animationDelay: `${at * 70}ms` }} onClick={() => onOpen(session.id)}>
              <span className="row-seal busy" aria-hidden="true">
                <Light />运
              </span>
              <span className="desk-title">{titleOf(session)}</span>
              {where(session)}
            </button>
          ))}
        </section>
      )}
      {coming.length > 0 && (
        <section className="desk-part" aria-label="将至的功课">
          <h2>将至的功课</h2>
          {coming.map((schedule, at) => (
            <button type="button" key={schedule.id} className="desk-item coming" style={{ animationDelay: `${at * 70}ms` }} title={clock(schedule.next!)} onClick={onSchedules}>
              <span className="seal small schedule-seal" aria-hidden="true">
                课
              </span>
              <span className="desk-title">{schedule.name}</span>
              <span className="desk-when">{ahead(schedule.next!, now)}</span>
            </button>
          ))}
        </section>
      )}
      {recent.length > 0 && (
        <section className="desk-part" aria-label="近来的分身">
          <h2>近来的分身</h2>
          {recent.map((session, at) => (
            <button type="button" key={session.id} className="desk-item" style={{ animationDelay: `${at * 70}ms` }} title={baseName(homeOf(session))} onClick={() => onOpen(session.id)}>
              <span className="row-seal" aria-hidden="true" />
              <span className="desk-title">{titleOf(session)}</span>
              <span className="desk-when">{ago(session.updated_at, now)}</span>
            </button>
          ))}
        </section>
      )}
    </div>
  );
}

import { type CSSProperties, useEffect, useState } from "react";
import { useI18n } from "../i18n";
import { clock } from "../lib/schedule";
import type { Schedule, SessionItem } from "../lib/types";
import { ago, ahead } from "../lib/when";
import { Light } from "./Light";

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
const homeOf = (session: SessionItem) => session.worktree?.origin ?? session.workspace;

interface DeskProps {
  sessions: SessionItem[];
  schedules: Schedule[];
  onOpen: (id: string) => void;
  onSchedules: () => void;
}

export function Desk({ sessions, schedules, onOpen, onSchedules }: DeskProps) {
  const { locale, t } = useI18n();
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 30_000);
    return () => clearInterval(timer);
  }, []);
  const titleOf = (session: SessionItem) =>
    session.title || (session.parent ? t("nav.childDefault") : t("nav.sessionDefault"));
  const asking = sessions.filter((session) => session.status === "waiting_approval");
  const working = sessions.filter((session) => session.status === "running");
  const coming = schedules
    .filter((schedule) => schedule.enabled && schedule.next)
    .sort((a, b) => Date.parse(a.next!) - Date.parse(b.next!))
    .slice(0, 3);
  const recent = sessions.filter((session) => session.status === "idle" && !session.parent).slice(0, 4);
  if (asking.length + working.length + coming.length + recent.length === 0) return null;
  const where = (session: SessionItem) => <span className="desk-where">{baseName(homeOf(session))}</span>;
  return (
    <div className="desk">
      {asking.length > 0 && (
        <section className="desk-part asking" aria-label={t("desk.asking")}>
          <h2>{t("desk.asking")}</h2>
          {asking.map((session, at) => (
            <button type="button" key={session.id} className="desk-item await-slip" style={{ "--nth": at } as CSSProperties} onClick={() => onOpen(session.id)}>
              <span className="seal small" aria-hidden="true">
                {t("nav.sealAwait")}
              </span>
              <span className="desk-title">{titleOf(session)}</span>
              {where(session)}
            </button>
          ))}
        </section>
      )}
      {working.length > 0 && (
        <section className="desk-part" aria-label={t("desk.working")}>
          <h2>{t("desk.working")}</h2>
          {working.map((session, at) => (
            <button type="button" key={session.id} className="desk-item busy" style={{ "--nth": at } as CSSProperties} onClick={() => onOpen(session.id)}>
              <span className="row-seal busy" aria-hidden="true">
                <Light />
                {t("nav.sealRun")}
              </span>
              <span className="desk-title">{titleOf(session)}</span>
              {where(session)}
            </button>
          ))}
        </section>
      )}
      {coming.length > 0 && (
        <section className="desk-part" aria-label={t("desk.coming")}>
          <h2>{t("desk.coming")}</h2>
          {coming.map((schedule, at) => (
            <button
              type="button"
              key={schedule.id}
              className="desk-item coming"
              style={{ "--nth": at } as CSSProperties}
              title={clock(schedule.next!, locale)}
              onClick={onSchedules}
            >
              <span className="seal small schedule-seal" aria-hidden="true">
                {locale === "en" ? "CR" : "课"}
              </span>
              <span className="desk-title">{schedule.name}</span>
              <span className="desk-when">{ahead(schedule.next!, now, locale)}</span>
            </button>
          ))}
        </section>
      )}
      {recent.length > 0 && (
        <section className="desk-part" aria-label={t("desk.recent")}>
          <h2>{t("desk.recent")}</h2>
          {recent.map((session, at) => (
            <button type="button" key={session.id} className="desk-item" style={{ "--nth": at } as CSSProperties} title={baseName(homeOf(session))} onClick={() => onOpen(session.id)}>
              <span className="row-seal" aria-hidden="true" />
              <span className="desk-title">{titleOf(session)}</span>
              <span className="desk-when">{ago(session.updated_at, now, locale)}</span>
            </button>
          ))}
        </section>
      )}
    </div>
  );
}

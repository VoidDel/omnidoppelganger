//! Tasks the user sets to start by themselves: at a time, on days of the
//! week, or every so often, a session is made in a folder and given a
//! message, with nobody at the keyboard.
//!
//! This module keeps the schedules, in one file, and says which are due.
//! It starts nothing: whoever holds the sessions looks at the clock, takes
//! what is due with [`ScheduleStore::take_due`], and starts it. A schedule
//! therefore runs only while that program is open. One whose time passed
//! while it was closed is due as soon as it opens, once, however many times
//! were missed.
//!
//! Times of day are the user's wall clock. The functions take the present
//! moment as an argument, in the time zone to read that clock in.

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Days, Duration, LocalResult, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::fsutil::write_atomically;
use crate::policy::Mode;

/// The longest gap an `every` schedule may have: a week.
pub const MAX_MINUTES: u32 = 7 * 24 * 60;
/// How much of the task a schedule without a name is named after.
const NAME_CHARS: usize = 40;

/// When a schedule is due.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum When {
    /// One time, at a date and time on the wall clock: `2026-01-31T09:00`.
    Once { at: String },
    /// At a time of day, `09:00`, on the given days of the week (1 is Monday,
    /// 7 is Sunday); on every day when none is given.
    Daily {
        time: String,
        #[serde(default)]
        days: Vec<u8>,
    },
    /// Again and again, this many minutes after the time before.
    Every { minutes: u32 },
}

fn date_time(text: &str) -> Result<NaiveDateTime, String> {
    ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
        .ok_or_else(|| format!("`{text}` is not a date and time like 2026-01-31T09:00"))
}

fn time_of_day(text: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(text, "%H:%M").map_err(|_| format!("`{text}` is not a time of day like 09:00"))
}

/// The moment a wall-clock time names in `zone`. A time skipped when the
/// clocks go forward names the moment an hour later; one that comes twice
/// when they go back, the first of the two.
fn moment<Tz: TimeZone>(zone: &Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => Some(at.with_timezone(&Utc)),
        LocalResult::None => zone.from_local_datetime(&(local + Duration::hours(1))).earliest().map(|at| at.with_timezone(&Utc)),
    }
}

impl When {
    /// Whether this names times at all: its date, time, days, and gap are ones that exist.
    pub fn check(&self) -> Result<(), String> {
        match self {
            When::Once { at } => date_time(at).map(|_| ()),
            When::Daily { time, days } => {
                time_of_day(time)?;
                match days.iter().find(|day| !(1..=7).contains(*day)) {
                    Some(day) => Err(format!("{day} is not a day of the week: Monday is 1 and Sunday is 7")),
                    None => Ok(()),
                }
            }
            When::Every { minutes } if (1..=MAX_MINUTES).contains(minutes) => Ok(()),
            When::Every { minutes } => Err(format!("a schedule can repeat every 1 to {MAX_MINUTES} minutes, not every {minutes}")),
        }
    }

    /// The first time after `now` that this names, read on the clock of
    /// `now`'s time zone. `None` when there is none: a one-time whose time
    /// has come, or times that [`When::check`] refuses.
    pub fn next_after<Tz: TimeZone>(&self, now: &DateTime<Tz>) -> Option<DateTime<Utc>> {
        let zone = now.timezone();
        let now = now.with_timezone(&Utc);
        match self {
            When::Once { at } => moment(&zone, date_time(at).ok()?).filter(|at| *at > now),
            When::Daily { time, days } => {
                let time = time_of_day(time).ok()?;
                let today = now.with_timezone(&zone).date_naive();
                // Today, and the seven days after: the same day of next week is the furthest it can be.
                (0..=7)
                    .filter_map(|ahead| today.checked_add_days(Days::new(ahead)))
                    .filter(|date| days.is_empty() || days.iter().any(|day| u32::from(*day) == date.weekday().number_from_monday()))
                    .filter_map(|date| moment(&zone, date.and_time(time)))
                    .find(|at| *at > now)
            }
            When::Every { minutes } => (1..=MAX_MINUTES).contains(minutes).then(|| now + Duration::minutes(i64::from(*minutes))),
        }
    }
}

/// How a schedule's last start went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// RFC 3339 time of the start.
    pub at: String,
    /// The session that was started, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Why none was, or why it was given no message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn on() -> bool {
    true
}

/// A task that starts by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Given by the store; empty in a schedule that is new.
    #[serde(default)]
    pub id: String,
    /// What the user calls it, and the title of the sessions it starts.
    #[serde(default)]
    pub name: String,
    /// The folder its sessions work in.
    pub workspace: String,
    /// The message each session is given.
    pub prompt: String,
    pub when: When,
    /// What its sessions do without asking. Nobody may be there to answer what they do ask.
    #[serde(default)]
    pub mode: Mode,
    /// Whether each session gets a Git worktree of its own; see [`crate::worktree`].
    #[serde(default)]
    pub isolated: bool,
    /// Off, it keeps its place in the list and is never due.
    #[serde(default = "on")]
    pub enabled: bool,
    /// RFC 3339 time it is next due; none when it is off or has no time left. Set by the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// How its last start went. Set by the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<Run>,
}

/// A moment as the store writes it.
pub fn stamp(at: &DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The present moment on this machine's clock, for the functions here that ask for one.
pub fn now() -> DateTime<chrono::Local> {
    chrono::Local::now()
}

pub struct ScheduleStore {
    path: PathBuf,
    /// Held across a read-change-write of the file.
    writing: Mutex<()>,
}

impl ScheduleStore {
    /// A store kept in the file at `path`, which need not exist yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), writing: Mutex::new(()) }
    }

    /// Every schedule, in the order they were added. A missing or unreadable file holds none.
    pub fn all(&self) -> Vec<Schedule> {
        std::fs::read_to_string(&self.path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// Read the list, change it, and write it back when `change` says it changed.
    fn change<T>(&self, change: impl FnOnce(&mut Vec<Schedule>) -> Result<(T, bool), String>) -> Result<T, String> {
        let _guard = self.writing.lock().unwrap();
        let mut schedules = self.all();
        let (result, changed) = change(&mut schedules)?;
        if changed {
            let failed = |error: std::io::Error| format!("cannot write {}: {error}", self.path.display());
            if let Some(directory) = self.path.parent() {
                std::fs::create_dir_all(directory).map_err(failed)?;
            }
            let text = serde_json::to_vec_pretty(&schedules).map_err(|error| error.to_string())?;
            write_atomically(&self.path, &text).map_err(failed)?;
        }
        Ok(result)
    }

    /// Add a schedule, or put it in the place of the one with its id, and
    /// work out when it is next due. Returns it as kept. Refused when it has
    /// no task, its folder is not there, its times do not exist, or it is on
    /// and has no time left to run at.
    pub fn save<Tz: TimeZone>(&self, schedule: Schedule, now: &DateTime<Tz>) -> Result<Schedule, String> {
        let mut schedule = Schedule { name: schedule.name.trim().to_string(), prompt: schedule.prompt.trim().to_string(), ..schedule };
        if schedule.prompt.is_empty() {
            return Err("the schedule has no task: say what the agent is to do".to_string());
        }
        if !std::path::Path::new(&schedule.workspace).is_dir() {
            return Err(format!("{} is not a directory", schedule.workspace));
        }
        schedule.when.check()?;
        if schedule.name.is_empty() {
            schedule.name = schedule.prompt.lines().next().unwrap_or_default().chars().take(NAME_CHARS).collect();
        }
        if let When::Daily { days, .. } = &mut schedule.when {
            days.sort_unstable();
            days.dedup();
        }
        schedule.next = match schedule.enabled {
            true => Some(stamp(&schedule.when.next_after(now).ok_or("that time has passed: pick a later one, or turn the schedule off")?)),
            false => None,
        };
        self.change(|schedules| {
            match schedules.iter_mut().find(|kept| !schedule.id.is_empty() && kept.id == schedule.id) {
                Some(kept) => {
                    schedule.last = kept.last.take();
                    *kept = schedule.clone();
                }
                None => {
                    schedule.id = uuid::Uuid::now_v7().to_string();
                    schedule.last = None;
                    schedules.push(schedule.clone());
                }
            }
            Ok((schedule, true))
        })
    }

    /// Take a schedule out of the list. Taking out one that is not there succeeds.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.change(|schedules| {
            let before = schedules.len();
            schedules.retain(|kept| kept.id != id);
            Ok(((), schedules.len() != before))
        })
    }

    /// The schedules whose time has come. Each is moved on to its next time
    /// as it is taken, so none is taken twice; a one-time, with no next time,
    /// is turned off. The caller starts them and tells how with [`ScheduleStore::record`].
    pub fn take_due<Tz: TimeZone>(&self, now: &DateTime<Tz>) -> Result<Vec<Schedule>, String> {
        let present = now.with_timezone(&Utc);
        self.change(|schedules| {
            let mut due = Vec::new();
            for schedule in schedules.iter_mut().filter(|schedule| schedule.enabled) {
                let Some(next) = schedule.next.as_deref().and_then(|next| DateTime::parse_from_rfc3339(next).ok()) else { continue };
                if next > present {
                    continue;
                }
                due.push(schedule.clone());
                schedule.next = schedule.when.next_after(now).as_ref().map(stamp);
                schedule.enabled = schedule.next.is_some();
            }
            let changed = !due.is_empty();
            Ok((due, changed))
        })
    }

    /// Keep how a schedule's start went. Nothing is kept for one that has been removed since.
    pub fn record(&self, id: &str, run: Run) -> Result<(), String> {
        self.change(|schedules| match schedules.iter_mut().find(|kept| kept.id == id) {
            Some(kept) => {
                kept.last = Some(run);
                Ok(((), true))
            }
            None => Ok(((), false)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    /// A moment on a clock eight hours ahead of UTC. 2026-11-19 is a Thursday.
    fn at(text: &str) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(8 * 3600).unwrap().from_local_datetime(&date_time(text).unwrap()).unwrap()
    }

    fn next(when: &When, now: &str) -> Option<String> {
        when.next_after(&at(now)).as_ref().map(stamp)
    }

    #[test]
    fn a_daily_time_is_next_due_today_or_on_the_next_day_it_names() {
        let daily = When::Daily { time: "09:00".into(), days: vec![] };
        // The wall clock is the user's: nine in the morning there is one at night in UTC.
        assert_eq!(next(&daily, "2026-11-19T08:00").as_deref(), Some("2026-11-19T01:00:00Z"));
        assert_eq!(next(&daily, "2026-11-19T09:00").as_deref(), Some("2026-11-20T01:00:00Z"), "the time itself has come, so the next is tomorrow's");
        // Mondays and Fridays, asked on a Thursday, then on the Friday after its time, then on the Monday at its time.
        let some = When::Daily { time: "09:00".into(), days: vec![1, 5] };
        assert_eq!(next(&some, "2026-11-19T12:00").as_deref(), Some("2026-11-20T01:00:00Z"));
        assert_eq!(next(&some, "2026-11-20T09:30").as_deref(), Some("2026-11-23T01:00:00Z"));
        assert_eq!(next(&some, "2026-11-23T09:00").as_deref(), Some("2026-11-27T01:00:00Z"));
        // One day a week, asked on that day after its time: the same day next week.
        let thursdays = When::Daily { time: "09:00".into(), days: vec![4] };
        assert_eq!(next(&thursdays, "2026-11-19T10:00").as_deref(), Some("2026-11-26T01:00:00Z"));
    }

    #[test]
    fn a_one_time_is_due_once_and_a_gap_counts_from_now() {
        let once = When::Once { at: "2026-11-19T18:30".into() };
        assert_eq!(next(&once, "2026-11-19T08:00").as_deref(), Some("2026-11-19T10:30:00Z"));
        assert_eq!(next(&once, "2026-11-19T18:30"), None);
        assert_eq!(next(&When::Once { at: "2026-11-19T18:30:05".into() }, "2026-11-19T18:30").as_deref(), Some("2026-11-19T10:30:05Z"));
        assert_eq!(next(&When::Every { minutes: 90 }, "2026-11-19T08:00").as_deref(), Some("2026-11-19T01:30:00Z"));
    }

    #[test]
    fn refuses_times_that_do_not_exist() {
        for (when, said) in [
            (When::Once { at: "tomorrow".into() }, "not a date and time"),
            (When::Once { at: "2026-02-30T09:00".into() }, "not a date and time"),
            (When::Daily { time: "25:00".into(), days: vec![] }, "not a time of day"),
            (When::Daily { time: "09:00".into(), days: vec![0] }, "not a day of the week"),
            (When::Every { minutes: 0 }, "every 1 to"),
            (When::Every { minutes: MAX_MINUTES + 1 }, "every 1 to"),
        ] {
            let error = when.check().unwrap_err();
            assert!(error.contains(said), "{when:?}: {error}");
            assert_eq!(when.next_after(&at("2026-11-19T08:00")), None, "{when:?}");
        }
    }

    fn store() -> (tempfile::TempDir, ScheduleStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(directory.path().join("home").join("schedules.json"));
        (directory, store)
    }

    fn schedule(directory: &tempfile::TempDir, when: When) -> Schedule {
        Schedule {
            id: String::new(),
            name: String::new(),
            workspace: directory.path().to_string_lossy().into_owned(),
            prompt: "  run the tests and say what failed\nin detail  ".into(),
            when,
            mode: Mode::Auto,
            isolated: false,
            enabled: true,
            next: None,
            last: None,
        }
    }

    #[test]
    fn a_schedule_is_due_when_its_time_comes_and_then_moves_on() {
        let (directory, store) = store();
        let daily = store.save(schedule(&directory, When::Daily { time: "09:00".into(), days: vec![5, 1, 5] }), &at("2026-11-19T08:00")).unwrap();
        assert!(!daily.id.is_empty());
        assert_eq!(daily.name, "run the tests and say what failed", "a schedule without a name is named after its task's first line");
        assert_eq!(daily.when, When::Daily { time: "09:00".into(), days: vec![1, 5] });
        assert_eq!(daily.next.as_deref(), Some("2026-11-20T01:00:00Z"));
        let once = store.save(Schedule { name: "once".into(), ..schedule(&directory, When::Once { at: "2026-11-20T08:00".into() }) }, &at("2026-11-19T08:00")).unwrap();
        assert_eq!(store.all(), [daily.clone(), once.clone()]);

        assert_eq!(store.take_due(&at("2026-11-20T07:59")).unwrap(), []);
        assert_eq!(store.take_due(&at("2026-11-20T08:00")).unwrap(), std::slice::from_ref(&once));
        // Taken, a one-time has no time left and is off; it is not due again.
        assert_eq!(store.take_due(&at("2026-11-20T08:01")).unwrap(), []);
        let kept = store.all();
        assert_eq!((kept[1].enabled, kept[1].next.as_deref()), (false, None));

        // The program was closed over the weekend and Monday's time: on Tuesday it is due once, and next on Friday.
        assert_eq!(store.take_due(&at("2026-11-24T10:00")).unwrap(), std::slice::from_ref(&daily));
        assert_eq!(store.take_due(&at("2026-11-24T10:00")).unwrap(), []);
        assert_eq!(store.all()[0].next.as_deref(), Some("2026-11-27T01:00:00Z"));
    }

    #[test]
    fn keeps_how_the_last_start_went_through_an_edit_and_forgets_a_removed_schedule() {
        let (directory, store) = store();
        let now = at("2026-11-19T08:00");
        let saved = store.save(schedule(&directory, When::Every { minutes: 30 }), &now).unwrap();
        let run = Run { at: "2026-11-19T00:30:00Z".into(), session: Some("s1".into()), error: None };
        store.record(&saved.id, run.clone()).unwrap();

        // An edit keeps its place and its last start. Turned off, it has no next time and is never due.
        let edited = store.save(Schedule { name: "nightly".into(), enabled: false, last: None, ..saved.clone() }, &now).unwrap();
        assert_eq!((edited.id.as_str(), edited.last.as_ref(), edited.next.as_deref()), (saved.id.as_str(), Some(&run), None));
        assert_eq!(store.take_due(&at("2027-01-01T00:00")).unwrap(), []);
        assert_eq!(store.all(), [edited]);

        store.remove(&saved.id).unwrap();
        store.remove(&saved.id).unwrap();
        store.record(&saved.id, run).unwrap();
        assert_eq!(store.all(), []);
    }

    #[test]
    fn refuses_a_schedule_it_could_never_run() {
        let (directory, store) = store();
        let now = at("2026-11-19T08:00");
        let daily = || schedule(&directory, When::Daily { time: "09:00".into(), days: vec![] });
        for (schedule, said) in [
            (Schedule { prompt: " \n".into(), ..daily() }, "no task"),
            (Schedule { workspace: directory.path().join("gone").to_string_lossy().into_owned(), ..daily() }, "not a directory"),
            (schedule(&directory, When::Every { minutes: 0 }), "every 1 to"),
            (schedule(&directory, When::Once { at: "2026-11-19T07:00".into() }), "time has passed"),
        ] {
            let error = store.save(schedule, &now).unwrap_err();
            assert!(error.contains(said), "{error}");
        }
        assert_eq!(store.all(), []);
        // A one-time whose time has passed can still be kept, off.
        let off = store.save(Schedule { enabled: false, ..schedule(&directory, When::Once { at: "2026-11-19T07:00".into() }) }, &now).unwrap();
        assert_eq!(off.next, None);
    }
}

//! Pure scheduling logic for the Sword & Staff reminder bot.
//!
//! Core idea: events are defined by *day number since a server opened*
//! (day 0 = launch day). Each subscribed channel records the real-world date
//! its server opened (`start`). For that channel, an event on day D happens on
//! the calendar date `start + D days`. Each event may carry `notice_days`: how
//! many days early reminders should fire (`[3, 1, 0]` => 3 days before, 1 day
//! before, and on the day). Every (event, offset) pair becomes one reminder
//! with a concrete UTC fire time (`notify` time on the relevant date).

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::events::Event;
use crate::discord_time::{DiscordTimestamp, TsStyle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    /// Exact UTC datetime this reminder should be sent.
    pub fire_dt: DateTime<Utc>,
    pub season: String,
    /// Day-within-season the event occurs on.
    pub day: i64,
    /// How many days early this particular reminder is (0 == day-of).
    pub offset: i64,
    /// Pre-composed bold first line (what the event is).
    pub headline: String,
    /// Pre-composed bullet detail lines (may be empty).
    pub detail: String,
}

impl Reminder {
    pub fn is_day_of(&self) -> bool {
        self.offset == 0
    }
}

/// Expand the schedule into a time-sorted list of reminders for one channel,
/// anchored to `season` starting on `season_start` at `notify` time-of-day.
/// Only events in `season` are considered.
pub fn compute_reminders(
    season: &str,
    season_start: NaiveDate,
    notify: NaiveTime,
    events: &[Event],
) -> Vec<Reminder> {
    let mut out = Vec::new();
    for ev in events.iter().filter(|e| e.season == season) {
        let headline = ev.headline();
        let detail = ev.detail();
        for off in ev.offsets() {
            let idx = ev.day - off;
            if idx < 0 {
                // A reminder that would fire before the server opened is
                // meaningless; skip it.
                continue;
            }
            let fire_dt = Utc.from_utc_datetime(&(season_start + Duration::days(idx)).and_time(notify));
            out.push(Reminder {
                fire_dt,
                season: ev.season.clone(),
                day: ev.day,
                offset: off,
                headline: headline.clone(),
                detail: detail.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.fire_dt.cmp(&b.fire_dt).then(a.day.cmp(&b.day)));
    out
}

/// Reminders older than this are treated as "missed": acknowledged (the
/// watermark is advanced past them) but NOT sent. This is what stops a
/// mid-season anchor (`setday 40`) or a long downtime from flattening the whole
/// backlog into one giant message on the first tick.
pub const CATCHUP_DAYS: i64 = 1;

/// Outcome of splitting a channel's timeline against its watermark.
pub struct DueBatch {
    /// Reminders fresh enough to actually send (within the catch-up window).
    pub due: Vec<Reminder>,
    /// Fire time of the next future reminder (for `status` / `next_ts`).
    pub next: Option<DateTime<Utc>>,
    /// Newest reminder deliberately skipped as too old, if any. The scheduler
    /// advances the watermark past this so stale history isn't reconsidered
    /// every tick.
    pub skip_to: Option<DateTime<Utc>>,
}

/// Split reminders into what to send now, the next upcoming fire, and how far
/// to fast-forward past deliberately-skipped stale reminders.
///
/// `due` = reminders whose fire time has arrived (`<= now`) but have not
/// already been sent (fire time strictly after `last_sent`). Comparing
/// against `last_sent` makes this safe across restarts and downtime.
pub fn due_and_next(
    reminders: &[Reminder],
    last_sent: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    catchup: Duration,
) -> DueBatch {
    let floor = now - catchup;
    let mut due = Vec::new();
    let mut skip_to: Option<DateTime<Utc>> = None;

    for r in reminders {
        let arrived = r.fire_dt <= now;
        let unsent = last_sent.map_or(true, |ls| r.fire_dt > ls);
        if arrived && unsent {
            if r.fire_dt >= floor {
                due.push(r.clone());
            } else {
                // Too old to send; remember the newest skipped one so the
                // caller can advance the watermark past the whole stale run.
                skip_to = Some(skip_to.map_or(r.fire_dt, |s| s.max(r.fire_dt)));
            }
        }
    }

    let next = reminders.iter().find(|r| r.fire_dt > now).map(|r| r.fire_dt);
    DueBatch { due, next, skip_to }
}

/// Day-within-season from this channel's anchor.
pub fn current_day_in_season(season_start: NaiveDate, now: DateTime<Utc>) -> i64 {
    (now.date_naive() - season_start).num_days()
}

/// Render a single reminder into a Discord-ready message string.
pub fn format_reminder(r: &Reminder) -> String {
    let when = if r.is_day_of() {
        "Today".to_string()
    } else {
        let unit = if r.offset == 1 { "day" } else { "days" };
        format!("In {} {}", r.offset, unit)
    };
    // Event open datetime = fire time + the notice offset we subtracted when
    // scheduling (fire_dt = origin + (day - offset)); recovered here rather
    // than stored on Reminder.
    let event_dt = r.fire_dt + Duration::days(r.offset);
    let mut s = format!(
        "**{} \u{00B7} {} Day {}** ({})\n{}",
        when,
        r.season,
        r.day,
        event_dt.discord(TsStyle::LongDate),
        r.headline,
    );
    if !r.detail.is_empty() {
        s.push('\n');
        s.push_str(&r.detail);
    }
    s
}

/// Combine one or more due reminders into a single channel message.
pub fn format_batch(reminders: &[Reminder]) -> String {
    reminders
        .iter()
        .map(format_reminder)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// An event that hasn't opened yet for an anchored channel, with the precise
/// time until it opens. Borrows the source event so the caller can render
/// title/level/power.
#[derive(Debug, Clone)]
pub struct Upcoming<'a> {
    /// Day-within-season the event occurs on.
    pub day: i64,
    /// Exact UTC datetime the event opens (season origin + day, at notify time).
    pub event_dt: DateTime<Utc>,
    pub event: &'a Event,
}

/// The next `limit` events in `season` that open strictly after `now`, soonest
/// first. Events at the same instant keep a stable order by title. Only the
/// channel's anchored season is considered.
///
/// Unlike the scheduled-reminder path (which labels each reminder by its whole
/// notice-day offset), this is a live "how far away is it" query, so it works
/// in real datetimes: an event opens at `season_start + day` at the channel's
/// `notify` time, and the delta from `now` is kept intact for days+hours
/// display. `notify` is therefore required here.
pub fn upcoming_events<'a>(
    season: &str,
    season_start: NaiveDate,
    notify: NaiveTime,
    now: DateTime<Utc>,
    events: &'a [Event],
    limit: usize,
) -> Vec<Upcoming<'a>> {
    let mut items: Vec<Upcoming<'a>> = events
        .iter()
        .filter(|e| e.season == season)
        .filter_map(|e| {
            let event_dt =
                Utc.from_utc_datetime(&(season_start + Duration::days(e.day)).and_time(notify));
            let until = event_dt - now;
            // Skip events that have already opened (time-aware boundary: an
            // event earlier *today* than `now` is in the past, not upcoming).
            if until <= Duration::zero() {
                return None;
            }
            Some(Upcoming {
                day: e.day,
                event_dt,
                event: e,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        a.event_dt
            .cmp(&b.event_dt)
            .then_with(|| a.event.title.cmp(&b.event.title))
    });
    items.truncate(limit);
    items
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Render one upcoming event: precise time-until + season day + open datetime,
/// then the event's own headline and requirement lines.
pub fn format_upcoming(u: &Upcoming<'_>) -> String {
    let mut s = format!(
        "**In {} \u{00B7} {} Day {}** ({})\n{}",
        u.event_dt.discord(TsStyle::Relative),
        u.event.season,
        u.day,
        u.event_dt.discord(TsStyle::ShortDateTime),
        u.event.headline()
    );
    let detail = u.event.detail();
    if !detail.is_empty() {
        s.push('\n');
        s.push_str(&detail);
    }
    s
}

/// Discord's hard per-message limit, in Unicode code points.
pub const DISCORD_MSG_LIMIT: usize = 2000;

/// Split `body` into pieces each within `limit` code points, breaking on
/// reminder boundaries ("\n\n") first, then lines ("\n"), then spaces, and only
/// hard-splitting mid-token as a last resort. Whole reminders stay intact
/// whenever they fit.
pub fn chunk_message(body: &str, limit: usize) -> Vec<String> {
    let mut pieces = vec![body.to_string()];
    for sep in ["\n\n", "\n", " "] {
        if pieces.iter().all(|p| p.chars().count() <= limit) {
            break;
        }
        pieces = pieces.iter().flat_map(|p| greedy_join(p, sep, limit)).collect();
    }
    pieces.iter().flat_map(|p| hard_split(p, limit)).collect()
}

/// Greedily pack `sep`-separated parts of `s` into <= `limit` chunks, rejoining
/// with `sep`. Parts individually over the limit pass through for a finer pass.
fn greedy_join(s: &str, sep: &str, limit: usize) -> Vec<String> {
    let sep_len = sep.chars().count();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for part in s.split(sep) {
        let part_len = part.chars().count();
        let joined = if cur.is_empty() { part_len } else { cur_len + sep_len + part_len };
        if !cur.is_empty() && joined > limit {
            out.push(std::mem::take(&mut cur));
            cur.push_str(part);
            cur_len = part_len;
        } else if cur.is_empty() {
            cur.push_str(part);
            cur_len = part_len;
        } else {
            cur.push_str(sep);
            cur.push_str(part);
            cur_len += sep_len + part_len;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Last resort: hard-split a separator-less string into <= `limit` pieces.
fn hard_split(s: &str, limit: usize) -> Vec<String> {
    if s.chars().count() <= limit {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut n = 0usize;
    for ch in s.chars() {
        if n == limit {
            out.push(std::mem::take(&mut cur));
            n = 0;
        }
        cur.push(ch);
        n += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, NoticeDays, Power};

    fn ev(season: &str, day: i64, title: &str, kind: &str, notice: NoticeDays) -> Event {
        Event {
            season: season.to_string(),
            kingdom: None,
            day,
            title: title.to_string(),
            kind: kind.to_string(),
            level: None,
            power: None,
            note: None,
            notice_days: notice,
        }
    }

    fn schedule() -> Vec<Event> {
        let mut s3_stage = ev("S3", 28, "Sky Fortress", "stage", NoticeDays::Many(vec![3, 1, 0]));
        s3_stage.level = Some(160);
        s3_stage.power = Some(Power {
            normal: Some("20M".into()),
            hard: Some("26M".into()),
            nightmare: Some("30M".into()),
            purgatory: Some("46M".into()),
            abyss: Some("X".into()),
        });
        vec![
            ev("S3", 1, "Crystalline Spiralwood", "kingdom", NoticeDays::Many(vec![3, 1, 0])),
            s3_stage,
            ev("S3", 15, "Astral Odyssey", "season_map", NoticeDays::Many(vec![3, 1, 0])),
            // Different season: must be excluded when anchored to S3.
            ev("S2", 14, "Warlord's Rest", "stage", NoticeDays::Many(vec![3, 1, 0])),
        ]
    }

    fn start() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()
    }
    fn notify() -> NaiveTime {
        NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    }
    fn dt(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    #[test]
    fn only_current_season_and_correct_dates() {
        let r = compute_reminders("S3", start(), notify(), &schedule());
        // S2 event excluded. With season_start as the day-0 origin, a day-1
        // event with offsets [3,1,0] keeps offset 1 (fires day 0) and 0 (day 1)
        // but drops offset 3 (day -2). So: day1 -> 2, day28 -> 3, day15 -> 3 = 8.
        assert_eq!(r.len(), 8);
        assert!(r.iter().all(|x| x.season == "S3"));
        // day-15 "in 3 days" fires on day 12 -> Mar 13
        let d15_early = r.iter().find(|x| x.day == 15 && x.offset == 3).unwrap();
        assert_eq!(d15_early.fire_dt, dt(2026, 3, 13, 0, 0));
        assert_eq!(r.iter().filter(|x| x.day == 1).count(), 2);
    }

    #[test]
    fn message_includes_level_and_power() {
        let r = compute_reminders("S3", start(), notify(), &schedule());
        let day_of = r.iter().find(|x| x.day == 28 && x.offset == 0).unwrap();
        let msg = format_reminder(day_of);
        assert!(msg.contains("Today \u{00B7} S3 Day 28"));
        assert!(msg.contains("Sky Fortress opens"));
        assert!(msg.contains("Requires Player Lv.160"));
        assert!(msg.contains("Normal 20M"));
        assert!(msg.contains("Purgatory 46M"));
        // Abyss was "X" -> filtered out
        assert!(!msg.contains("Abyss"));
    }

    #[test]
    fn advance_wording() {
        let r = compute_reminders("S3", start(), notify(), &schedule());
        let three = r.iter().find(|x| x.day == 28 && x.offset == 3).unwrap();
        assert!(format_reminder(three).starts_with("**In 3 days \u{00B7} S3 Day 28**"));
    }

    #[test]
    fn day_in_season() {
        assert_eq!(current_day_in_season(start(), dt(2026, 3, 9, 6, 0)), 8);
    }

    #[test]
    fn same_day_already_opened_is_excluded() {
        // day15 opens Mar 16 00:00; at Mar 16 09:00 it's already open -> gone.
        let now = dt(2026, 3, 16, 9, 0);
        let sched = schedule();
        let up = upcoming_events("S3", start(), notify(), now, &sched, 5);
        assert!(up.iter().all(|u| u.day != 15));
        // day28 (Mar 29) is still ahead.
        assert!(up.iter().any(|u| u.day == 28));
    }

    #[test]
    fn upcoming_empty_when_season_finished() {
        // Far past the last S3 event (day 28) => nothing upcoming.
        let now = dt(2026, 6, 1, 0, 0);
        let sched = schedule();
        assert!(upcoming_events("S3", start(), notify(), now, &sched, 5).is_empty());
    }
}
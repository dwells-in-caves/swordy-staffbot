//! Pure scheduling logic for the Sword & Staff reminder bot.
//!
//! No Discord dependency, so it is unit-tested in isolation (see the tests at
//! the bottom).
//!
//! Model: events are authored per-season as (season, day-within-season). A
//! subscribed channel is anchored to ONE season at a time via that season's
//! start date (the in-game "Telescope Date"). For that channel, an event on
//! season-day D fires on `season_start + D days`. Because server merges change
//! how long each season lasts, anchoring per season (rather than one cumulative
//! day count from launch) keeps the timeline accurate; the admin re-anchors
//! when a new season begins.
//!
//! Each event may carry `notice_days` — how many days early to remind
//! (`[3,1,0]` => 3 days before, 1 day before, and on the day). Every
//! (event, offset) pair becomes one Reminder with a concrete UTC fire time.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::events::Event;

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
                // Would fire before the season began; skip.
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

/// Split reminders into `(due now, next upcoming fire time)`.
///
/// `due` = fire time reached (`<= now`) and not already sent (strictly after
/// `last_sent`). Comparing against `last_sent` makes this safe across restarts
/// and downtime: no double-sends, and missed reminders are caught up.
pub fn due_and_next(
    reminders: &[Reminder],
    last_sent: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (Vec<Reminder>, Option<DateTime<Utc>>) {
    let due: Vec<Reminder> = reminders
        .iter()
        .filter(|r| r.fire_dt <= now && last_sent.map_or(true, |ls| r.fire_dt > ls))
        .cloned()
        .collect();
    let next = reminders.iter().find(|r| r.fire_dt > now).map(|r| r.fire_dt);
    (due, next)
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
    let mut s = format!("**{} \u{00B7} {} Day {}**\n{}", when, r.season, r.day, r.headline);
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
    /// Time from `now` until the event opens. Always strictly positive.
    pub until: Duration,
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
                until,
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

/// Render a strictly-positive duration as days + hours: "5 days 7 hours",
/// "7 hours", "1 day", or "under an hour". Hours are floored (countdown-style).
pub fn humanize_until(delta: Duration) -> String {
    let minutes = delta.num_minutes().max(0);
    let days = minutes / (24 * 60);
    let hours = (minutes % (24 * 60)) / 60;
    match (days, hours) {
        (0, 0) => "under an hour".to_string(),
        (0, h) => format!("{h} hour{}", plural(h)),
        (d, 0) => format!("{d} day{}", plural(d)),
        (d, h) => format!("{d} day{} {h} hour{}", plural(d), plural(h)),
    }
}

/// Render one upcoming event: precise time-until + season day + open datetime,
/// then the event's own headline and requirement lines.
pub fn format_upcoming(u: &Upcoming<'_>) -> String {
    let mut s = format!(
        "**In {} \u{00B7} {} Day {}** ({} UTC)\n{}",
        humanize_until(u.until),
        u.event.season,
        u.day,
        u.event_dt.format("%Y-%m-%d %H:%M"),
        u.event.headline()
    );
    let detail = u.event.detail();
    if !detail.is_empty() {
        s.push('\n');
        s.push_str(&detail);
    }
    s
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
    fn no_double_send_and_catchup() {
        let r = compute_reminders("S3", start(), notify(), &schedule());
        // Nothing double-sent when last_sent == now.
        let now = dt(2026, 3, 13, 9, 0);
        let (due, _) = due_and_next(&r, Some(now), now);
        assert!(due.is_empty());
        // Catch up on the Mar 13 batch when we were last active Mar 12.
        let (due2, _) = due_and_next(&r, Some(dt(2026, 3, 12, 0, 0)), now);
        assert!(due2.iter().any(|x| x.day == 15 && x.offset == 3));
    }

    #[test]
    fn day_in_season() {
        assert_eq!(current_day_in_season(start(), dt(2026, 3, 9, 6, 0)), 8);
    }

    #[test]
    fn upcoming_lists_future_events_in_order() {
        // Mar 1 origin, notify 00:00. day15 opens Mar 16 00:00, day28 Mar 29 00:00.
        // "Now" is Mar 13 10:00 UTC.
        let now = dt(2026, 3, 13, 10, 0);
        let sched = schedule(); // outlive the borrowed Upcoming values
        let all = upcoming_events("S3", start(), notify(), now, &sched, 5);
        // day 1 (Mar 2) already opened; day 15 and day 28 remain, soonest first.
        assert_eq!(all.iter().map(|u| u.day).collect::<Vec<_>>(), vec![15, 28]);
        assert!(all.iter().all(|u| u.event.season == "S3")); // S2 excluded
        assert_eq!(all[0].event_dt, dt(2026, 3, 16, 0, 0));
        // Mar 13 10:00 -> Mar 16 00:00 is 2 days 14 hours.
        assert!(format_upcoming(&all[0]).contains("In 2 days 14 hours \u{00B7} S3 Day 15"));

        // Default single-event case respects the limit.
        let one = upcoming_events("S3", start(), notify(), now, &sched, 1);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].day, 15);
    }

    #[test]
    fn humanize_covers_days_hours_and_edges() {
        assert_eq!(humanize_until(Duration::hours(24 * 5 + 7)), "5 days 7 hours");
        assert_eq!(humanize_until(Duration::hours(7)), "7 hours");
        assert_eq!(humanize_until(Duration::hours(1)), "1 hour");
        assert_eq!(humanize_until(Duration::days(3)), "3 days"); // exact -> no hours
        assert_eq!(humanize_until(Duration::days(1)), "1 day");
        assert_eq!(humanize_until(Duration::minutes(30)), "under an hour");
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
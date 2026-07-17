//! Pure scheduling logic for the Sword & Staff reminder bot.
//!
//! This module has NO Discord dependency on purpose: everything here is plain
//! data in / data out, so it can be unit-tested and reasoned about in isolation
//! (see the `#[cfg(test)]` block at the bottom).
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    /// Exact UTC datetime this reminder should be sent.
    pub fire_dt: DateTime<Utc>,
    /// The day-number the event actually occurs on.
    pub event_day: i64,
    /// How many days early this particular reminder is (0 == day-of).
    pub offset: i64,
    pub title: String,
    pub message: String,
}

impl Reminder {
    pub fn is_day_of(&self) -> bool {
        self.offset == 0
    }
}

/// Expand the event schedule into a time-sorted list of concrete reminders for
/// a single channel, given its server open date and notify time-of-day.
pub fn compute_reminders(start: NaiveDate, notify: NaiveTime, events: &[Event]) -> Vec<Reminder> {
    let mut out = Vec::new();
    for ev in events {
        let offsets = if ev.notice_days.is_empty() {
            vec![0]
        } else {
            ev.notice_days.clone()
        };
        for off in offsets {
            let idx = ev.day - off;
            if idx < 0 {
                // A reminder that would fire before the server opened is
                // meaningless; skip it.
                continue;
            }
            let fire_date = start + Duration::days(idx);
            let fire_dt = Utc.from_utc_datetime(&fire_date.and_time(notify));
            out.push(Reminder {
                fire_dt,
                event_day: ev.day,
                offset: off,
                title: ev.title.clone(),
                message: ev.message.clone(),
            });
        }
    }
    out.sort_by_key(|r| r.fire_dt);
    out
}

/// Split reminders into `(due now, next upcoming fire time)`.
///
/// `due` = reminders whose fire time has arrived (`<= now`) but which we have
/// not already sent (fire time strictly after `last_sent`). Comparing against
/// `last_sent` is what makes this safe across restarts and downtime
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
    let next = reminders
        .iter()
        .find(|r| r.fire_dt > now)
        .map(|r| r.fire_dt);
    (due, next)
}

/// How many days since the server opened, from this channel's perspective.
pub fn current_day_number(start: NaiveDate, now: DateTime<Utc>) -> i64 {
    (now.date_naive() - start).num_days()
}

/// Render a single reminder into a Discord-ready message string.
pub fn format_reminder(r: &Reminder) -> String {
    if r.is_day_of() {
        format!("\u{1F4C5} **Today \u{2014} Day {}: {}**\n{}", r.event_day, r.title, r.message)
    } else {
        let unit = if r.offset == 1 { "day" } else { "days" };
        format!(
            "\u{23F3} **In {} {} (Day {}): {}**\n{}",
            r.offset, unit, r.event_day, r.title, r.message
        )
    }
}

/// Combine one or more due reminders into a single channel message.
pub fn format_batch(reminders: &[Reminder]) -> String {
    reminders
        .iter()
        .map(format_reminder)
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;

    fn ev(day: i64, title: &str, notice: Vec<i64>) -> Event {
        Event {
            day,
            title: title.to_string(),
            message: format!("{title} body"),
            notice_days: notice,
        }
    }

    fn events() -> Vec<Event> {
        vec![
            ev(5, "Guild War", vec![2, 0]),
            ev(10, "Tower Rush", vec![0]),
            ev(0, "Launch", vec![]), // empty -> treated as [0]
        ]
    }

    fn start() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
    }

    fn notify() -> NaiveTime {
        NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    }

    fn dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn expansion_and_sort() {
        let r = compute_reminders(start(), notify(), &events());
        // day5 -> 2, day10 -> 1, day0 -> 1 => 4
        assert_eq!(r.len(), 4);
        // sorted ascending, first fire is the day-0 launch
        assert!(r.windows(2).all(|w| w[0].fire_dt <= w[1].fire_dt));
        assert_eq!(r[0].fire_dt, dt(2026, 1, 1, 0, 0));
    }

    #[test]
    fn advance_offsets_map_to_right_dates() {
        let r = compute_reminders(start(), notify(), &events());
        let find = |day: i64, off: i64| {
            r.iter()
                .find(|x| x.event_day == day && x.offset == off)
                .unwrap()
                .fire_dt
        };
        // Guild War day 5, "2 days early" -> day 3 -> Jan 4
        assert_eq!(find(5, 2), dt(2026, 1, 4, 0, 0));
        // day-of -> day 5 -> Jan 6
        assert_eq!(find(5, 0), dt(2026, 1, 6, 0, 0));
    }

    #[test]
    fn due_and_next_basic() {
        let r = compute_reminders(start(), notify(), &events());
        let now = dt(2026, 1, 4, 9, 0);
        let (due, next) = due_and_next(&r, None, now);
        let mut got: Vec<(i64, i64)> = due.iter().map(|d| (d.event_day, d.offset)).collect();
        got.sort();
        assert_eq!(got, vec![(0, 0), (5, 2)]);
        assert_eq!(next, Some(dt(2026, 1, 6, 0, 0)));
    }

    #[test]
    fn no_double_send_across_restart() {
        let r = compute_reminders(start(), notify(), &events());
        let now = dt(2026, 1, 4, 9, 0);
        let (due, _) = due_and_next(&r, Some(now), now);
        assert!(due.is_empty());
    }

    #[test]
    fn downtime_catchup() {
        let r = compute_reminders(start(), notify(), &events());
        let last_sent = Some(dt(2026, 1, 3, 0, 0));
        let now = dt(2026, 1, 7, 12, 0);
        let (due, next) = due_and_next(&r, last_sent, now);
        let mut got: Vec<(i64, i64)> = due.iter().map(|d| (d.event_day, d.offset)).collect();
        got.sort();
        // Missed the Jan 4 (day5 -2) and Jan 6 (day5 day-of); NOT the Jan 1 launch.
        assert_eq!(got, vec![(5, 0), (5, 2)]);
        assert_eq!(next, Some(dt(2026, 1, 11, 0, 0))); // day-10 event
    }

    #[test]
    fn day_number() {
        let now = dt(2026, 1, 8, 6, 0);
        assert_eq!(current_day_number(start(), now), 7);
    }
}

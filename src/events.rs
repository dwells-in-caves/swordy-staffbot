//! Loads the event schedule from a JSON file.
//!
//! The schedule is data, not code, so it can be edited without recompiling.
//! Each event:
//!   day          (int, required)  day number since server open, 0 = launch day
//!   title        (str, required)  short name shown in the reminder header
//!   message      (str, required)  the "what to do / what to save" body
//!   notice_days  (list<int> | int, optional) how many days early to remind;
//!                defaults to [0] (remind on the day itself only)

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// Normalized event used throughout the bot. `notice_days` is always a Vec;
/// an empty Vec is treated as `[0]` by the scheduler.
#[derive(Debug, Clone)]
pub struct Event {
    pub day: i64,
    pub title: String,
    pub message: String,
    pub notice_days: Vec<i64>,
}

/// Raw shape as it appears in JSON, before normalization.
#[derive(Debug, Deserialize)]
struct RawEvent {
    day: i64,
    title: String,
    message: String,
    #[serde(default)]
    notice_days: NoticeDays,
}

/// `notice_days` may be omitted, a single int, or a list of ints.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NoticeDays {
    One(i64),
    Many(Vec<i64>),
}

impl Default for NoticeDays {
    fn default() -> Self {
        NoticeDays::Many(vec![0])
    }
}

impl NoticeDays {
    fn into_vec(self) -> Vec<i64> {
        match self {
            NoticeDays::One(n) => vec![n],
            NoticeDays::Many(v) => {
                if v.is_empty() {
                    vec![0]
                } else {
                    v
                }
            }
        }
    }
}

pub fn load_events(path: impl AsRef<Path>) -> Result<Vec<Event>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading events file {}", path.display()))?;
    let raw: Vec<RawEvent> =
        serde_json::from_str(&text).with_context(|| "parsing events JSON")?;

    let mut events = Vec::with_capacity(raw.len());
    for (i, r) in raw.into_iter().enumerate() {
        if r.day < 0 {
            bail!("event #{i} has a negative day");
        }
        let notice_days = r.notice_days.into_vec();
        if notice_days.iter().any(|&n| n < 0) {
            bail!("event #{i} has a negative notice_days value");
        }
        events.push(Event {
            day: r.day,
            title: r.title,
            message: r.message,
            notice_days,
        });
    }
    Ok(events)
}

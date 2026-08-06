//! Loads and models the Sword & Staff event schedule (events.json).
//!
//! The schedule is data, not code, so it can be edited without recompiling.
//! Each event is authored as (season, day-within-season) matching the source
//! sheet, plus optional level/power/note used to compose the reminder text.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// Per-difficulty combat-power requirements. Any tier may be absent, "X"
/// (locked) or "-" (no requirement); those are filtered out on display.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Power {
    #[serde(default)]
    pub normal: Option<String>,
    #[serde(default)]
    pub hard: Option<String>,
    #[serde(default)]
    pub nightmare: Option<String>,
    #[serde(default)]
    pub purgatory: Option<String>,
    #[serde(default)]
    pub abyss: Option<String>,
}

impl Power {
    /// The tiers worth showing, in difficulty order, skipping X/-/empty.
    pub fn tiers(&self) -> Vec<(&'static str, String)> {
        let raw = [
            ("Normal", &self.normal),
            ("Hard", &self.hard),
            ("Nightmare", &self.nightmare),
            ("Purgatory", &self.purgatory),
            ("Abyss", &self.abyss),
        ];
        let mut out = Vec::new();
        for (label, val) in raw {
            if let Some(v) = val {
                let v = v.trim();
                if !v.is_empty() && v != "X" && v != "-" {
                    out.push((label, v.to_string()));
                }
            }
        }
        out
    }
}

/// `notice_days` may be omitted, a single int, or a list of ints. Omitted
/// defaults to the "prepare ahead" pattern used for most timed events.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NoticeDays {
    One(i64),
    Many(Vec<i64>),
}

impl Default for NoticeDays {
    fn default() -> Self {
        NoticeDays::Many(vec![3, 1, 0])
    }
}

impl NoticeDays {
    fn into_vec(self) -> Vec<i64> {
        let v = match self {
            NoticeDays::One(n) => vec![n],
            NoticeDays::Many(v) => v,
        };
        if v.is_empty() {
            vec![0]
        } else {
            v
        }
    }
}

fn default_kind() -> String {
    "event".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub season: String,
    #[serde(default)]
    pub kingdom: Option<String>,
    pub day: i64,
    pub title: String,
    #[serde(rename = "type", default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub level: Option<i64>,
    #[serde(default)]
    pub power: Option<Power>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub notice_days: NoticeDays,
}

impl Event {
    pub fn offsets(&self) -> Vec<i64> {
        self.notice_days.clone().into_vec()
    }

    /// The bolded first line describing what the event is.
    pub fn headline(&self) -> String {
        match self.kind.as_str() {
            "kingdom" => format!(
                "\u{1F3F0} {} opens",
                self.kingdom.clone().unwrap_or_else(|| self.title.clone())
            ),
            "stage" => format!("\u{2694}\u{FE0F} {} opens", self.title),
            "season_map" => format!("\u{1F5FA}\u{FE0F} {} (Season Map)", self.title),
            "relic" => format!("\u{1F3B2} {}", self.title),
            "fantomon" => format!("\u{1F43E} {}", self.title),
            "class" => format!("\u{1F393} {} available", self.title),
            "unlock" => format!("\u{1F513} {} available", self.title),
            _ => self.title.clone(),
        }
    }

    /// The bulleted requirement/detail lines (may be empty).
    pub fn detail(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if self.kind == "kingdom" {
            lines.push(format!("First stage: {}", self.title));
        }
        if let Some(lv) = self.level {
            lines.push(format!("Requires Player Lv.{lv}"));
        }
        if let Some(p) = &self.power {
            let tiers = p.tiers();
            if !tiers.is_empty() {
                let joined = tiers
                    .iter()
                    .map(|(l, v)| format!("{l} {v}"))
                    .collect::<Vec<_>>()
                    .join(" \u{00B7} ");
                lines.push(format!("Power to enter \u{2014} {joined}"));
            }
        }
        if let Some(n) = &self.note {
            if !n.trim().is_empty() {
                lines.push(n.clone());
            }
        }
        lines
            .iter()
            .map(|l| format!("\u{2022} {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Top-level file shape: a metadata string plus the events array.
#[derive(Debug, Deserialize)]
struct Schedule {
    #[serde(default)]
    #[allow(dead_code)]
    _source: String,
    events: Vec<Event>,
}

pub fn load_events(path: impl AsRef<Path>) -> Result<Vec<Event>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading events file {}", path.display()))?;
    let schedule: Schedule =
        serde_json::from_str(&text).with_context(|| "parsing events JSON")?;

    for (i, ev) in schedule.events.iter().enumerate() {
        if ev.day < 0 {
            bail!("event #{i} ({}) has a negative day", ev.title);
        }
        if ev.offsets().iter().any(|&n| n < 0) {
            bail!("event #{i} ({}) has a negative notice_days value", ev.title);
        }
    }
    Ok(schedule.events)
}

/// Distinct season labels present in the schedule, in first-seen order.
pub fn seasons(events: &[Event]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ev in events {
        if !out.contains(&ev.season) {
            out.push(ev.season.clone());
        }
    }
    out
}

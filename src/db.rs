//! SQLite persistence for subscribed channels (rusqlite, bundled SQLite).
//!
//! One row per channel that has ever interacted with the bot. `subscribed`
//! toggles whether reminders are sent; unsubscribing keeps the season anchor.
//!
//! A channel is anchored to one season at a time: `season` (e.g. "S3") plus
//! `start_date` = that season's start (the in-game Telescope Date). When a new
//! season begins the admin re-anchors, which also resets send state.
//!
//! Datetimes are RFC3339, dates ISO, times "HH:MM". Discord snowflake IDs are
//! u64 but fit SQLite's i64 for the foreseeable future, so stored as i64.

use anyhow::{Context as _, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct ChannelRow {
    pub channel_id: i64,
    pub guild_id: Option<i64>,
    pub season: Option<String>,
    pub start_date: Option<String>,
    pub notify_time: String,
    pub last_sent_ts: Option<String>,
    pub next_ts: Option<String>,
    pub subscribed: bool,
}

impl ChannelRow {
    pub fn start_date_parsed(&self) -> Option<NaiveDate> {
        self.start_date
            .as_deref()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
    }
    pub fn notify_time_parsed(&self) -> NaiveTime {
        NaiveTime::parse_from_str(&self.notify_time, "%H:%M")
            .unwrap_or_else(|_| NaiveTime::from_hms_opt(0, 0, 0).unwrap())
    }
    pub fn last_sent_parsed(&self) -> Option<DateTime<Utc>> {
        parse_dt(self.last_sent_ts.as_deref())
    }
}

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS channels (
            channel_id   INTEGER PRIMARY KEY,
            guild_id     INTEGER,
            season       TEXT,
            start_date   TEXT,
            notify_time  TEXT NOT NULL DEFAULT '00:00',
            last_sent_ts TEXT,
            next_ts      TEXT,
            subscribed   INTEGER NOT NULL DEFAULT 1,
            created_at   TEXT NOT NULL
        );
        "#,
    )
    .context("creating channels table")?;
    Ok(())
}

// --- writes -------------------------------------------------------------

pub fn upsert_subscription(conn: &Connection, channel_id: i64, guild_id: Option<i64>) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        INSERT INTO channels (channel_id, guild_id, subscribed, created_at)
        VALUES (?1, ?2, 1, ?3)
        ON CONFLICT(channel_id) DO UPDATE SET
            subscribed = 1,
            guild_id = excluded.guild_id
        "#,
        params![channel_id, guild_id, now],
    )?;
    Ok(())
}

pub fn set_subscribed(conn: &Connection, channel_id: i64, subscribed: bool) -> Result<()> {
    conn.execute(
        "UPDATE channels SET subscribed = ?1 WHERE channel_id = ?2",
        params![subscribed as i64, channel_id],
    )?;
    Ok(())
}

/// Anchor a channel to a season and its start date. Resets send state so the
/// new season's reminders fire cleanly from the anchor.
pub fn set_season_anchor(
    conn: &Connection,
    channel_id: i64,
    guild_id: Option<i64>,
    season: &str,
    start: NaiveDate,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        INSERT INTO channels (channel_id, guild_id, season, start_date, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(channel_id) DO UPDATE SET
            season = excluded.season,
            start_date = excluded.start_date,
            last_sent_ts = NULL,
            next_ts = NULL
        "#,
        params![channel_id, guild_id, season, start.format("%Y-%m-%d").to_string(), now],
    )?;
    Ok(())
}

pub fn set_notify_time(conn: &Connection, channel_id: i64, notify: NaiveTime) -> Result<()> {
    conn.execute(
        "UPDATE channels SET notify_time = ?1 WHERE channel_id = ?2",
        params![notify.format("%H:%M").to_string(), channel_id],
    )?;
    Ok(())
}

pub fn record_sent(
    conn: &Connection,
    channel_id: i64,
    last_sent: DateTime<Utc>,
    next_ts: Option<DateTime<Utc>>,
) -> Result<()> {
    conn.execute(
        "UPDATE channels SET last_sent_ts = ?1, next_ts = ?2 WHERE channel_id = ?3",
        params![last_sent.to_rfc3339(), next_ts.map(|d| d.to_rfc3339()), channel_id],
    )?;
    Ok(())
}

pub fn set_next_ts(conn: &Connection, channel_id: i64, next_ts: Option<DateTime<Utc>>) -> Result<()> {
    conn.execute(
        "UPDATE channels SET next_ts = ?1 WHERE channel_id = ?2",
        params![next_ts.map(|d| d.to_rfc3339()), channel_id],
    )?;
    Ok(())
}

// --- reads --------------------------------------------------------------

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<ChannelRow> {
    Ok(ChannelRow {
        channel_id: r.get("channel_id")?,
        guild_id: r.get("guild_id")?,
        season: r.get("season")?,
        start_date: r.get("start_date")?,
        notify_time: r.get("notify_time")?,
        last_sent_ts: r.get("last_sent_ts")?,
        next_ts: r.get("next_ts")?,
        subscribed: r.get::<_, i64>("subscribed")? != 0,
    })
}

pub fn get_channel(conn: &Connection, channel_id: i64) -> Result<Option<ChannelRow>> {
    let row = conn
        .query_row(
            "SELECT * FROM channels WHERE channel_id = ?1",
            params![channel_id],
            row_from,
        )
        .optional()?;
    Ok(row)
}

/// Channels that are subscribed AND anchored (season + start date set).
pub fn get_subscribed_channels(conn: &Connection) -> Result<Vec<ChannelRow>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM channels
         WHERE subscribed = 1 AND season IS NOT NULL AND start_date IS NOT NULL",
    )?;
    let rows = stmt.query_map([], row_from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// --- helpers ------------------------------------------------------------

pub fn parse_dt(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        conn
    }

    #[test]
    fn anchor_required_for_scheduling() {
        let conn = mem();
        upsert_subscription(&conn, 123, Some(999)).unwrap();
        // Subscribed but not anchored -> not scheduled.
        assert!(get_subscribed_channels(&conn).unwrap().is_empty());

        let start = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        set_season_anchor(&conn, 123, Some(999), "S3", start).unwrap();

        let due = get_subscribed_channels(&conn).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].season.as_deref(), Some("S3"));
        assert_eq!(due[0].start_date_parsed(), Some(start));
        assert_eq!(due[0].notify_time, "00:00");
    }

    #[test]
    fn re_anchoring_resets_send_state() {
        let conn = mem();
        let s2_start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        set_season_anchor(&conn, 7, None, "S2", s2_start).unwrap();
        upsert_subscription(&conn, 7, None).unwrap();
        record_sent(&conn, 7, Utc::now(), None).unwrap();
        assert!(get_channel(&conn, 7).unwrap().unwrap().last_sent_ts.is_some());

        // Moving to S3 clears last_sent so the new season fires from scratch.
        let s3_start = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
        set_season_anchor(&conn, 7, None, "S3", s3_start).unwrap();
        let row = get_channel(&conn, 7).unwrap().unwrap();
        assert_eq!(row.season.as_deref(), Some("S3"));
        assert!(row.last_sent_ts.is_none());
    }

    #[test]
    fn unsubscribe_keeps_anchor() {
        let conn = mem();
        let start = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        set_season_anchor(&conn, 5, None, "S1", start).unwrap();
        upsert_subscription(&conn, 5, None).unwrap();
        set_subscribed(&conn, 5, false).unwrap();
        assert!(get_subscribed_channels(&conn).unwrap().is_empty());
        let row = get_channel(&conn, 5).unwrap().unwrap();
        assert!(!row.subscribed);
        assert_eq!(row.season.as_deref(), Some("S1"));
    }
}

//! SQLite persistence for subscribed channels (rusqlite, bundled SQLite).
//!
//! One row per channel that has ever interacted with the bot. `subscribed`
//! toggles whether reminders are actually sent, so unsubscribing keeps the
//! start-date config around in case the channel re-subscribes.
//!
//! Datetimes/dates are stored as strings (RFC3339 for datetimes, ISO for
//! dates, "HH:MM" for times) for readability and easy debugging.
//!
//! Discord snowflake IDs are u64 but comfortably fit in SQLite's i64 for the
//! foreseeable future, so they are stored as i64.

use anyhow::{Context as _, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct ChannelRow {
    pub channel_id: i64,
    pub guild_id: Option<i64>,
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

pub fn set_start_date(
    conn: &Connection,
    channel_id: i64,
    guild_id: Option<i64>,
    start: NaiveDate,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        r#"
        INSERT INTO channels (channel_id, guild_id, start_date, created_at)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(channel_id) DO UPDATE SET start_date = excluded.start_date
        "#,
        params![channel_id, guild_id, start.format("%Y-%m-%d").to_string(), now],
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
        params![
            last_sent.to_rfc3339(),
            next_ts.map(|d| d.to_rfc3339()),
            channel_id
        ],
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

/// All channels that are subscribed AND have a start date set.
pub fn get_subscribed_channels(conn: &Connection) -> Result<Vec<ChannelRow>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM channels WHERE subscribed = 1 AND start_date IS NOT NULL",
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
    fn subscribe_then_set_start_roundtrips() {
        let conn = mem();
        upsert_subscription(&conn, 123, Some(999)).unwrap();

        // Subscribed but no start date yet -> not returned by the scheduler query.
        assert!(get_subscribed_channels(&conn).unwrap().is_empty());

        let start = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        set_start_date(&conn, 123, Some(999), start).unwrap();

        let due = get_subscribed_channels(&conn).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].channel_id, 123);
        assert_eq!(due[0].start_date_parsed(), Some(start));
        assert_eq!(due[0].notify_time, "00:00"); // default
    }

    #[test]
    fn unsubscribe_hides_but_keeps_start() {
        let conn = mem();
        let start = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        set_start_date(&conn, 7, None, start).unwrap();
        upsert_subscription(&conn, 7, None).unwrap();
        assert_eq!(get_subscribed_channels(&conn).unwrap().len(), 1);

        set_subscribed(&conn, 7, false).unwrap();
        assert!(get_subscribed_channels(&conn).unwrap().is_empty());

        // Start date is retained for a future re-subscribe.
        let row = get_channel(&conn, 7).unwrap().unwrap();
        assert!(!row.subscribed);
        assert_eq!(row.start_date_parsed(), Some(start));
    }

    #[test]
    fn record_sent_persists_timestamps() {
        let conn = mem();
        set_start_date(&conn, 5, None, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()).unwrap();
        upsert_subscription(&conn, 5, None).unwrap();

        let sent = Utc::now();
        let next = sent + chrono::Duration::days(2);
        record_sent(&conn, 5, sent, Some(next)).unwrap();

        let row = get_channel(&conn, 5).unwrap().unwrap();
        assert!(row.last_sent_parsed().is_some());
        assert_eq!(parse_dt(row.next_ts.as_deref()).unwrap(), next);
    }
}

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

use crate::conquest::Slot;

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
    // Conquest: per-slot fire time ("HH:MM" UTC), the pinged role id, and a
    // per-day watermark ("YYYY-MM-DD") so each slot fires at most once a day.
    pub cq_early_time: Option<String>,
    pub cq_late_time: Option<String>,
    pub cq_early_role_id: Option<i64>,
    pub cq_late_role_id: Option<i64>,
    pub cq_early_last_sent: Option<String>,
    pub cq_late_last_sent: Option<String>,
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

    /// This slot's configured daily fire time (UTC), if set.
    pub fn cq_time(&self, slot: Slot) -> Option<NaiveTime> {
        let raw = match slot {
            Slot::Early => &self.cq_early_time,
            Slot::Late => &self.cq_late_time,
        };
        raw.as_deref()
            .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok())
    }

    /// This slot's pinged role id, if a role has been ensured for it.
    pub fn cq_role_id(&self, slot: Slot) -> Option<i64> {
        match slot {
            Slot::Early => self.cq_early_role_id,
            Slot::Late => self.cq_late_role_id,
        }
    }

    /// The date this slot last fired (UTC), if ever. Used as a once-per-day guard.
    pub fn cq_last_sent(&self, slot: Slot) -> Option<NaiveDate> {
        let raw = match slot {
            Slot::Early => &self.cq_early_last_sent,
            Slot::Late => &self.cq_late_last_sent,
        };
        raw.as_deref()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
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

    // Lightweight migration for the conquest columns. `CREATE TABLE IF NOT
    // EXISTS` above never alters an existing table, so add these idempotently
    // for databases created before conquest support.
    for (col, decl) in [
        ("cq_early_time", "TEXT"),
        ("cq_late_time", "TEXT"),
        ("cq_early_role_id", "INTEGER"),
        ("cq_late_role_id", "INTEGER"),
        ("cq_early_last_sent", "TEXT"),
        ("cq_late_last_sent", "TEXT"),
    ] {
        add_column_if_missing(conn, "channels", col, decl)?;
    }
    Ok(())
}

/// Add `column` to `table` only if it isn't already present. SQLite's
/// `ALTER TABLE ADD COLUMN` errors if the column exists, so we check
/// `PRAGMA table_info` first. `table`/`column`/`decl` are compile-time
/// constants (never user input), so interpolating them is safe.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))? // column 1 = name
        .collect::<rusqlite::Result<_>>()?;
    if !existing.iter().any(|c| c == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )
        .with_context(|| format!("adding column {column} to {table}"))?;
    }
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

// --- conquest writes ----------------------------------------------------
pub fn set_cq_time(
    conn: &Connection,
    channel_id: i64,
    slot: Slot,
    time: Option<NaiveTime>,
) -> Result<usize> {
    let sql = format!("UPDATE channels SET {} = ?1 WHERE channel_id = ?2", slot.time_col());
    let changed = conn.execute(
        &sql,
        params![time.map(|t| t.format("%H:%M").to_string()), channel_id],
    )?;
    Ok(changed)
}

/// Store the resolved role id for a slot. Best-effort: 0 rows if the channel
/// row doesn't exist yet.
pub fn set_cq_role_id(conn: &Connection, channel_id: i64, slot: Slot, role_id: i64) -> Result<usize> {
    let sql = format!("UPDATE channels SET {} = ?1 WHERE channel_id = ?2", slot.role_col());
    let changed = conn.execute(&sql, params![role_id, channel_id])?;
    Ok(changed)
}

/// Stamp the per-day watermark for a slot after a successful ping.
pub fn record_cq_sent(conn: &Connection, channel_id: i64, slot: Slot, date: NaiveDate) -> Result<()> {
    let sql = format!(
        "UPDATE channels SET {} = ?1 WHERE channel_id = ?2",
        slot.last_sent_col()
    );
    conn.execute(&sql, params![date.format("%Y-%m-%d").to_string(), channel_id])?;
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
        cq_early_time: r.get("cq_early_time")?,
        cq_late_time: r.get("cq_late_time")?,
        cq_early_role_id: r.get("cq_early_role_id")?,
        cq_late_role_id: r.get("cq_late_role_id")?,
        cq_early_last_sent: r.get("cq_early_last_sent")?,
        cq_late_last_sent: r.get("cq_late_last_sent")?,
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

/// Subscribed channels with at least one conquest slot configured. Independent
/// of the season anchor: a channel can get conquest pings without a season set,
/// as long as it has run `/subscribe`.
pub fn get_cq_channels(conn: &Connection) -> Result<Vec<ChannelRow>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM channels
         WHERE subscribed = 1
           AND (cq_early_time IS NOT NULL OR cq_late_time IS NOT NULL)",
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

    #[test]
    fn migration_adds_cq_columns_to_pre_conquest_db() {
        let conn = Connection::open_in_memory().unwrap();
        // Simulate an existing DB created before conquest support: the old
        // channels schema with none of the cq_* columns.
        conn.execute_batch(
            r#"CREATE TABLE channels (
                channel_id   INTEGER PRIMARY KEY,
                guild_id     INTEGER,
                season       TEXT,
                start_date   TEXT,
                notify_time  TEXT NOT NULL DEFAULT '00:00',
                last_sent_ts TEXT,
                next_ts      TEXT,
                subscribed   INTEGER NOT NULL DEFAULT 1,
                created_at   TEXT NOT NULL
            );"#,
        )
        .unwrap();
        // First init adds the columns; a second run must be a no-op (idempotent).
        init(&conn).unwrap();
        init(&conn).unwrap();
        // Rows now read back cleanly, with all cq fields defaulting to None.
        upsert_subscription(&conn, 1, Some(2)).unwrap();
        let row = get_channel(&conn, 1).unwrap().unwrap();
        assert!(row.cq_time(Slot::Early).is_none());
        assert!(row.cq_role_id(Slot::Late).is_none());
        assert!(row.cq_last_sent(Slot::Early).is_none());
    }

    #[test]
    fn cq_time_role_and_watermark_roundtrip() {
        let conn = mem();
        upsert_subscription(&conn, 10, Some(99)).unwrap();

        let t = NaiveTime::from_hms_opt(12, 30, 0).unwrap();
        assert_eq!(set_cq_time(&conn, 10, Slot::Early, Some(t)).unwrap(), 1);
        set_cq_role_id(&conn, 10, Slot::Early, 555).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        record_cq_sent(&conn, 10, Slot::Early, d).unwrap();

        let row = get_channel(&conn, 10).unwrap().unwrap();
        assert_eq!(row.cq_time(Slot::Early), Some(t));
        assert_eq!(row.cq_role_id(Slot::Early), Some(555));
        assert_eq!(row.cq_last_sent(Slot::Early), Some(d));
        // The other slot is independent and stays empty.
        assert!(row.cq_time(Slot::Late).is_none());

        // Clearing a slot's time sets it back to NULL.
        assert_eq!(set_cq_time(&conn, 10, Slot::Early, None).unwrap(), 1);
        assert!(get_channel(&conn, 10).unwrap().unwrap().cq_time(Slot::Early).is_none());
    }

    #[test]
    fn get_cq_channels_requires_subscribe_and_a_time() {
        let conn = mem();
        let nine = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let nine_pm = NaiveTime::from_hms_opt(21, 0, 0).unwrap();

        // Has a time but unsubscribed -> excluded.
        upsert_subscription(&conn, 1, None).unwrap();
        set_cq_time(&conn, 1, Slot::Early, Some(nine)).unwrap();
        set_subscribed(&conn, 1, false).unwrap();
        // Subscribed but no CQ time -> excluded.
        upsert_subscription(&conn, 2, None).unwrap();
        // Subscribed with a CQ time -> included (no season anchor needed).
        upsert_subscription(&conn, 3, None).unwrap();
        set_cq_time(&conn, 3, Slot::Late, Some(nine_pm)).unwrap();

        let ids: Vec<i64> = get_cq_channels(&conn)
            .unwrap()
            .iter()
            .map(|r| r.channel_id)
            .collect();
        assert_eq!(ids, vec![3]);
    }
}

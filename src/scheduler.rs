//! Background scheduler: a tokio task that wakes up on an interval and, for
//! every subscribed channel, figures out which reminders are due and sends them.
//!
//! Flow each tick:
//!   1. Load the (small) event schedule once.
//!   2. For each subscribed channel with a start date:
//!        - Expand its personal reminder timeline (compute_reminders)
//!        - Find reminders due since the last sending (due_and_next)
//!        - Post a single combined message if any are due
//!        - Persist last_sent_ts and next_ts so reminders are never sent
//!          twice and so `status` can show the next reminder even across
//!          restarts.
//!
//!
//! Locking discipline mirrors commands.rs: the std `Mutex` guard is only ever
//! held inside a sync `{ ... }` block, never across an `.await`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{NaiveTime, Utc};
use poise::serenity_prelude as serenity;
use rusqlite::Connection;

use crate::db::{self, ChannelRow};
use crate::events::load_events;
use crate::reminders::{compute_reminders, due_and_next, format_batch};

type Db = Arc<Mutex<Connection>>;

/// Long-running loop. Spawn this with `tokio::spawn` once the bot is ready.
pub async fn run(
    ctx: serenity::Context,
    db: Db,
    events_path: String,
    interval_secs: u64,
    _default_notify: NaiveTime,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
    tracing::info!("scheduler started (interval {interval_secs}s)");
    loop {
        ticker.tick().await;
        if let Err(e) = tick(&ctx, &db, &events_path).await {
            tracing::error!("scheduler tick error: {e}");
        }
    }
}

async fn tick(ctx: &serenity::Context, db: &Db, events_path: &str) -> anyhow::Result<()> {
    let now = Utc::now();

    let events = match load_events(events_path) {
        Ok(e) => e,
        Err(e) => {
            // A bad manual edit to events.json shouldn't kill the loop.
            tracing::warn!("failed to load events: {e}");
            return Ok(());
        }
    };

    let channels: Vec<ChannelRow> = {
        let conn = db.lock().unwrap();
        db::get_subscribed_channels(&conn)?
    };

    for row in channels {
        let Some(start) = row.start_date_parsed() else {
            continue;
        };
        let reminders = compute_reminders(start, row.notify_time_parsed(), &events);
        let (due, next) = due_and_next(&reminders, row.last_sent_parsed(), now);

        if due.is_empty() {
            // Keep the cached next timestamp fresh even when nothing fires.
            let next_str = next.map(|d| d.to_rfc3339());
            if row.next_ts != next_str {
                let conn = db.lock().unwrap();
                db::set_next_ts(&conn, row.channel_id, next)?;
            }
            continue;
        }

        let body = format_batch(&due);
        let channel = serenity::ChannelId::new(row.channel_id as u64);
        match channel.say(&ctx.http, body).await {
            Ok(_) => {
                let conn = db.lock().unwrap();
                db::record_sent(&conn, row.channel_id, now, next)?;
            }
            Err(e) => {
                tracing::warn!("send failed for channel {}: {e}", row.channel_id);
            }
        }
    }

    Ok(())
}

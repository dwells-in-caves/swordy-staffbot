//! Background loop that checks each subscribed+anchored channel and sends any
//! due reminders. Runs every `check_interval` seconds.
//!
//! For each channel we recompute reminders for its anchored season from
//! (season, start_date, notify_time), then send whatever is due since
//! `last_sent`. `record_sent` advances the watermark so nothing repeats and
//! missed reminders are caught up after downtime.
//!
//! DB guard is always dropped before any `.await` (Discord send / sleep).

use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::Utc;
use rusqlite::Connection;
use serenity::all::{ChannelId, Http};
use tracing::{error, info, warn};

use crate::db::{self, ChannelRow};
use crate::events::load_events;
use crate::reminders::{compute_reminders, due_and_next, format_batch};

pub async fn run(
    http: Arc<Http>,
    db: Arc<Mutex<Connection>>,
    events_path: String,
    check_interval: u64,
) {
    info!(check_interval, "reminder scheduler started");
    let mut ticker = tokio::time::interval(StdDuration::from_secs(check_interval.max(5)));
    loop {
        ticker.tick().await;
        if let Err(e) = tick(&http, &db, &events_path).await {
            error!(error = %e, "scheduler tick failed");
        }
    }
}

async fn tick(
    http: &Arc<Http>,
    db: &Arc<Mutex<Connection>>,
    events_path: &str,
) -> anyhow::Result<()> {
    let events = match load_events(events_path) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "could not load events this tick");
            return Ok(());
        }
    };

    // Snapshot rows, then release the lock before awaiting on Discord.
    let rows: Vec<ChannelRow> = {
        let conn = db.lock().unwrap();
        db::get_subscribed_channels(&conn)?
    };

    let now = Utc::now();
    for row in rows {
        let (Some(season), Some(start)) = (row.season.clone(), row.start_date_parsed()) else {
            continue; // query guarantees these, but stay defensive
        };
        let reminders =
            compute_reminders(&season, start, row.notify_time_parsed(), &events);
        let (due, next) = due_and_next(&reminders, row.last_sent_parsed(), now);

        if due.is_empty() {
            // Keep the stored "next" fresh for status output.
            let conn = db.lock().unwrap();
            let _ = db::set_next_ts(&conn, row.channel_id, next);
            continue;
        }

        let body = format_batch(&due);
        let channel = ChannelId::new(row.channel_id as u64);
        match channel.say(http, body).await {
            Ok(_) => {
                let latest = due.iter().map(|r| r.fire_dt).max().unwrap_or(now);
                let conn = db.lock().unwrap();
                if let Err(e) = db::record_sent(&conn, row.channel_id, latest, next) {
                    error!(error = %e, channel = row.channel_id, "failed to record send");
                }
                info!(channel = row.channel_id, count = due.len(), "sent reminders");
            }
            Err(e) => {
                // Don't advance the watermark; we'll retry next tick.
                warn!(error = %e, channel = row.channel_id, "failed to send reminders");
            }
        }
    }
    Ok(())
}

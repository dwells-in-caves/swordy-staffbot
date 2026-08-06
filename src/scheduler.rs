//! Background scheduler: a tokio task that wakes on an interval and, for every
//! subscribed+anchored channel, sends whatever reminders are due.
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
//! The DB guard is always dropped before any `.await` (Discord send / sleep).

use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use rusqlite::Connection;
use serenity::all::{ChannelId, Http};
use tracing::{error, info, warn};

use crate::db::{self, ChannelRow};
use crate::events::load_events;
use crate::send_failure; 
use crate::reminders::{
    compute_reminders, due_and_next, format_batch, chunk_message, DISCORD_MSG_LIMIT, CATCHUP_DAYS,
};

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
            continue; // query guarantees these; defensive check
        };

        // Reused below by the skip_to guard.
        let last_sent = row.last_sent_parsed();
        let reminders =
            compute_reminders(&season, start, row.notify_time_parsed(), &events);

        let batch = due_and_next(&reminders, last_sent, now, Duration::days(CATCHUP_DAYS));

        // Fast-forward past deliberately-skipped stale reminders, even when
        // nothing fresh is due (mid-season anchor / long downtime), so a
        // stale watermark doesn't dump the whole backlog once it catches up.
        if let Some(skip_ts) = batch.skip_to {
            if last_sent.map_or(true, |ls| skip_ts > ls) {
                let conn = db.lock().unwrap();
                let _ = db::record_sent(&conn, row.channel_id, skip_ts, batch.next);
            }
        }

        if batch.due.is_empty() {
            // Keep the stored "next" fresh for status output.
            let conn = db.lock().unwrap();
            let _ = db::set_next_ts(&conn, row.channel_id, batch.next);
            continue;
        }

        let latest = batch.due.iter().map(|r| r.fire_dt).max().unwrap_or(now);
        let channel = ChannelId::new(row.channel_id as u64);

        // Split the batch under Discord's 2000-char limit and send each piece;
        // stop at the first failure so the watermark isn't advanced past an
        // unsent part.
        let chunks = chunk_message(&format_batch(&batch.due), DISCORD_MSG_LIMIT);
        let mut failure = None;
        for piece in &chunks {
            if let Err(e) = channel.say(http, piece.as_str()).await {
                failure = Some(e);
                break;
            }
        }

        match failure {
            None => {
                let conn = db.lock().unwrap();
                if let Err(e) = db::record_sent(&conn, row.channel_id, latest, batch.next) {
                    error!(error = %e, channel = row.channel_id, "failed to record send");
                }
                info!(
                    channel = row.channel_id,
                    reminders = batch.due.len(),
                    parts = chunks.len(),
                    "sent reminders"
                );
            }
            // Classify the failure as permanent vs transient (unsubscribe /
            // skip / retry) instead of blindly retrying every tick.
            Some(e) => {
                send_failure::handle(db, row.channel_id, &e, latest, batch.next);
            }
        }
    }
    Ok(())
}
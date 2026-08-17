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

use chrono::{Duration, TimeZone, Utc}; 
use rusqlite::Connection;
use serenity::all::{ChannelId, CreateAllowedMentions, CreateMessage, Http, RoleId};
use tracing::{error, info, warn};

use crate::db::{self, ChannelRow, CqSlotRow};
use crate::events::load_events;
use crate::send_failure;
use crate::reminders::{
    compute_reminders, due_and_next, format_batch, chunk_message, DISCORD_MSG_LIMIT, CATCHUP_DAYS,
};

/// How late (minutes past the configured time) a conquest ping may still fire.
/// Past this, a missed slot is skipped for the day rather than pinging people
/// long after the activity started (e.g. after bot downtime).
const CQ_GRACE_MINUTES: i64 = 60;

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
        
        let last_sent = row.last_sent_parsed();
        let reminders =
            compute_reminders(&season, start, row.notify_time_parsed(), &events);
        
        let batch = due_and_next(&reminders, last_sent, now, Duration::days(CATCHUP_DAYS));
        
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
            Some(e) => {
                send_failure::handle(db, row.channel_id, &e, latest, batch.next);
            }
        }
    }

    cq_tick(http, db, now).await;
    Ok(())
}

/// Conquest pass: independent of the season timeline. Each configured slot on a
/// subscribed channel gets one `@role` ping per day at its UTC time. A per-day
/// watermark (`cq_slots.last_sent`) prevents repeats; `CQ_GRACE_MINUTES` bounds
/// how late a missed ping may still fire after downtime.
async fn cq_tick(http: &Arc<Http>, db: &Arc<Mutex<Connection>>, now: chrono::DateTime<Utc>) {
    let slots: Vec<CqSlotRow> = {
        let conn = db.lock().unwrap();
        match db::get_due_cq_slots(&conn) {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "could not load conquest slots this tick");
                return;
            }
        }
    };

    let today = now.date_naive();
    for slot in slots {
        // A slot's role id is set alongside its time; skip if either is missing.
        let (Some(time), Some(role_raw)) = (slot.time(), slot.role_id) else {
            continue;
        };
        if slot.last_sent_date() == Some(today) {
            continue; // already pinged today
        }
        let fire_dt = Utc.from_utc_datetime(&today.and_time(time));
        // Not yet, or too late to be useful. Skipping without stamping is
        // harmless: it's a cheap in-memory check with no send.
        if now < fire_dt || now - fire_dt > Duration::minutes(CQ_GRACE_MINUTES) {
            continue;
        }

        let channel = ChannelId::new(slot.channel_id as u64);
        let role_id = RoleId::new(role_raw as u64);
        let content = format!(
            "<@&{}> \u{2694}\u{FE0F} **Conquest** \u{2014} time to assemble!",
            role_raw
        );
        // Explicit allowed_mentions so the role actually pings even if it
        // isn't marked mentionable server-side.
        let msg = CreateMessage::new()
            .content(content)
            .allowed_mentions(CreateAllowedMentions::new().roles(vec![role_id]));

        match channel.send_message(http, msg).await {
            Ok(_) => {
                let conn = db.lock().unwrap();
                if let Err(e) = db::record_cq_sent(&conn, slot.channel_id, slot.slot, today) {
                    error!(
                        error = %e,
                        channel = slot.channel_id,
                        "failed to record conquest send"
                    );
                }
                info!(
                    channel = slot.channel_id,
                    slot = slot.slot.num() as u64,
                    "sent conquest ping"
                );
            }
            // Reuse the send-failure classifier: a permanently unreachable
            // channel auto-unsubscribes; transient errors retry next tick
            // (watermark untouched). `latest`/`next` don't apply to a daily
            // ping, so pass `now`/`None`.
            Err(e) => {
                send_failure::handle(db, slot.channel_id, &e, now, None);
            }
        }
    }
}
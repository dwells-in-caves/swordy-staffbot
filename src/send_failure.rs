//! Classification of Discord send failures, so the scheduler can react
//! differently to *transient* vs *permanent* errors instead of retrying
//! everything on every tick.
//!
//! The scheduler only advances a channel's watermark (`last_sent_ts`) after a
//! send succeeds. Leaving the watermark untouched on failure is correct for a
//! transient blip (retryable) but wrong for a permanent failure, since the
//! next tick re-selects the same "still due" rows and fails again, forever —
//! producing a once-a-minute `failed to send reminders` spam.
//!
//! This module splits failures into three actions:
//!   * `Retry`         — transient (rate limit, 5xx, network). Leave the
//!                       watermark alone; next tick tries again.
//!   * `AccessLost`    — the channel is permanently unreachable (bot kicked,
//!                       channel deleted, permissions revoked). Auto-unsubscribe
//!                       so it stops being targeted.
//!   * `Undeliverable` — the message itself can't be delivered as built (too
//!                       long / payload too large). Advance the watermark past
//!                       this batch so it stops being retried. This *skips*
//!                       the batch; the real fix is to chunk oversized messages
//!                       under Discord's 2000-char limit so they actually send.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serenity::Error as SerenityError;
use serenity::http::HttpError;
use serenity::model::ModelError;
use tracing::{error, warn};

use crate::db;

/// What the scheduler should do about a failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// Transient — retry next tick (do not touch the watermark).
    Retry,
    /// Channel is gone or access was lost — unsubscribe it.
    AccessLost,
    /// Message can't be delivered as built — skip this batch.
    Undeliverable,
}

// --- Discord JSON error codes handled here -------------------------------
// https://discord.com/developers/docs/topics/opcodes-and-status-codes#json
const UNKNOWN_CHANNEL: isize = 10003;
const UNKNOWN_GUILD: isize = 10004;
const MISSING_ACCESS: isize = 50001;
const MISSING_PERMISSIONS: isize = 50013;
const CANNOT_SEND_TO_USER: isize = 50007;

/// Decide how to treat a serenity send error.
///
/// Default is `Retry`: for a reminder bot it is safer to keep retrying an
/// *unrecognized* error (and not lose a reminder) than to silently drop it.
/// Truly-novel permanent codes will still be logged so they can be added here.
pub fn classify(err: &SerenityError) -> SendOutcome {
    match err {
        // Content-side: serenity rejects the message before it ever hits the
        // API because it is over 2000 code points.
        SerenityError::Model(ModelError::MessageTooLong(_)) => SendOutcome::Undeliverable,

        SerenityError::Http(http) => classify_http(http),

        // Anything else (serialization, etc.) — let it retry.
        _ => SendOutcome::Retry,
    }
}

fn classify_http(http: &HttpError) -> SendOutcome {
    match http {
        HttpError::UnsuccessfulRequest(resp) => {
            let status = resp.status_code.as_u16();
            let code = resp.error.code;

            // Permanent "channel unreachable" cases → unsubscribe.
            if matches!(
                code,
                UNKNOWN_CHANNEL
                    | UNKNOWN_GUILD
                    | MISSING_ACCESS
                    | MISSING_PERMISSIONS
                    | CANNOT_SEND_TO_USER
            ) {
                return SendOutcome::AccessLost;
            }

            // Payload too large (413) → can't deliver as built.
            if status == 413 {
                return SendOutcome::Undeliverable;
            }

            // Rate limited or Discord-side outage → transient.
            if status == 429 || (500..=599).contains(&status) {
                return SendOutcome::Retry;
            }

            // Unknown 4xx: don't assume it's permanent — retry; the caller
            // logs the code for triage.
            SendOutcome::Retry
        }
        // Network-level failure (reqwest): transient by nature.
        _ => SendOutcome::Retry,
    }
}

/// Classify the failure, perform the matching DB side-effect, and log it.
///
/// * `latest` / `next` mirror the success path's `record_sent` arguments and
///   are only used for the `Undeliverable` skip.
///
/// Does only synchronous DB work — no `.await` — so the caller never holds the
/// mutex across an await point.
pub fn handle(
    db: &Arc<Mutex<Connection>>,
    channel_id: i64,
    err: &SerenityError,
    latest: DateTime<Utc>,
    next: Option<DateTime<Utc>>,
) -> SendOutcome {
    let outcome = classify(err);
    match outcome {
        SendOutcome::Retry => {
            warn!(error = %err, channel = channel_id, "transient send failure; will retry");
        }
        SendOutcome::AccessLost => {
            let conn = db.lock().unwrap();
            match db::set_subscribed(&conn, channel_id, false) {
                Ok(_) => warn!(
                    error = %err,
                    channel = channel_id,
                    "channel unreachable; auto-unsubscribed to stop retrying"
                ),
                Err(e) => error!(
                    error = %e,
                    channel = channel_id,
                    "channel unreachable but failed to unsubscribe it"
                ),
            }
        }
        SendOutcome::Undeliverable => {
            let conn = db.lock().unwrap();
            // Advance the watermark past this batch so it stops being retried.
            // This SKIPS the batch — chunking is the real fix (see module docs).
            match db::record_sent(&conn, channel_id, latest, next) {
                Ok(_) => error!(
                    error = %err,
                    channel = channel_id,
                    "message undeliverable (too large); skipped this batch — add chunking to deliver it"
                ),
                Err(e) => error!(
                    error = %e,
                    channel = channel_id,
                    "message undeliverable and failed to advance watermark"
                ),
            }
        }
    }
    outcome
}

//! Configuration, loaded from environment variables or a local .env file.
//!
//! The bot token is read from the environment and is never hardcoded or logged.

use anyhow::{anyhow, Context as _, Result};
use chrono::NaiveTime;

#[derive(Debug, Clone)]
pub struct Config {
    pub token: String,
    pub prefix: String,
    pub check_interval_secs: u64,
    pub default_notify: NaiveTime,
    pub events_path: String,
    pub db_path: String,
}

pub fn load() -> Result<Config> {
    let token = std::env::var("DISCORD_TOKEN").map_err(|_| {
        anyhow!(
            "DISCORD_TOKEN is not set. Copy .env.example to .env and add your \
             bot token, or export DISCORD_TOKEN in your environment."
        )
    })?;

    let prefix = std::env::var("SS_PREFIX").unwrap_or_else(|_| "!ss ".to_string());

    let check_interval_secs = std::env::var("SS_CHECK_INTERVAL")
        .ok()
        .map(|s| s.parse::<u64>())
        .transpose()
        .context("SS_CHECK_INTERVAL must be a whole number of seconds")?
        .unwrap_or(60);

    let default_notify_raw =
        std::env::var("SS_DEFAULT_NOTIFY_TIME").unwrap_or_else(|_| "00:00".to_string());
    let default_notify = NaiveTime::parse_from_str(&default_notify_raw, "%H:%M")
        .context("SS_DEFAULT_NOTIFY_TIME must be HH:MM (24h UTC)")?;

    let events_path = std::env::var("SS_EVENTS_PATH").unwrap_or_else(|_| "events.json".to_string());
    let db_path = std::env::var("SS_DB_PATH").unwrap_or_else(|_| "ssbot.db".to_string());

    Ok(Config {
        token,
        prefix,
        check_interval_secs,
        default_notify,
        events_path,
        db_path,
    })
}

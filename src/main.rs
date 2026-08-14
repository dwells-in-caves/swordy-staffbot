//! Entry point for the Sword & Staff reminder bot.
//!
//! Run with: `cargo run`
//! Requires DISCORD_TOKEN in the environment (see config.rs / .env.example).

mod commands;
mod config;
mod db;
mod events;
mod reminders;
mod scheduler;
mod send_failure;
mod discord_time;

use std::sync::{Arc, Mutex};

use chrono::NaiveTime;
use poise::serenity_prelude as serenity;
use rusqlite::Connection;

/// Shared state available to every command and to the scheduler.
pub struct Data {
    pub db: Arc<Mutex<Connection>>,
    pub events_path: String,
    pub default_notify: NaiveTime,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Honors RUST_LOG (e.g. RUST_LOG=info). Defaults to warn if unset.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,ssbot=info")),
        )
        .init();

    let cfg = config::load()?;

    // Open the database once and share it behind a Mutex. Every critical
    // section is short and synchronous, so this is fine at bot scale.
    let conn = Connection::open(&cfg.db_path)?;
    db::init(&conn)?;
    let database: Arc<Mutex<Connection>> = Arc::new(Mutex::new(conn));

    // Prefix commands need the privileged MESSAGE_CONTENT intent. Enable it in
    // the Discord Developer Portal for this bot. (Slash commands don't need it,
    // so you could drop MESSAGE_CONTENT if you only want slash commands.)
    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;

    let prefix = cfg.prefix.clone();
    let events_path = cfg.events_path.clone();
    let default_notify = cfg.default_notify;
    let interval = cfg.check_interval_secs;
    let db_for_setup = database.clone();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            prefix_options: poise::PrefixFrameworkOptions {
                prefix: Some(prefix),
                ..Default::default()
            },
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                tracing::info!("logged in as {}", ready.user.name);
                // Register slash commands globally. (Global registration can
                // take up to an hour to propagate the first time; for fast
                // iteration, register per-guild instead.)
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;

                // Kick off the background reminder loop. It only needs the HTTP
                // client to post messages, so hand it a clone of that.
                tokio::spawn(scheduler::run(
                    ctx.http.clone(),
                    db_for_setup.clone(),
                    events_path.clone(),
                    interval,
                ));

                Ok(Data {
                    db: db_for_setup,
                    events_path,
                    default_notify,
                })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(&cfg.token, intents)
        .framework(framework)
        .await?;

    client.start().await?;
    Ok(())
}

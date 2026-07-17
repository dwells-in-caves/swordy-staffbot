//! Bot commands, defined with poise so each one is BOTH a prefix command and a
//! slash command from a single function. Adding a new command later is just
//! another `#[poise::command]` function added to the `all()` list in main.rs —
//! that is the "command extension" the design is built around.
//!
//! Admin-only commands use poise's `required_permissions = "MANAGE_GUILD"`.

use chrono::{Duration, NaiveDate, NaiveTime, Utc};

use crate::events::load_events;
use crate::reminders::{compute_reminders, current_day_number, due_and_next};
use crate::{db, Context, Error};

fn ids(ctx: &Context<'_>) -> (i64, Option<i64>) {
    let channel_id = ctx.channel_id().get() as i64;
    let guild_id = ctx.guild_id().map(|g| g.get() as i64);
    (channel_id, guild_id)
}

/// Subscribe this channel to Sword & Staff reminders.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn subscribe(ctx: Context<'_>) -> Result<(), Error> {
    let (channel_id, guild_id) = ids(&ctx);
    let prefix = ctx.prefix().to_string();

    let msg = {
        let conn = ctx.data().db.lock().unwrap();
        db::upsert_subscription(&conn, channel_id, guild_id)?;
        let row = db::get_channel(&conn, channel_id)?;
        match row {
            Some(r) if r.start_date.is_some() => {
                "\u{2705} Subscribed. Reminders are active for this channel.".to_string()
            }
            _ => format!(
                "\u{2705} Subscribed. Now set your server's open date with \
                 `{prefix}setstart YYYY-MM-DD` or `{prefix}setday N` so I know \
                 which day this server is on."
            ),
        }
    };
    ctx.say(msg).await?;
    Ok(())
}

/// Stop reminders in this channel (keeps your start date for later).
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn unsubscribe(ctx: Context<'_>) -> Result<(), Error> {
    let (channel_id, _) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_subscribed(&conn, channel_id, false)?;
    }
    ctx.say("\u{1F515} Unsubscribed. Your start date is kept in case you re-subscribe.")
        .await?;
    Ok(())
}

/// Set the server's open date, e.g. `/setstart 2026-01-15`.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn setstart(
    ctx: Context<'_>,
    #[description = "Server open date as YYYY-MM-DD"] date: String,
) -> Result<(), Error> {
    let start = match NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => {
            ctx.say("\u{274C} Please use the format `YYYY-MM-DD`, e.g. `2026-01-15`.")
                .await?;
            return Ok(());
        }
    };
    let (channel_id, guild_id) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_start_date(&conn, channel_id, guild_id, start)?;
    }
    let day_now = current_day_number(start, Utc::now());
    ctx.say(format!(
        "\u{1F4C6} Server open date set to **{}**. This server is currently on **day {}**.",
        start.format("%Y-%m-%d"),
        day_now
    ))
    .await?;
    Ok(())
}

/// Shortcut: tell me what day the server is on right now, e.g. `/setday 12`.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn setday(
    ctx: Context<'_>,
    #[description = "The day number the server is on today"] day: i64,
) -> Result<(), Error> {
    if day < 0 {
        ctx.say("\u{274C} Day must be 0 or greater.").await?;
        return Ok(());
    }
    let start = Utc::now().date_naive() - Duration::days(day);
    let (channel_id, guild_id) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_start_date(&conn, channel_id, guild_id, start)?;
    }
    ctx.say(format!(
        "\u{1F4C6} Got it \u{2014} treating today as **day {}** (server open date **{}**).",
        day,
        start.format("%Y-%m-%d")
    ))
    .await?;
    Ok(())
}

/// Set the UTC time of day reminders send, e.g. `/notifytime 13:30`.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn notifytime(
    ctx: Context<'_>,
    #[description = "Time of day in 24h UTC, HH:MM"] hhmm: String,
) -> Result<(), Error> {
    let t = match NaiveTime::parse_from_str(hhmm.trim(), "%H:%M") {
        Ok(t) => t,
        Err(_) => {
            ctx.say("\u{274C} Please use 24h `HH:MM` UTC, e.g. `13:30`.")
                .await?;
            return Ok(());
        }
    };
    let (channel_id, _) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_notify_time(&conn, channel_id, t)?;
    }
    ctx.say(format!(
        "\u{23F0} Reminders will send around **{} UTC**.",
        hhmm.trim()
    ))
    .await?;
    Ok(())
}

/// Show this channel's subscription config and next reminder.
#[poise::command(slash_command, prefix_command)]
pub async fn status(ctx: Context<'_>) -> Result<(), Error> {
    let (channel_id, _) = ids(&ctx);
    let prefix = ctx.prefix().to_string();
    let events_path = ctx.data().events_path.clone();
    let now = Utc::now();

    let row = {
        let conn = ctx.data().db.lock().unwrap();
        db::get_channel(&conn, channel_id)?
    };

    let Some(row) = row else {
        ctx.say(format!(
            "This channel isn't set up yet. Try `{prefix}subscribe`."
        ))
        .await?;
        return Ok(());
    };

    let mut lines = vec![
        format!("**Subscribed:** {}", if row.subscribed { "yes" } else { "no" }),
        format!(
            "**Server open date:** {}",
            row.start_date.clone().unwrap_or_else(|| "not set".to_string())
        ),
        format!("**Notify time (UTC):** {}", row.notify_time),
        format!("**Current UTC time:** {}", now.format("%Y-%m-%d %H:%M")),
    ];

    if let Some(start) = row.start_date_parsed() {
        lines.push(format!("**Current server day:** {}", current_day_number(start, now)));
        // Loading events can fail if the file is edited badly; don't crash the command.
        if let Ok(events) = load_events(&events_path) {
            let reminders = compute_reminders(start, row.notify_time_parsed(), &events);
            let (_, next) = due_and_next(&reminders, row.last_sent_parsed(), now);
            let next_str = next
                .map(|d| format!("{} UTC", d.format("%Y-%m-%d %H:%M")))
                .unwrap_or_else(|| "none scheduled".to_string());
            lines.push(format!("**Next reminder:** {next_str}"));
        }
    }

    ctx.say(lines.join("\n")).await?;
    Ok(())
}

/// List the configured event schedule.
#[poise::command(slash_command, prefix_command, rename = "events")]
pub async fn list_events(ctx: Context<'_>) -> Result<(), Error> {
    let events_path = ctx.data().events_path.clone();
    let mut events = match load_events(&events_path) {
        Ok(e) => e,
        Err(e) => {
            ctx.say(format!("\u{274C} Couldn't read the event schedule: {e}"))
                .await?;
            return Ok(());
        }
    };
    events.sort_by_key(|e| e.day);

    let mut lines = vec!["**Sword & Staff event schedule** (by day since server open):".to_string()];
    for ev in &events {
        let notices = if ev.notice_days.is_empty() {
            "day-of".to_string()
        } else {
            ev.notice_days
                .iter()
                .map(|&n| if n == 0 { "day-of".to_string() } else { format!("-{n}d") })
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(format!(
            "\u{2022} **Day {} \u{2014} {}** _(reminds: {})_",
            ev.day, ev.title, notices
        ));
    }
    ctx.say(lines.join("\n")).await?;
    Ok(())
}

/// Send a sample reminder here to confirm I can post in this channel.
#[poise::command(slash_command, prefix_command)]
pub async fn test(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say(
        "\u{1F4C5} **Sample reminder \u{2014} Day 3: First Guild War**\n\
         Save at least 200 gems and hold your summon tickets. \
         (This is a test; real reminders fire on your schedule.)",
    )
    .await?;
    Ok(())
}

/// Every command the bot exposes. Add new ones here.
pub fn all() -> Vec<poise::Command<crate::Data, Error>> {
    vec![
        subscribe(),
        unsubscribe(),
        setstart(),
        setday(),
        notifytime(),
        status(),
        list_events(),
        test(),
    ]
}

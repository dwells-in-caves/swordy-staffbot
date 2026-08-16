//! Bot commands, defined with poise so each one is both a prefix command and a
//! slash command from a single function.
//!
//! Season model: a channel is anchored to one season at a time via
//! `setseason`/`setday`. Admin-only commands use `required_permissions`.
//!
//! Locking: `Data::db` is a std `Mutex`; every DB critical section is kept in a
//! `{ ... }` block so the guard is dropped before any `.await`.

use chrono::{Duration, NaiveDate, NaiveTime, Utc};
use serenity::all::RoleId;

use crate::conquest::{self, Slot};
use crate::events::{load_events, seasons};
use crate::reminders::{compute_reminders, current_day_in_season, due_and_next, format_upcoming, upcoming_events, CATCHUP_DAYS};
use crate::{db, Context, Error};

fn ids(ctx: &Context<'_>) -> (i64, Option<i64>) {
    (
        ctx.channel_id().get() as i64,
        ctx.guild_id().map(|g| g.get() as i64),
    )
}

/// Normalize "s3" -> "S3" and confirm it exists in the schedule.
fn resolve_season(ctx: &Context<'_>, raw: &str) -> Result<Option<String>, Error> {
    let events = load_events(&ctx.data().events_path)?;
    let wanted = raw.trim().to_uppercase();
    Ok(seasons(&events).into_iter().find(|s| s.to_uppercase() == wanted))
}

/// Subscribe this channel to Sword & Staff reminders.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn subscribe(ctx: Context<'_>) -> Result<(), Error> {
    let (channel_id, guild_id) = ids(&ctx);
    let prefix = ctx.prefix().to_string();
    let anchored = {
        let conn = ctx.data().db.lock().unwrap();
        db::upsert_subscription(&conn, channel_id, guild_id)?;
        db::get_channel(&conn, channel_id)?
            .map(|r| r.season.is_some() && r.start_date.is_some())
            .unwrap_or(false)
    };
    let msg = if anchored {
        "\u{2705} Subscribed. Reminders are active for this channel.".to_string()
    } else {
        format!(
            "\u{2705} Subscribed. Now anchor this server's current season with \
             `{prefix}setday S3 20` (\"we're on S3 day 20 today\") or \
             `{prefix}setseason S3 2026-03-01` (the season's start / Telescope Date)."
        )
    };
    ctx.say(msg).await?;
    Ok(())
}

/// Stop reminders in this channel (keeps your season anchor).
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn unsubscribe(ctx: Context<'_>) -> Result<(), Error> {
    let (channel_id, _) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_subscribed(&conn, channel_id, false)?;
    }
    ctx.say("\u{1F515} Unsubscribed. Your season anchor is kept in case you re-subscribe.")
        .await?;
    Ok(())
}

/// Anchor the current season by its start date, e.g. `/setseason S3 2026-03-01`.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn setseason(
    ctx: Context<'_>,
    #[description = "Season label, e.g. S3"] season: String,
    #[description = "Season start / Telescope Date, YYYY-MM-DD"] date: String,
) -> Result<(), Error> {
    let Some(season) = resolve_season(&ctx, &season)? else {
        ctx.say("\u{274C} Unknown season. Use one of the labels from the schedule, e.g. `S1`\u{2013}`S5`.")
            .await?;
        return Ok(());
    };
    let Ok(start) = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d") else {
        ctx.say("\u{274C} Please use `YYYY-MM-DD`, e.g. `2026-03-01`.").await?;
        return Ok(());
    };
    let (channel_id, guild_id) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_season_anchor(&conn, channel_id, guild_id, &season, start)?;
    }
    let day_now = current_day_in_season(start, Utc::now());
    ctx.say(format!(
        "\u{1F4C6} Anchored **{season}** to **{}**. This server is on **{season} day {day_now}**.",
        start.format("%Y-%m-%d")
    ))
        .await?;
    Ok(())
}

/// Shortcut: "the server is on this season + day right now", e.g. `/setday S3 20`.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD")]
pub async fn setday(
    ctx: Context<'_>,
    #[description = "Season label, e.g. S3"] season: String,
    #[description = "Current day within that season"] day: i64,
) -> Result<(), Error> {
    let Some(season) = resolve_season(&ctx, &season)? else {
        ctx.say("\u{274C} Unknown season. Use e.g. `S1`\u{2013}`S5`.").await?;
        return Ok(());
    };
    if day < 0 {
        ctx.say("\u{274C} Day must be 0 or greater.").await?;
        return Ok(());
    }
    // season_start is the day-0 origin, so today = start + day => start = today - day.
    let start = Utc::now().date_naive() - Duration::days(day);
    let (channel_id, guild_id) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_season_anchor(&conn, channel_id, guild_id, &season, start)?;
    }
    ctx.say(format!(
        "\u{1F4C6} Anchored **{season} day {day}** today (origin **{}**).",
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
    let Ok(t) = NaiveTime::parse_from_str(hhmm.trim(), "%H:%M") else {
        ctx.say("\u{274C} Please use 24h `HH:MM` UTC, e.g. `13:30`.").await?;
        return Ok(());
    };
    let (channel_id, _) = ids(&ctx);
    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_notify_time(&conn, channel_id, t)?;
    }
    ctx.say(format!("\u{23F0} Reminders will send around **{} UTC**.", hhmm.trim()))
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
        ctx.say(format!("This channel isn't set up yet. Try `{prefix}subscribe`."))
            .await?;
        return Ok(());
    };

    let mut lines = vec![
        format!("**Subscribed:** {}", if row.subscribed { "yes" } else { "no" }),
        format!("**Season:** {}", row.season.clone().unwrap_or_else(|| "not set".into())),
        format!(
            "**Season origin (Telescope Date):** {}",
            row.start_date.clone().unwrap_or_else(|| "not set".into())
        ),
        format!("**Notify time (UTC):** {}", row.notify_time),
        format!("**Current UTC time:** {}", now.format("%Y-%m-%d %H:%M")),
    ];

    if let (Some(season), Some(start)) = (row.season.clone(), row.start_date_parsed()) {
        lines.push(format!("**Current day:** {season} day {}", current_day_in_season(start, now)));
        if let Ok(events) = load_events(&events_path) {
            let rs = compute_reminders(&season, start, row.notify_time_parsed(), &events);
            let next = due_and_next(&rs, row.last_sent_parsed(), now, Duration::days(CATCHUP_DAYS)).next;
            let next_str = next
                .map(|d| format!("{} UTC", d.format("%Y-%m-%d %H:%M")))
                .unwrap_or_else(|| "none left this season".into());
            lines.push(format!("**Next reminder:** {next_str}"));
        }
    }
    ctx.say(lines.join("\n")).await?;
    Ok(())
}

/// List the schedule. With no season, shows a summary; with one, lists it.
#[poise::command(slash_command, prefix_command, rename = "events")]
pub async fn list_events(
    ctx: Context<'_>,
    #[description = "Optional season to list, e.g. S3"] season: Option<String>,
) -> Result<(), Error> {
    let events = match load_events(&ctx.data().events_path) {
        Ok(e) => e,
        Err(e) => {
            ctx.say(format!("\u{274C} Couldn't read the schedule: {e}")).await?;
            return Ok(());
        }
    };

    let msg = match season.and_then(|s| resolve_season(&ctx, &s).ok().flatten()) {
        Some(season) => {
            let mut evs: Vec<_> = events.iter().filter(|e| e.season == season).collect();
            evs.sort_by_key(|e| e.day);
            let mut lines = vec![format!("**{season} schedule** (day within season):")];
            for e in evs {
                let lv = e.level.map(|l| format!(" \u{2022} Lv.{l}")).unwrap_or_default();
                lines.push(format!("Day {} \u{2014} {}{}", e.day, e.title, lv));
            }
            lines.join("\n")
        }
        None => {
            let mut lines = vec!["**Sword & Staff schedule** \u{2014} seasons:".to_string()];
            for s in seasons(&events) {
                let n = events.iter().filter(|e| e.season == *s).count();
                lines.push(format!("\u{2022} **{s}** ({n} events)"));
            }
            lines.push(format!("Use `{}events S3` to list a season.", ctx.prefix()));
            lines.join("\n")
        }
    };
    ctx.say(msg).await?;
    Ok(())
}

/// List upcoming events. `/next` shows the next one; `/next 3` shows three.
#[poise::command(slash_command, prefix_command, guild_only)]
pub async fn next(
    ctx: Context<'_>,
    #[description = "How many upcoming events to list (default 1)"] count: Option<u32>,
) -> Result<(), Error> {
    // Default to the next single event; clamp so one call can't blow past
    // Discord's message limit.
    let want = count.unwrap_or(1).clamp(1, 20) as usize;
    let (channel_id, _) = ids(&ctx);
    let prefix = ctx.prefix().to_string();
    let events_path = ctx.data().events_path.clone();
    let now = Utc::now();

    let row = {
        let conn = ctx.data().db.lock().unwrap();
        db::get_channel(&conn, channel_id)?
    };
    let Some(row) = row else {
        ctx.say(format!("This channel isn't set up yet. Try `{prefix}subscribe`."))
            .await?;
        return Ok(());
    };
    let (Some(season), Some(start)) = (row.season.clone(), row.start_date_parsed()) else {
        ctx.say(format!(
            "No season anchored yet \u{2014} set one with `{prefix}setday S3 20`."
        ))
            .await?;
        return Ok(());
    };

    let events = match load_events(&events_path) {
        Ok(e) => e,
        Err(e) => {
            ctx.say(format!("\u{274C} Couldn't read the schedule: {e}")).await?;
            return Ok(());
        }
    };

    let items = upcoming_events(&season, start, row.notify_time_parsed(), now, &events, want);
    if items.is_empty() {
        ctx.say(format!(
            "No more events in **{season}** \u{2014} re-anchor when the next season starts."
        ))
            .await?;
        return Ok(());
    }

    let header = if items.len() == 1 {
        format!("**Next event** in {season}:")
    } else {
        format!("**Next {} events** in {season}:", items.len())
    };

    // Assemble within Discord's 2000-char limit; drop overflow with a note.
    let mut out = header;
    let mut shown = 0usize;
    for u in &items {
        let block = format!("\n\n{}", format_upcoming(u));
        if out.len() + block.len() > 1900 {
            break;
        }
        out.push_str(&block);
        shown += 1;
    }
    if shown < items.len() {
        out.push_str(&format!("\n\n\u{2026}and {} more.", items.len() - shown));
    }

    ctx.say(out).await?;
    Ok(())
}

/// Send a sample reminder here to confirm I can post in this channel.
#[poise::command(slash_command, prefix_command)]
pub async fn test(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say(
        "**In 3 days \u{00B7} S3 Day 28**\n\u{2694}\u{FE0F} Abyssal Bastion opens\n\
         \u{2022} Power to enter \u{2014} Normal 20M \u{00B7} Hard 26M \u{00B7} Nightmare 30M \u{00B7} Purgatory 46M\n\
         _(This is a test; real reminders fire on your schedule.)_",
    )
        .await?;
    Ok(())
}

/// Admin: set the daily UTC time a conquest slot pings, e.g. `/setcqtime early 12:30`.
///
/// Ensures the slot's role exists (creating it if needed) and stores both the
/// role id and the time. The channel must have run `/subscribe` first.
#[poise::command(slash_command, prefix_command, required_permissions = "MANAGE_GUILD", guild_only)]
pub async fn setcqtime(
    ctx: Context<'_>,
    #[description = "Which conquest slot: early or late"] slot: String,
    #[description = "Time of day in 24h UTC, HH:MM"] hhmm: String,
) -> Result<(), Error> {
    let Some(slot) = Slot::parse(&slot) else {
        ctx.say("\u{274C} Slot must be `early` or `late`.").await?;
        return Ok(());
    };
    let Ok(t) = NaiveTime::parse_from_str(hhmm.trim(), "%H:%M") else {
        ctx.say("\u{274C} Please use 24h `HH:MM` UTC, e.g. `12:30`.").await?;
        return Ok(());
    };
    let Some(guild_id) = ctx.guild_id() else {
        ctx.say("\u{274C} This command only works in a server.").await?;
        return Ok(());
    };
    let (channel_id, _) = ids(&ctx);

    // Conquest is gated behind /subscribe: the channel row must already exist.
    let row = {
        let conn = ctx.data().db.lock().unwrap();
        db::get_channel(&conn, channel_id)?
    };
    let Some(row) = row else {
        ctx.say(format!("\u{274C} Run `{}subscribe` in this channel first.", ctx.prefix()))
            .await?;
        return Ok(());
    };

    // Resolve/create the role without holding the DB lock across the await.
    let known_role = row.cq_role_id(slot).map(|id| RoleId::new(id as u64));
    let role_id =
        conquest::ensure_role(&ctx.serenity_context().http, guild_id, slot, known_role).await?;

    {
        let conn = ctx.data().db.lock().unwrap();
        db::set_cq_role_id(&conn, channel_id, slot, role_id.get() as i64)?;
        db::set_cq_time(&conn, channel_id, slot, Some(t))?;
    }
    ctx.say(format!(
        "\u{2694}\u{FE0F} Conquest **{}** reminder set for **{} UTC**. The **{}** role will be \
         pinged \u{2014} members can opt in with `{}conquest {}`.",
        slot.label(),
        hhmm.trim(),
        slot.role_name(),
        ctx.prefix(),
        slot.label()
    ))
    .await?;
    Ok(())
}

/// Opt in or out of conquest pings: `/conquest early` or `/conquest late`.
///
/// Toggles the slot's role on yourself. The two slots are independent, so you
/// can hold both.
#[poise::command(slash_command, prefix_command, guild_only)]
pub async fn conquest(
    ctx: Context<'_>,
    #[description = "Which conquest slot to toggle: early or late"] slot: String,
) -> Result<(), Error> {
    let Some(slot) = Slot::parse(&slot) else {
        ctx.say("\u{274C} Pick `early` or `late`.").await?;
        return Ok(());
    };
    let Some(guild_id) = ctx.guild_id() else {
        ctx.say("\u{274C} This command only works in a server.").await?;
        return Ok(());
    };
    let (channel_id, _) = ids(&ctx);

    let known_role = {
        let conn = ctx.data().db.lock().unwrap();
        db::get_channel(&conn, channel_id)?
            .and_then(|r| r.cq_role_id(slot))
            .map(|id| RoleId::new(id as u64))
    };
    let role_id =
        conquest::ensure_role(&ctx.serenity_context().http, guild_id, slot, known_role).await?;

    let Some(member) = ctx.author_member().await else {
        ctx.say("\u{274C} Couldn't find your membership in this server.").await?;
        return Ok(());
    };

    let http = ctx.serenity_context();
    if member.roles.contains(&role_id) {
        member.remove_role(http, role_id).await?;
        ctx.say(format!("\u{1F515} Removed you from **{}** conquest pings.", slot.label()))
            .await?;
    } else {
        member.add_role(http, role_id).await?;
        ctx.say(format!("\u{2705} You're in \u{2014} you'll be pinged for **{}** conquest.", slot.label()))
            .await?;
    }
    Ok(())
}

/// Every command the bot exposes. Add new ones here.
/// All commands registered with the framework.
pub fn all() -> Vec<poise::Command<crate::Data, Error>> {
    vec![
        subscribe(),
        unsubscribe(),
        setseason(),
        setday(),
        notifytime(),
        status(),
        list_events(),
        next(),
        test(),
        setcqtime(),
        conquest(),
    ]
}

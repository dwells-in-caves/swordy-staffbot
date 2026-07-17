# Sword & Staff reminder bot

A Discord bot that reminds subscribed channels about time-gated **Sword & Staff**
events, based on each server's own open date. Built in Rust with
[Serenity](https://github.com/serenity-rs/serenity) + [Poise](https://github.com/serenity-rs/poise).

## Why this stack

- **Serenity 0.12** is the Discord API library.
- **Poise 0.6** is the command framework built on top of Serenity. Serenity's
  own `standard_framework` is deprecated as of 0.12.1 in favour of Poise.
  Poise lets each command be **both a prefix command and a slash command** from
  a single function, which is what makes the bot easy to extend later — adding a
  command is one `#[poise::command]` function added to `commands::all()`.
- **rusqlite** (bundled SQLite) for persistence — synchronous but more than fast
  enough at this scale.

## How reminders work

Events are defined by *day number since a server opened* (`day: 0` = launch day)
in `events.json`. Each subscribed channel stores the real-world date its server
opened, so an event on day `D` fires on the calendar date `start_date + D days`.

Each event can carry `notice_days` — how many days early to remind:

```json
{ "day": 7, "title": "Tower of Trials", "message": "Bank your energy potions.", "notice_days": [3, 1, 0] }
```

`[3, 1, 0]` means: remind 3 days before, 1 day before, and on the day itself.
Omit `notice_days` (or use `[0]`) for a single day-of reminder.

The scheduler wakes up every `SS_CHECK_INTERVAL` seconds and, per channel, sends
any reminders whose fire time has passed since the last send. Because it tracks
`last_sent_ts`, it never double-sends and automatically catches up on anything
missed while the bot was offline.

## Setup

1. **Install Rust** (current stable — see the toolchain note below).
2. **Create the bot** in the [Discord Developer Portal](https://discord.com/developers/applications):
   create an application, add a Bot, and copy the token.
   - Enable the **Message Content Intent** (Bot → Privileged Gateway Intents).
     This is required for prefix commands. If you only want slash commands, you
     can remove `MESSAGE_CONTENT` in `main.rs` and skip this.
   - Invite the bot with the `bot` and `applications.commands` scopes and the
     **Send Messages** permission.
3. **Configure**: `cp .env.example .env` and paste your token into `.env`.
4. **Run**: `cargo run`

The token is read from the environment only — it is never hardcoded or logged.

## Commands

Default prefix is `!ss ` (configurable via `SS_PREFIX`). All commands also work
as slash commands.

| Command | Permission | Description |
| --- | --- | --- |
| `subscribe` | Manage Server | Subscribe this channel to reminders |
| `unsubscribe` | Manage Server | Stop reminders (keeps your start date) |
| `setstart YYYY-MM-DD` | Manage Server | Set the server's open date |
| `setday N` | Manage Server | Shortcut: "the server is on day N today" |
| `notifytime HH:MM` | Manage Server | UTC time of day reminders send |
| `status` | anyone | Show config, current day, next reminder |
| `events` | anyone | List the configured event schedule |
| `test` | anyone | Post a sample reminder here |

## Configuration (environment variables)

| Variable | Default | Meaning |
| --- | --- | --- |
| `DISCORD_TOKEN` | *(required)* | Bot token |
| `SS_PREFIX` | `!ss ` | Text command prefix |
| `SS_CHECK_INTERVAL` | `60` | Scheduler tick, seconds |
| `SS_DEFAULT_NOTIFY_TIME` | `00:00` | Default send time (UTC HH:MM) |
| `SS_EVENTS_PATH` | `events.json` | Path to the schedule file |
| `SS_DB_PATH` | `ssbot.db` | SQLite file path |
| `RUST_LOG` | `warn,ssbot=info` | Log filter |

## Project layout

```
src/
  main.rs        entry point: framework setup, intents, spawns the scheduler
  config.rs      environment configuration
  events.rs      loads + validates events.json
  reminders.rs   pure scheduling logic (no Discord dep) + unit tests
  db.rs          rusqlite persistence + round-trip tests
  commands.rs    poise commands (prefix + slash)
  scheduler.rs   background reminder loop
events.json      the event schedule (edit this; it's data, not code)
```

## Tests

```
cargo test
```

Covers the scheduling logic (advance-notice date math, due/next selection,
no-double-send across restarts, downtime catch-up) and database round-trips
(subscribe/unsubscribe, start-date persistence, sent-timestamp tracking).

## Toolchain note

The current Serenity dependency tree pulls transitive crates that require a
recent Rust (edition 2024, i.e. **Rust 1.85+**). Use an up-to-date stable
toolchain (`rustup update stable`). Building the bundled SQLite also needs a C
compiler (`cc`/`gcc`) on the build machine.

## Extending it

Each command is a self-contained `#[poise::command]` function. To add one,
write the function in `commands.rs` and add it to the `all()` list — it becomes
available as both a prefix and slash command automatically. The event schedule
lives entirely in `events.json`, so updating game content needs no code change.

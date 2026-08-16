//! Conquest reminders: a group activity that pings a self-assign role at an
//! admin-configured time of day, independent of the season event schedule.
//!
//! There are two slots, `early` and `late`, each backed by its own Discord role
//! ("CQ Early" / "CQ Late"). Admins set the daily UTC fire time per slot with
//! `/setcqtime`; users opt in/out with `/conquest`. The scheduler pings the
//! slot's role once per day at its time (see scheduler.rs).
//!
//! This module holds the pure `Slot` model plus the one Discord-touching helper
//! (`ensure_role`). Persistence lives in db.rs (keyed off the column-name
//! helpers here); the scheduler and commands drive the rest.

use serenity::all::{EditRole, GuildId, Http, RoleId};

/// One of the two conquest time slots. Each maps 1:1 to a Discord role and to a
/// fixed set of `channels` columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Early,
    Late,
}

impl Slot {
    /// Both slots, for iterating in the scheduler.
    pub const ALL: [Slot; 2] = [Slot::Early, Slot::Late];

    /// Parse the user-facing argument (`early` / `late`, case-insensitive).
    pub fn parse(raw: &str) -> Option<Slot> {
        match raw.trim().to_lowercase().as_str() {
            "early" => Some(Slot::Early),
            "late" => Some(Slot::Late),
            _ => None,
        }
    }

    /// Lower-case label used in user-facing messages.
    pub fn label(self) -> &'static str {
        match self {
            Slot::Early => "early",
            Slot::Late => "late",
        }
    }

    /// The Discord role name this slot creates / looks up.
    pub fn role_name(self) -> &'static str {
        match self {
            Slot::Early => "CQ Early",
            Slot::Late => "CQ Late",
        }
    }

    // --- channels-table column names (compile-time constants, so they are safe
    // to interpolate into SQL in db.rs) ---

    pub fn time_col(self) -> &'static str {
        match self {
            Slot::Early => "cq_early_time",
            Slot::Late => "cq_late_time",
        }
    }

    pub fn role_col(self) -> &'static str {
        match self {
            Slot::Early => "cq_early_role_id",
            Slot::Late => "cq_late_role_id",
        }
    }

    pub fn last_sent_col(self) -> &'static str {
        match self {
            Slot::Early => "cq_early_last_sent",
            Slot::Late => "cq_late_last_sent",
        }
    }
}

/// Resolve the Discord role for a slot, creating it if needed, and return its id.
///
/// Resolution order, to avoid ever creating duplicate roles:
///   1. If `known_id` still exists in the guild, use it.
///   2. Else if a role already exists with this slot's name, adopt it.
///   3. Else create a new mentionable role and return its id.
///
/// Requires the bot to have the Manage Roles permission. The created role is
/// `mentionable` so the scheduled ping reliably notifies members.
pub async fn ensure_role(
    http: &Http,
    guild_id: GuildId,
    slot: Slot,
    known_id: Option<RoleId>,
) -> serenity::Result<RoleId> {
    let roles = guild_id.roles(http).await?;

    if let Some(id) = known_id {
        if roles.contains_key(&id) {
            return Ok(id);
        }
    }

    if let Some(role) = roles.values().find(|r| r.name == slot.role_name()) {
        return Ok(role.id);
    }

    let role = guild_id
        .create_role(http, EditRole::new().name(slot.role_name()).mentionable(true))
        .await?;
    Ok(role.id)
}

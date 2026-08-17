//! Conquest reminders: a group activity that pings a self-assign role at an
//! admin-configured time of day, independent of the season event schedule.
//!
//! There are six time-neutral slots, `1`..`6`, each backed by a Discord role
//! ("CQ_1".."CQ_6"). Admins set a slot's daily UTC fire time with `/setcqtime`;
//! users opt in/out with `/conquest`; `/cqtimes` lists configured slots. The
//! scheduler pings a slot's role once per day at its time (see scheduler.rs).
//!
//! This module holds the pure `Slot` model plus the one Discord-touching helper
//! (`ensure_role`). Persistence lives in db.rs (the `cq_slots` table); the
//! scheduler and commands drive the rest.

use serenity::all::{EditRole, GuildId, Http, RoleId};

/// One conquest time slot, numbered 1..=`COUNT`. Each maps 1:1 to a Discord role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot(u8);

impl Slot {
    /// How many slots exist (1..=COUNT).
    pub const COUNT: u8 = 6;

    /// Wrap a raw number if it's a valid slot (1..=COUNT).
    pub fn from_num(n: u8) -> Option<Slot> {
        if (1..=Self::COUNT).contains(&n) {
            Some(Slot(n))
        } else {
            None
        }
    }

    /// Parse the user-facing argument (a number 1..=COUNT).
    pub fn parse(raw: &str) -> Option<Slot> {
        raw.trim().parse::<u8>().ok().and_then(Self::from_num)
    }

    /// The slot number.
    pub fn num(self) -> u8 {
        self.0
    }

    /// The Discord role name this slot creates / looks up.
    pub fn role_name(self) -> String {
        format!("CQ_{}", self.0)
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

    let name = slot.role_name();
    if let Some(role) = roles.values().find(|r| r.name == name) {
        return Ok(role.id);
    }

    let role = guild_id
        .create_role(http, EditRole::new().name(name).mentionable(true))
        .await?;
    Ok(role.id)
}

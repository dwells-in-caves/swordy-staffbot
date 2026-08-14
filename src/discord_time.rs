//! Discord timestamp rendering.

use chrono::{DateTime, Utc};

/// A Discord timestamp display style.
///
/// Only the styles the bot actually emits are represented. Add variants here
/// as new rendering needs appear. The full Discord set is `t T d D f F R`;
/// note that `d` is *Short Date* (date only) while `f` below is
/// *Short Date/Time* (date and time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsStyle {
    /// `f` — short date and time, e.g. "20 April 2026 13:00".
    ShortDateTime,
    /// `D` — long date, e.g. "20 April 2026" (no time-of-day).
    LongDate,
    /// `R` — relative time, e.g. "in 2 hours", "3 days ago".
    Relative,
}

impl TsStyle {
    /// The single-character Discord style code.
    const fn code(self) -> char {
        match self {
            TsStyle::ShortDateTime => 'f',
            TsStyle::LongDate => 'D',
            TsStyle::Relative => 'R',
        }
    }
}

/// Render a UTC instant as a Discord timestamp tag.
pub trait DiscordTimestamp {
    /// Produce a `<t:UNIX:STYLE>` tag that Discord renders in the viewer's
    /// local timezone.
    fn discord(&self, style: TsStyle) -> String;
}

impl DiscordTimestamp for DateTime<Utc> {
    fn discord(&self, style: TsStyle) -> String {
        format!("<t:{}:{}>", self.timestamp(), style.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn renders_expected_tags() {
        let dt = Utc.with_ymd_and_hms(2026, 4, 20, 13, 0, 0).unwrap();
        let unix = dt.timestamp();
        assert_eq!(dt.discord(TsStyle::ShortDateTime), format!("<t:{unix}:f>"));
        assert_eq!(dt.discord(TsStyle::Relative), format!("<t:{unix}:R>"));
    }
}
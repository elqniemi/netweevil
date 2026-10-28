use serde::{Deserialize, Serialize};

pub const MONDAY: u8 = 1 << 0;
pub const TUESDAY: u8 = 1 << 1;
pub const WEDNESDAY: u8 = 1 << 2;
pub const THURSDAY: u8 = 1 << 3;
pub const FRIDAY: u8 = 1 << 4;
pub const SATURDAY: u8 = 1 << 5;
pub const SUNDAY: u8 = 1 << 6;
pub const PUBLIC_HOLIDAY: u8 = 1 << 7;
/// Every calendar day, including dates selected by an attached public-holiday
/// calendar. Use the individual weekday bits when holidays should be excluded.
pub const EVERY_DAY: u8 =
    MONDAY | TUESDAY | WEDNESDAY | THURSDAY | FRIDAY | SATURDAY | SUNDAY | PUBLIC_HOLIDAY;
/// Monday through Sunday, without the public-holiday bit.
pub const ALL_WEEKDAYS: u8 = EVERY_DAY & !PUBLIC_HOLIDAY;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinuteInterval {
    /// Minute after midnight, inclusive.
    pub start_minute: u16,
    /// Minute after midnight, exclusive. `1440` is permitted. A start after
    /// the end denotes a midnight-wrapping interval.
    pub end_minute: u16,
}

impl MinuteInterval {
    pub fn contains(self, minute: u16) -> bool {
        if self.start_minute <= self.end_minute {
            minute >= self.start_minute && minute < self.end_minute
        } else {
            minute >= self.start_minute || minute < self.end_minute
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalEffect {
    Closed,
    OpenOnly,
    ForwardOnly,
    BackwardOnly,
    SpeedFactor(f32),
    /// Posted speed limit in km/h while the rule is active
    /// (`maxspeed:conditional`). Profile compilation resolves it into the
    /// profile's own [`TemporalEffect::SpeedFactor`], because the resulting
    /// travel time depends on the profile's speeds and posted-limit policy.
    MaxSpeed(f32),
}

/// A calendar date. `year` is `None` for dates that recur every year.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CalendarDate {
    pub year: Option<i32>,
    pub month: u8,
    /// Day of month; `31` also stands for "end of month".
    pub day: u8,
}

/// Inclusive date window. Both ends either carry a year (an absolute
/// window) or neither does (a yearly window, which may wrap the year end,
/// for example November 1 to April 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    pub start: CalendarDate,
    pub end: CalendarDate,
}

impl DateRange {
    pub fn contains(&self, year: i32, month: u8, day: u8) -> bool {
        match (self.start.year, self.end.year) {
            (Some(start_year), Some(end_year)) => {
                let date = (year, month, day);
                date >= (start_year, self.start.month, self.start.day)
                    && date <= (end_year, self.end.month, self.end.day)
            }
            _ => {
                let date = (month, day);
                let start = (self.start.month, self.start.day);
                let end = (self.end.month, self.end.day);
                if start <= end {
                    date >= start && date <= end
                } else {
                    date >= start || date <= end
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalRule {
    /// Monday is bit 0 through Sunday bit 6; public holiday is bit 7.
    pub day_mask: u8,
    #[serde(default)]
    pub intervals: Vec<MinuteInterval>,
    pub effect: TemporalEffect,
    /// Travel modes (`AccessMask` bits) the rule applies to; `0` applies it
    /// to every mode.
    #[serde(default)]
    pub modes: u16,
    /// Optional date window; the rule is inactive outside it.
    #[serde(default)]
    pub dates: Option<DateRange>,
    /// When true, a listed public holiday still matches its weekday bit, as
    /// in OSM opening hours that do not mention `PH`. When false (the
    /// default), the `PUBLIC_HOLIDAY` bit replaces the weekday bit on
    /// holidays.
    #[serde(default)]
    pub weekdays_include_holidays: bool,
}

impl TemporalRule {
    /// A rule active on `day_mask` within `intervals` for every mode.
    pub fn new(day_mask: u8, intervals: Vec<MinuteInterval>, effect: TemporalEffect) -> Self {
        Self {
            day_mask,
            intervals,
            effect,
            modes: 0,
            dates: None,
            weekdays_include_holidays: false,
        }
    }

    pub fn applies_to_mode(&self, mode_bit: u16) -> bool {
        self.modes == 0 || self.modes & mode_bit != 0
    }

    /// Whether the rule's day selection matches a date. `weekday_bit` is the
    /// date's weekday bit (`MONDAY` ...), `is_holiday` whether the attached
    /// calendar lists the date.
    pub fn matches_day(
        &self,
        weekday_bit: u8,
        is_holiday: bool,
        year: i32,
        month: u8,
        day: u8,
    ) -> bool {
        if self
            .dates
            .is_some_and(|dates| !dates.contains(year, month, day))
        {
            return false;
        }
        if is_holiday && !self.weekdays_include_holidays {
            self.day_mask & PUBLIC_HOLIDAY != 0
        } else {
            self.day_mask & weekday_bit != 0 || (is_holiday && self.day_mask & PUBLIC_HOLIDAY != 0)
        }
    }

    /// True when the rule restricts time at all: it does not hold on every
    /// day all day.
    pub fn is_time_varying(&self) -> bool {
        !self.intervals.is_empty() || self.day_mask != EVERY_DAY || self.dates.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TemporalRuleSet {
    pub rules: Vec<TemporalRule>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_midnight_wrapping_intervals() {
        let interval = MinuteInterval {
            start_minute: 22 * 60,
            end_minute: 2 * 60,
        };
        assert!(interval.contains(23 * 60));
        assert!(interval.contains(60));
        assert!(!interval.contains(12 * 60));
    }

    #[test]
    fn date_ranges_cover_absolute_and_year_wrapping_windows() {
        let yearly = DateRange {
            start: CalendarDate {
                year: None,
                month: 11,
                day: 1,
            },
            end: CalendarDate {
                year: None,
                month: 4,
                day: 1,
            },
        };
        assert!(yearly.contains(2026, 12, 24));
        assert!(yearly.contains(2027, 4, 1));
        assert!(!yearly.contains(2027, 4, 2));
        let absolute = DateRange {
            start: CalendarDate {
                year: Some(2025),
                month: 1,
                day: 13,
            },
            end: CalendarDate {
                year: Some(2025),
                month: 4,
                day: 4,
            },
        };
        assert!(absolute.contains(2025, 2, 1));
        assert!(!absolute.contains(2026, 2, 1));
    }

    #[test]
    fn holiday_matching_follows_rule_holiday_mode() {
        let weekday_rule = TemporalRule {
            weekdays_include_holidays: true,
            ..TemporalRule::new(MONDAY, Vec::new(), TemporalEffect::Closed)
        };
        let holiday_aware = TemporalRule::new(MONDAY, Vec::new(), TemporalEffect::Closed);
        assert!(weekday_rule.matches_day(MONDAY, true, 2026, 4, 6));
        assert!(!holiday_aware.matches_day(MONDAY, true, 2026, 4, 6));
        assert!(holiday_aware.matches_day(MONDAY, false, 2026, 4, 13));
    }
}

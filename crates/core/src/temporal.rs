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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalRule {
    /// Monday is bit 0 through Sunday bit 6; public holiday is bit 7.
    pub day_mask: u8,
    #[serde(default)]
    pub intervals: Vec<MinuteInterval>,
    pub effect: TemporalEffect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TemporalRuleSet {
    #[serde(default)]
    pub rules: Vec<TemporalRule>,
}

#[cfg(test)]
mod tests {
    use super::MinuteInterval;

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
}

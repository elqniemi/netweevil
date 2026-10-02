//! Historical time-of-week traffic speeds keyed by directed source feature.
//!
//! A bundle holds a set of interned weekly speed patterns plus one pattern
//! assignment per directed source feature (OSM way id or mapped feature id).
//! Edges are bound to patterns at load time through their
//! `source_way_id`/`source_direction`, so a bundle survives re-imports of the
//! same source data.

use serde::{Deserialize, Serialize};

/// Schema version of [`TrafficSpeedBundle`].
pub const TRAFFIC_BUNDLE_SCHEMA_VERSION: u32 = 1;

/// Stored speeds are tenths of a km/h.
pub const TRAFFIC_SPEED_UNITS_PER_KPH: f64 = 10.0;

/// A stored speed of zero marks a bucket without an observation; the
/// profile speed applies there.
pub const TRAFFIC_SPEED_NO_DATA: u16 = 0;

pub const MINUTES_PER_DAY: u32 = 24 * 60;
pub const DAYS_PER_WEEK: u32 = 7;

/// Geometry-relative travel direction of an assignment: `1` follows the
/// source geometry and `-1` traverses it backwards (the same convention as
/// `RoutingEdge::source_direction`). Edges of unknown orientation (`0`) use
/// the forward assignment.
pub const TRAFFIC_FORWARD: i8 = 1;
pub const TRAFFIC_BACKWARD: i8 = -1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrafficSpeedBundle {
    pub schema_version: u32,
    pub traffic_id: String,
    /// IANA time zone in which the time-of-week buckets are defined.
    pub timezone: String,
    /// Bucket width; divides 60 so that clock changes fall on boundaries.
    pub bucket_minutes: u16,
    /// Interned weekly patterns, [`Self::buckets_per_week`] values each,
    /// starting Monday 00:00 local time. Units of 0.1 km/h; zero is no data.
    pub pattern_speeds: Vec<u16>,
    /// Directed source features sorted by `(feature_id, direction)`.
    pub feature_ids: Vec<i64>,
    pub directions: Vec<i8>,
    pub feature_patterns: Vec<u32>,
}

impl TrafficSpeedBundle {
    pub fn buckets_per_day(&self) -> usize {
        (MINUTES_PER_DAY / u32::from(self.bucket_minutes.max(1))) as usize
    }

    pub fn buckets_per_week(&self) -> usize {
        self.buckets_per_day() * DAYS_PER_WEEK as usize
    }

    pub fn pattern_count(&self) -> usize {
        self.pattern_speeds.len() / self.buckets_per_week().max(1)
    }

    pub fn pattern(&self, pattern: u32) -> &[u16] {
        let width = self.buckets_per_week();
        let start = pattern as usize * width;
        &self.pattern_speeds[start..start + width]
    }

    /// The pattern of a directed source feature.
    pub fn lookup(&self, feature_id: i64, direction: i8) -> Option<u32> {
        let direction = if direction < 0 {
            TRAFFIC_BACKWARD
        } else {
            TRAFFIC_FORWARD
        };
        let start = self.feature_ids.partition_point(|id| *id < feature_id);
        (start..self.feature_ids.len())
            .take_while(|&index| self.feature_ids[index] == feature_id)
            .find(|&index| self.directions[index] == direction)
            .map(|index| self.feature_patterns[index])
    }

    /// Checks the structural invariants readers rely on.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != TRAFFIC_BUNDLE_SCHEMA_VERSION {
            return Err(format!(
                "traffic bundle schema {} is not the supported schema {TRAFFIC_BUNDLE_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.bucket_minutes == 0 || 60 % self.bucket_minutes != 0 {
            return Err(format!(
                "bucket_minutes must divide 60, got {}",
                self.bucket_minutes
            ));
        }
        let width = self.buckets_per_week();
        if self.pattern_speeds.len() % width != 0 {
            return Err("pattern speeds are not a whole number of weekly patterns".into());
        }
        let feature_count = self.feature_ids.len();
        if self.directions.len() != feature_count || self.feature_patterns.len() != feature_count {
            return Err("traffic feature columns have different lengths".into());
        }
        let pattern_count = self.pattern_count() as u32;
        if self
            .feature_patterns
            .iter()
            .any(|pattern| *pattern >= pattern_count)
        {
            return Err("traffic feature references a missing pattern".into());
        }
        if self
            .directions
            .iter()
            .any(|direction| *direction != TRAFFIC_FORWARD && *direction != TRAFFIC_BACKWARD)
        {
            return Err("traffic direction must be 1 (forward) or -1 (backward)".into());
        }
        let sorted = (1..feature_count).all(|index| {
            (self.feature_ids[index - 1], self.directions[index - 1])
                < (self.feature_ids[index], self.directions[index])
        });
        if !sorted {
            return Err(
                "traffic features must be sorted and unique by (feature_id, direction)".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

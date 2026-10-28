//! Lane guidance from OSM `turn:lanes` values.
//!
//! A value lists lanes left to right in the travel direction, separated by
//! `|`; each lane lists its indications separated by `;`. An empty lane or
//! `none` has no marking and counts as a through lane.

use serde::{Deserialize, Serialize};

/// One arrow painted on a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneIndication {
    None,
    Through,
    SharpLeft,
    Left,
    SlightLeft,
    SlightRight,
    Right,
    SharpRight,
    Reverse,
    MergeToLeft,
    MergeToRight,
}

impl LaneIndication {
    fn parse(value: &str) -> Option<Self> {
        Some(match value.trim() {
            "" | "none" => Self::None,
            "through" => Self::Through,
            "sharp_left" => Self::SharpLeft,
            "left" => Self::Left,
            "slight_left" => Self::SlightLeft,
            "slight_right" => Self::SlightRight,
            "right" => Self::Right,
            "sharp_right" => Self::SharpRight,
            "reverse" => Self::Reverse,
            "merge_to_left" => Self::MergeToLeft,
            "merge_to_right" => Self::MergeToRight,
            _ => return None,
        })
    }
}

/// One lane of a maneuver's approach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub indications: Vec<LaneIndication>,
    /// The lane leads into the maneuver.
    pub valid: bool,
    /// The lane is valid and also sets up the next maneuver when it
    /// follows closely.
    pub active: bool,
    /// The indication of this lane that the maneuver uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_indication: Option<LaneIndication>,
}

/// The movement a maneuver makes through its junction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneMovement {
    Through,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    /// U-turn toward the given side (left in right-hand traffic).
    Reverse {
        left: bool,
    },
    KeepLeft,
    KeepRight,
}

impl LaneMovement {
    /// Indications to look for, best first. The first one any lane carries
    /// decides which lanes are valid.
    fn preferences(self) -> &'static [LaneIndication] {
        use LaneIndication as I;
        match self {
            Self::Through => &[I::Through, I::None],
            Self::SlightLeft => &[I::SlightLeft, I::Left],
            Self::Left => &[I::Left, I::SlightLeft, I::SharpLeft],
            Self::SharpLeft => &[I::SharpLeft, I::Left],
            Self::SlightRight => &[I::SlightRight, I::Right],
            Self::Right => &[I::Right, I::SlightRight, I::SharpRight],
            Self::SharpRight => &[I::SharpRight, I::Right],
            Self::Reverse { left: true } => &[I::Reverse, I::SharpLeft, I::Left],
            Self::Reverse { left: false } => &[I::Reverse, I::SharpRight, I::Right],
            Self::KeepLeft => &[I::SlightLeft, I::Left, I::Through, I::None],
            Self::KeepRight => &[I::SlightRight, I::Right, I::Through, I::None],
        }
    }

    /// Side a closely following maneuver pulls toward.
    pub(crate) fn side(self) -> Option<bool> {
        match self {
            Self::SlightLeft | Self::Left | Self::SharpLeft | Self::KeepLeft => Some(true),
            Self::SlightRight | Self::Right | Self::SharpRight | Self::KeepRight => Some(false),
            Self::Reverse { left } => Some(left),
            Self::Through => None,
        }
    }
}

/// Parses a `turn:lanes` value. Unknown indications drop the whole value:
/// guessing lanes is worse than showing none.
pub(crate) fn parse_turn_lanes(value: &str) -> Option<Vec<Vec<LaneIndication>>> {
    let lanes = value
        .split('|')
        .map(|lane| {
            let mut indications = lane
                .split(';')
                .map(LaneIndication::parse)
                .collect::<Option<Vec<_>>>()?;
            indications.dedup();
            if indications.len() > 1 {
                indications.retain(|indication| *indication != LaneIndication::None);
            }
            Some(indications)
        })
        .collect::<Option<Vec<_>>>()?;
    (lanes.len() > 1
        || lanes
            .first()
            .is_some_and(|lane| lane[0] != LaneIndication::None))
    .then_some(lanes)
}

/// Marks the lanes valid for `movement`. When `next` (the following
/// maneuver's movement) turns to one side, only the valid lane furthest to
/// that side is active.
pub(crate) fn lane_guidance(
    lanes: &[Vec<LaneIndication>],
    movement: LaneMovement,
    next: Option<LaneMovement>,
) -> Vec<Lane> {
    let chosen = movement
        .preferences()
        .iter()
        .copied()
        .find(|indication| lanes.iter().any(|lane| lane.contains(indication)));
    // An unmarked lane continues straight, so "through" also accepts it.
    let accepts = |lane: &[LaneIndication]| -> Option<LaneIndication> {
        let chosen = chosen?;
        if lane.contains(&chosen) {
            Some(chosen)
        } else if chosen == LaneIndication::Through && lane.contains(&LaneIndication::None) {
            Some(LaneIndication::None)
        } else {
            None
        }
    };
    let valid = lanes.iter().map(|lane| accepts(lane)).collect::<Vec<_>>();
    let positions = valid
        .iter()
        .enumerate()
        .filter_map(|(index, indication)| indication.map(|_| index))
        .collect::<Vec<_>>();
    let active_only = match next.and_then(LaneMovement::side) {
        Some(true) => positions.first().copied(),
        Some(false) => positions.last().copied(),
        None => None,
    };
    lanes
        .iter()
        .zip(&valid)
        .enumerate()
        .map(|(index, (indications, valid_indication))| Lane {
            indications: indications.clone(),
            valid: valid_indication.is_some(),
            active: valid_indication.is_some() && active_only.is_none_or(|only| only == index),
            valid_indication: *valid_indication,
        })
        .collect()
}

#[cfg(test)]
mod tests;

//! Per-mode, per-direction OSM access resolution.
//!
//! Each mode walks its access hierarchy from the most specific key to the
//! most general (`hgv`, `motor_vehicle`, `vehicle`, `access`), checking the
//! direction-scoped key (`hgv:forward`) before the plain one. The first key
//! with a recognized value decides the unconditional ("static") access;
//! without one the highway default applies.
//!
//! `:conditional` variants of the keys walked before that decision override
//! it while their condition holds. Time conditions become temporal rules
//! scoped to the mode (static routing ignores them; time-dependent routing
//! evaluates them). Vehicle-property conditions (`no @ (weight>7.5)`) become
//! physical vehicle restrictions. Everything else is skipped and counted.

use netweevil_core::{
    AccessMask, EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    HighwayClass, RoadClass, TemporalEffect, TemporalRule, VehicleRestriction,
};
use osmpbfreader::Tags;

use crate::classify::tag;
use crate::conditional::{Condition, parse_conditional};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeDirection {
    Both,
    ForwardOnly,
    ReverseOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heading {
    Forward,
    Backward,
}

impl Heading {
    const fn suffix(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
        }
    }

    const fn allowed_by(self, direction: EdgeDirection) -> bool {
        match (self, direction) {
            (_, EdgeDirection::Both) => true,
            (Self::Forward, EdgeDirection::ForwardOnly) => true,
            (Self::Backward, EdgeDirection::ReverseOnly) => true,
            _ => false,
        }
    }
}

/// Access of one travel direction of a way.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DirectionAccess {
    pub(crate) access: AccessMask,
    pub(crate) destination_only: AccessMask,
    pub(crate) conditional_access: AccessMask,
    pub(crate) flags: u32,
    /// Mode-scoped time rules from `:conditional` tags.
    pub(crate) rules: Vec<TemporalRule>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct WayAccess {
    pub(crate) forward: DirectionAccess,
    pub(crate) reverse: DirectionAccess,
    /// Restrictions from vehicle-property conditions.
    pub(crate) vehicle_restrictions: Vec<VehicleRestriction>,
    pub(crate) conditionals: ConditionalCounts,
}

/// `:conditional` entries imported and skipped, for import diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ConditionalCounts {
    pub(crate) imported: u64,
    pub(crate) skipped: u64,
}

impl std::ops::AddAssign for ConditionalCounts {
    fn add_assign(&mut self, other: Self) {
        self.imported += other.imported;
        self.skipped += other.skipped;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessClass {
    Allowed,
    DestinationOnly,
    Denied,
}

impl AccessClass {
    const fn is_allowed(self) -> bool {
        !matches!(self, Self::Denied)
    }
}

/// Classifies an access value; `None` for values that say nothing about
/// the mode (`unknown`, `dismount`), so the walk continues.
fn classify_access_value(value: &str) -> Option<AccessClass> {
    match value.trim() {
        "yes" | "designated" | "official" | "permissive" | "discouraged" => {
            Some(AccessClass::Allowed)
        }
        "destination" | "delivery" | "customers" => Some(AccessClass::DestinationOnly),
        "no" | "private" | "agricultural" | "forestry" | "use_sidepath" | "permit"
        | "emergency" | "military" => Some(AccessClass::Denied),
        _ => None,
    }
}

const MOTOR_MODES: [u16; 6] = [
    AccessMask::CAR,
    AccessMask::HGV,
    AccessMask::BUS,
    AccessMask::TAXI,
    AccessMask::MOTORCYCLE,
    AccessMask::MOPED,
];

/// Access keys of a mode from most to least specific.
fn hierarchy(mode: u16) -> &'static [&'static str] {
    match mode {
        AccessMask::CAR => &["motorcar", "motor_vehicle", "vehicle", "access"],
        AccessMask::HGV => &["hgv", "motor_vehicle", "vehicle", "access"],
        AccessMask::BUS => &["bus", "psv", "motor_vehicle", "vehicle", "access"],
        AccessMask::TAXI => &["taxi", "psv", "motor_vehicle", "vehicle", "access"],
        AccessMask::MOTORCYCLE => &["motorcycle", "motor_vehicle", "vehicle", "access"],
        AccessMask::MOPED => &["moped", "motor_vehicle", "vehicle", "access"],
        AccessMask::BICYCLE => &["bicycle", "vehicle", "access"],
        AccessMask::FOOT => &["foot", "access"],
        _ => &["access"],
    }
}

/// Mode-specific oneway keys overriding the generic `oneway`.
fn oneway_keys(mode: u16) -> &'static [&'static str] {
    match mode {
        AccessMask::CAR => &["oneway:motorcar"],
        AccessMask::HGV => &["oneway:hgv"],
        AccessMask::BUS => &["oneway:bus", "oneway:psv"],
        AccessMask::TAXI => &["oneway:taxi", "oneway:psv"],
        AccessMask::MOTORCYCLE => &["oneway:motorcycle"],
        AccessMask::MOPED => &["oneway:moped"],
        _ => &[],
    }
}

/// Modes allowed on a highway class before any access tag.
pub(crate) fn default_access(tags: &Tags, highway: HighwayClass) -> u16 {
    let mut bits = match highway {
        HighwayClass::Motorway | HighwayClass::MotorwayLink => {
            AccessMask::MOTOR_VEHICLES & !AccessMask::MOPED
        }
        HighwayClass::Trunk
        | HighwayClass::TrunkLink
        | HighwayClass::Primary
        | HighwayClass::PrimaryLink
        | HighwayClass::Secondary
        | HighwayClass::SecondaryLink
        | HighwayClass::Tertiary
        | HighwayClass::TertiaryLink
        | HighwayClass::Residential
        | HighwayClass::Unclassified
        | HighwayClass::LivingStreet
        | HighwayClass::Service
        | HighwayClass::Track
        | HighwayClass::Ferry
        | HighwayClass::Unknown => AccessMask::ALL,
        HighwayClass::Path
        | HighwayClass::Footway
        | HighwayClass::Pedestrian
        | HighwayClass::Steps
        | HighwayClass::Bridleway => AccessMask::FOOT,
        HighwayClass::Cycleway => AccessMask::BICYCLE | AccessMask::FOOT,
        HighwayClass::Busway => AccessMask::BUS,
    };
    if tag(tags, "motorroad") == Some("yes") {
        bits &= !(AccessMask::FOOT | AccessMask::BICYCLE | AccessMask::MOPED);
    }
    bits
}

/// Resolves every mode in both directions of a way.
pub(crate) fn classify_way_access(
    tags: &Tags,
    highway: HighwayClass,
    road_class: RoadClass,
) -> WayAccess {
    let defaults = default_access(tags, highway);
    let generic_direction = classify_direction(tags, road_class);
    let mut way = WayAccess::default();

    for mode in MOTOR_MODES {
        let direction = explicit_oneway_direction(tags, oneway_keys(mode))
            .or_else(|| {
                (mode == AccessMask::BUS && has_bus_opposite(tags)).then_some(EdgeDirection::Both)
            })
            .unwrap_or(generic_direction);
        for heading in [Heading::Forward, Heading::Backward] {
            if !heading.allowed_by(direction) {
                continue;
            }
            let resolution = resolve_mode(tags, mode, Some(heading), defaults & mode != 0);
            let target = match heading {
                Heading::Forward => &mut way.forward,
                Heading::Backward => &mut way.reverse,
            };
            // Vehicle limits are direction-independent; collect them once.
            let limits = if heading == Heading::Forward || !Heading::Forward.allowed_by(direction) {
                Some(&mut way.vehicle_restrictions)
            } else {
                None
            };
            apply_resolution(target, mode, &resolution, limits, &mut way.conditionals);
        }
    }

    classify_foot_and_bicycle(tags, highway, defaults, generic_direction, &mut way);
    for direction in [&mut way.forward, &mut way.reverse] {
        merge_rule_modes(&mut direction.rules);
    }
    way
}

struct ModeResolution {
    class: AccessClass,
    /// Time-conditional overrides in priority order: the first active one
    /// decides. `true` means the override allows the mode.
    overrides: Vec<(Condition, bool)>,
    vehicle_limits: Vec<netweevil_core::VehicleRestrictionKind>,
    counts: ConditionalCounts,
}

fn resolve_mode(
    tags: &Tags,
    mode: u16,
    heading: Option<Heading>,
    default_allowed: bool,
) -> ModeResolution {
    let mut resolution = ModeResolution {
        class: if default_allowed {
            AccessClass::Allowed
        } else {
            AccessClass::Denied
        },
        overrides: Vec::new(),
        vehicle_limits: Vec::new(),
        counts: ConditionalCounts::default(),
    };
    for key in hierarchy(mode) {
        let scoped = heading.map(|heading| format!("{key}:{}", heading.suffix()));
        for key in scoped.iter().map(String::as_str).chain([*key]) {
            if let Some(raw) = tag(tags, &format!("{key}:conditional")) {
                collect_conditional(raw, &mut resolution);
            }
            if let Some(class) = tag(tags, key).and_then(classify_access_value) {
                resolution.class = class;
                return resolution;
            }
        }
    }
    resolution
}

fn collect_conditional(raw: &str, resolution: &mut ModeResolution) {
    let Some(entries) = parse_conditional(raw) else {
        resolution.counts.skipped += 1;
        return;
    };
    // The last matching entry of a key wins, so later entries take priority.
    for entry in entries.into_iter().rev() {
        match (classify_access_value(entry.value), entry.condition) {
            (Some(AccessClass::Denied), Condition::VehicleAbove(limit)) => {
                resolution.vehicle_limits.push(limit);
                resolution.counts.imported += 1;
            }
            (Some(AccessClass::Allowed), condition @ Condition::Time(_)) => {
                resolution.overrides.push((condition, true));
            }
            (Some(AccessClass::Denied), condition @ Condition::Time(_)) => {
                resolution.overrides.push((condition, false));
            }
            // Destination-only windows (`delivery @ (05:00-12:00)`) would
            // need a time-dependent entry penalty; the unconditional tags
            // keep deciding instead.
            _ => resolution.counts.skipped += 1,
        }
    }
}

/// Folds one mode's resolution into a direction. Overrides that agree with
/// the static access only matter where they shadow a later override of the
/// opposite polarity; first-match precedence of mixed polarities cannot be
/// expressed as a union of rules, so such a mode keeps its static access.
fn apply_resolution(
    target: &mut DirectionAccess,
    mode: u16,
    resolution: &ModeResolution,
    vehicle_limits: Option<&mut Vec<VehicleRestriction>>,
    counts: &mut ConditionalCounts,
) {
    let allowed = resolution.class.is_allowed();
    if allowed {
        target.access.0 |= mode;
        if resolution.class == AccessClass::DestinationOnly {
            target.destination_only.0 |= mode;
        }
    }
    if let Some(vehicle_limits) = vehicle_limits {
        *counts += resolution.counts;
        for &kind in &resolution.vehicle_limits {
            push_restriction(vehicle_limits, mode, kind);
        }
    }

    let mut overrides = resolution.overrides.as_slice();
    while let Some(((_, last_allows), rest)) = overrides.split_last()
        && *last_allows == allowed
    {
        overrides = rest;
    }
    if overrides.iter().any(|(_, allows)| *allows == allowed) {
        return;
    }
    let effect = if allowed {
        TemporalEffect::Closed
    } else {
        TemporalEffect::OpenOnly
    };
    let mut imported = false;
    for (condition, _) in overrides {
        if let Some(rules) = condition.time_rules(effect, mode) {
            target.rules.extend(rules);
            imported = true;
        }
    }
    if imported && !allowed {
        target.conditional_access.0 |= mode;
    }
}

fn push_restriction(
    restrictions: &mut Vec<VehicleRestriction>,
    mode: u16,
    kind: netweevil_core::VehicleRestrictionKind,
) {
    if let Some(existing) = restrictions
        .iter_mut()
        .find(|restriction| restriction.kind == kind)
    {
        existing.modes.0 |= mode;
    } else {
        restrictions.push(VehicleRestriction {
            modes: AccessMask::new(mode),
            kind,
        });
    }
}

/// Rules that differ only in mode merge into one rule for all of them.
fn merge_rule_modes(rules: &mut Vec<TemporalRule>) {
    let mut merged: Vec<TemporalRule> = Vec::with_capacity(rules.len());
    for rule in rules.drain(..) {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            TemporalRule {
                modes: rule.modes,
                ..(*existing).clone()
            } == rule
        }) {
            existing.modes |= rule.modes;
        } else {
            merged.push(rule);
        }
    }
    *rules = merged;
}

/// Foot and bicycle keep their oneway semantics: pedestrians ignore plain
/// oneways (flagged so profiles can opt in), and cyclists get inferred
/// contraflow on ordinary roads unless tagged otherwise.
fn classify_foot_and_bicycle(
    tags: &Tags,
    highway: HighwayClass,
    defaults: u16,
    generic_direction: EdgeDirection,
    way: &mut WayAccess,
) {
    for mode in [AccessMask::FOOT, AccessMask::BICYCLE] {
        let resolution = resolve_mode(tags, mode, None, defaults & mode != 0);
        let (forward, reverse, forward_flags, reverse_flags) =
            soft_mode_directions(tags, highway, mode, generic_direction);
        let mut limits = Some(&mut way.vehicle_restrictions);
        for (included, flags, target) in [
            (forward, forward_flags, &mut way.forward),
            (reverse, reverse_flags, &mut way.reverse),
        ] {
            if !included {
                continue;
            }
            apply_resolution(
                target,
                mode,
                &resolution,
                limits.take(),
                &mut way.conditionals,
            );
            if target.access.contains(mode) || target.conditional_access.contains(mode) {
                target.flags |= flags;
            }
        }
    }
}

/// Directions a foot or bicycle edge exists in, with inferred-direction
/// flags, assuming the mode is allowed at all.
fn soft_mode_directions(
    tags: &Tags,
    highway: HighwayClass,
    mode: u16,
    generic_direction: EdgeDirection,
) -> (bool, bool, u32, u32) {
    let both = |direction: EdgeDirection| {
        (
            Heading::Forward.allowed_by(direction),
            Heading::Backward.allowed_by(direction),
            0,
            0,
        )
    };
    let inferred = |flag: u32| match generic_direction {
        EdgeDirection::Both => (true, true, 0, 0),
        EdgeDirection::ForwardOnly => (true, true, 0, flag),
        EdgeDirection::ReverseOnly => (true, true, flag, 0),
    };
    if mode == AccessMask::FOOT {
        return match explicit_oneway_direction(tags, &["oneway:foot", "oneway:pedestrian"])
            .or_else(|| access_forward_backward_direction(tags, "foot"))
        {
            Some(direction) => both(direction),
            None => inferred(EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY),
        };
    }
    if let Some(direction) = explicit_oneway_direction(tags, &["oneway:bicycle", "bicycle:oneway"])
        .or_else(|| access_forward_backward_direction(tags, "bicycle"))
    {
        both(direction)
    } else if has_bicycle_opposite(tags) {
        both(EdgeDirection::Both)
    } else if highway == HighwayClass::Cycleway {
        both(generic_direction)
    } else if is_ordinary_bicycle_contraflow_road(highway) {
        inferred(EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW)
    } else {
        both(generic_direction)
    }
}

pub(crate) fn classify_direction(tags: &Tags, road_class: RoadClass) -> EdgeDirection {
    match tag(tags, "oneway") {
        Some("-1") => EdgeDirection::ReverseOnly,
        Some("yes" | "true" | "1") => EdgeDirection::ForwardOnly,
        Some("no" | "false" | "0") => EdgeDirection::Both,
        _ if road_class == RoadClass::Motorway => EdgeDirection::ForwardOnly,
        _ if tag(tags, "junction") == Some("roundabout") => EdgeDirection::ForwardOnly,
        _ => EdgeDirection::Both,
    }
}

fn explicit_oneway_direction(tags: &Tags, keys: &[&str]) -> Option<EdgeDirection> {
    keys.iter()
        .find_map(|key| tag(tags, key).and_then(parse_oneway_direction))
}

fn parse_oneway_direction(value: &str) -> Option<EdgeDirection> {
    match value {
        "-1" | "reverse" => Some(EdgeDirection::ReverseOnly),
        "yes" | "true" | "1" => Some(EdgeDirection::ForwardOnly),
        "no" | "false" | "0" => Some(EdgeDirection::Both),
        _ => None,
    }
}

fn access_forward_backward_direction(tags: &Tags, prefix: &str) -> Option<EdgeDirection> {
    let forward_key = format!("{prefix}:forward");
    let backward_key = format!("{prefix}:backward");
    let forward_allowed = tag(tags, &forward_key).map(|value| !is_access_no(value));
    let backward_allowed = tag(tags, &backward_key).map(|value| !is_access_no(value));
    match (forward_allowed, backward_allowed) {
        (Some(true), Some(true)) => Some(EdgeDirection::Both),
        (Some(true), Some(false)) | (Some(true), None) | (None, Some(false)) => {
            Some(EdgeDirection::ForwardOnly)
        }
        (Some(false), Some(true)) | (Some(false), None) | (None, Some(true)) => {
            Some(EdgeDirection::ReverseOnly)
        }
        (Some(false), Some(false)) => Some(EdgeDirection::Both),
        (None, None) => None,
    }
}

fn is_access_no(value: &str) -> bool {
    matches!(value, "no" | "private")
}

const OPPOSITE_VALUES: [&str; 4] = [
    "opposite",
    "opposite_lane",
    "opposite_track",
    "opposite_share_busway",
];

fn has_bicycle_opposite(tags: &Tags) -> bool {
    ["cycleway", "cycleway:left", "cycleway:right"]
        .iter()
        .any(|key| tag(tags, key).is_some_and(|value| OPPOSITE_VALUES.contains(&value)))
}

/// A contraflow bus lane (`busway=opposite_lane`) opens the reverse
/// direction of a oneway to buses.
fn has_bus_opposite(tags: &Tags) -> bool {
    ["busway", "busway:left", "busway:right"]
        .iter()
        .any(|key| matches!(tag(tags, key), Some("opposite_lane" | "opposite_track")))
}

fn is_ordinary_bicycle_contraflow_road(highway: HighwayClass) -> bool {
    matches!(
        highway,
        HighwayClass::Primary
            | HighwayClass::PrimaryLink
            | HighwayClass::Secondary
            | HighwayClass::SecondaryLink
            | HighwayClass::Tertiary
            | HighwayClass::TertiaryLink
            | HighwayClass::Residential
            | HighwayClass::Unclassified
            | HighwayClass::LivingStreet
            | HighwayClass::Service
            | HighwayClass::Track
    )
}

#[cfg(test)]
mod tests;

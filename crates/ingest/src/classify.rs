use netweevil_core::{
    HighwayClass, RoadClass, SmoothnessClass, SurfaceClass, TemporalEffect, TemporalRule,
};
use osmpbfreader::Tags;

use crate::access::{ConditionalCounts, EdgeDirection, classify_direction};
use crate::conditional::{Condition, parse_conditional};

/// Road and highway class of a routable way or ferry route.
pub(crate) fn classify_way(tags: &Tags) -> Option<(RoadClass, HighwayClass)> {
    if let Some(highway) = tag(tags, "highway") {
        let highway_class = classify_highway(highway)?;
        return Some((highway_class.road_class(), highway_class));
    }

    if tag(tags, "route") == Some("ferry") || tag(tags, "ferry").is_some() {
        return Some((RoadClass::Ferry, HighwayClass::Ferry));
    }

    None
}

fn classify_highway(highway: &str) -> Option<HighwayClass> {
    match highway {
        "motorway" => Some(HighwayClass::Motorway),
        "motorway_link" => Some(HighwayClass::MotorwayLink),
        "trunk" => Some(HighwayClass::Trunk),
        "trunk_link" => Some(HighwayClass::TrunkLink),
        "primary" => Some(HighwayClass::Primary),
        "primary_link" => Some(HighwayClass::PrimaryLink),
        "secondary" => Some(HighwayClass::Secondary),
        "secondary_link" => Some(HighwayClass::SecondaryLink),
        "tertiary" => Some(HighwayClass::Tertiary),
        "tertiary_link" => Some(HighwayClass::TertiaryLink),
        "residential" => Some(HighwayClass::Residential),
        "unclassified" => Some(HighwayClass::Unclassified),
        "living_street" => Some(HighwayClass::LivingStreet),
        "service" => Some(HighwayClass::Service),
        "track" => Some(HighwayClass::Track),
        "path" => Some(HighwayClass::Path),
        "cycleway" => Some(HighwayClass::Cycleway),
        "footway" => Some(HighwayClass::Footway),
        "pedestrian" => Some(HighwayClass::Pedestrian),
        "steps" => Some(HighwayClass::Steps),
        "bridleway" => Some(HighwayClass::Bridleway),
        "busway" => Some(HighwayClass::Busway),
        _ => None,
    }
}

pub(crate) fn classify_surface(tags: &Tags) -> SurfaceClass {
    match tag(tags, "surface") {
        Some("asphalt") => SurfaceClass::Asphalt,
        Some("paved") | Some("concrete") | Some("concrete:lanes") | Some("concrete:plates") => {
            SurfaceClass::Paved
        }
        Some("gravel") | Some("fine_gravel") | Some("pebblestone") => SurfaceClass::Gravel,
        Some("cobblestone") | Some("sett") | Some("unhewn_cobblestone") => {
            SurfaceClass::Cobblestone
        }
        Some("ground") | Some("earth") | Some("grass") | Some("mud") => SurfaceClass::Ground,
        Some("dirt") | Some("compacted") => SurfaceClass::Dirt,
        Some("sand") => SurfaceClass::Sand,
        _ => SurfaceClass::Unknown,
    }
}

pub(crate) fn classify_smoothness(tags: &Tags) -> SmoothnessClass {
    match tag(tags, "smoothness") {
        Some("excellent") => SmoothnessClass::Excellent,
        Some("good") => SmoothnessClass::Good,
        Some("intermediate") => SmoothnessClass::Intermediate,
        Some("bad") => SmoothnessClass::Bad,
        Some("very_bad") => SmoothnessClass::VeryBad,
        Some("horrible") => SmoothnessClass::Horrible,
        Some("very_horrible") => SmoothnessClass::VeryHorrible,
        Some("impassable") => SmoothnessClass::Impassable,
        _ => SmoothnessClass::Unknown,
    }
}

pub(crate) fn classify_toll(tags: &Tags) -> bool {
    matches!(tag(tags, "toll"), Some("yes" | "true" | "1"))
}

pub(crate) fn is_traffic_signal_node(tags: &Tags) -> bool {
    tag(tags, "highway") == Some("traffic_signals")
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DirectionalSpeedLanes {
    pub(crate) forward_max_speed_kph: Option<f32>,
    pub(crate) reverse_max_speed_kph: Option<f32>,
    pub(crate) forward_lanes: Option<u8>,
    pub(crate) reverse_lanes: Option<u8>,
}

pub(crate) fn classify_speed_and_lanes(
    tags: &Tags,
    road_class: RoadClass,
) -> DirectionalSpeedLanes {
    let generic_direction = classify_direction(tags, road_class);
    let shared_max_speed = tag(tags, "maxspeed").and_then(parse_maxspeed_kph);
    let forward_max_speed_kph = tag(tags, "maxspeed:forward")
        .and_then(parse_maxspeed_kph)
        .or(shared_max_speed);
    let reverse_max_speed_kph = tag(tags, "maxspeed:backward")
        .and_then(parse_maxspeed_kph)
        .or(shared_max_speed);

    let explicit_forward_lanes = tag(tags, "lanes:forward").and_then(parse_lane_count);
    let explicit_reverse_lanes = tag(tags, "lanes:backward").and_then(parse_lane_count);
    let total_lanes = tag(tags, "lanes").and_then(parse_lane_count);
    let (forward_lanes, reverse_lanes) = match (explicit_forward_lanes, explicit_reverse_lanes) {
        (None, None) => split_total_lanes(total_lanes, generic_direction),
        (forward, reverse) => (
            forward.or_else(|| {
                total_lanes.map(|total| total.saturating_sub(reverse.unwrap_or(0)).max(1))
            }),
            reverse.or_else(|| {
                total_lanes.map(|total| total.saturating_sub(forward.unwrap_or(0)).max(1))
            }),
        ),
    };

    DirectionalSpeedLanes {
        forward_max_speed_kph,
        reverse_max_speed_kph,
        forward_lanes,
        reverse_lanes,
    }
}

fn split_total_lanes(total: Option<u8>, direction: EdgeDirection) -> (Option<u8>, Option<u8>) {
    let Some(total) = total else {
        return (None, None);
    };
    match direction {
        EdgeDirection::ForwardOnly => (Some(total), None),
        EdgeDirection::ReverseOnly => (None, Some(total)),
        EdgeDirection::Both => (Some(total.div_ceil(2)), Some((total / 2).max(1))),
    }
}

pub(crate) fn parse_maxspeed_kph(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(mph) = value
        .strip_suffix("mph")
        .map(str::trim)
        .and_then(|number| number.parse::<f32>().ok())
    {
        return positive_speed(mph * 1.609_344);
    }
    if let Some(knots) = value
        .strip_suffix("knots")
        .map(str::trim)
        .and_then(|number| number.parse::<f32>().ok())
    {
        return positive_speed(knots * 1.852);
    }
    // Non-numeric values ("none", "signals", "walk", zone refs) carry no
    // usable posted limit.
    value.parse::<f32>().ok().and_then(positive_speed)
}

/// Posted-speed rules from `maxspeed:conditional` and its `:forward` /
/// `:backward` forms, per direction. Only single time-conditional entries
/// import: several entries could overlap, and overlapping speed rules
/// would compound.
pub(crate) fn conditional_speed_rules(
    tags: &Tags,
) -> (Vec<TemporalRule>, Vec<TemporalRule>, ConditionalCounts) {
    let mut counts = ConditionalCounts::default();
    let mut parse = |key: &str| {
        let raw = tag(tags, key)?;
        let rules = maxspeed_rules(raw);
        if rules.is_some() {
            counts.imported += 1;
        } else {
            counts.skipped += 1;
        }
        Some(rules.unwrap_or_default())
    };
    let shared = parse("maxspeed:conditional");
    let forward = parse("maxspeed:forward:conditional").or_else(|| shared.clone());
    let backward = parse("maxspeed:backward:conditional").or(shared);
    (
        forward.unwrap_or_default(),
        backward.unwrap_or_default(),
        counts,
    )
}

fn maxspeed_rules(raw: &str) -> Option<Vec<TemporalRule>> {
    let entries = parse_conditional(raw)?;
    let [entry] = entries.as_slice() else {
        return None;
    };
    let kph = parse_maxspeed_kph(entry.value)?;
    match &entry.condition {
        condition @ Condition::Time(_) => condition.time_rules(
            TemporalEffect::MaxSpeed(kph),
            netweevil_core::AccessMask::MOTOR_VEHICLES,
        ),
        _ => None,
    }
}

fn positive_speed(kph: f32) -> Option<f32> {
    (kph.is_finite() && kph > 0.0).then_some(kph)
}

fn parse_lane_count(value: &str) -> Option<u8> {
    let lanes = value.trim().parse::<f32>().ok()?;
    if !(lanes.is_finite() && lanes >= 1.0) {
        return None;
    }
    Some(lanes.round().min(u8::MAX as f32) as u8)
}

pub(crate) fn parse_duration_from_tags(tags: &Tags) -> Option<f64> {
    tag(tags, "duration:seconds")
        .and_then(|value| value.parse::<f64>().ok())
        .or_else(|| tag(tags, "duration").and_then(parse_duration_seconds))
}

fn parse_duration_seconds(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    raw.parse::<f64>()
        .ok()
        .or_else(|| parse_iso8601_duration(raw))
        .or_else(|| parse_clock_duration(raw))
        .or_else(|| parse_unit_duration(raw))
}

fn parse_clock_duration(raw: &str) -> Option<f64> {
    let parts: Vec<_> = raw.split(':').collect();
    match parts.as_slice() {
        [hours, minutes] => {
            let hours = hours.parse::<f64>().ok()?;
            let minutes = minutes.parse::<f64>().ok()?;
            Some(hours * 3600.0 + minutes * 60.0)
        }
        [hours, minutes, seconds] => {
            let hours = hours.parse::<f64>().ok()?;
            let minutes = minutes.parse::<f64>().ok()?;
            let seconds = seconds.parse::<f64>().ok()?;
            Some(hours * 3600.0 + minutes * 60.0 + seconds)
        }
        _ => None,
    }
}

fn parse_iso8601_duration(raw: &str) -> Option<f64> {
    let mut chars = raw.trim().chars().peekable();
    if !chars.next()?.eq_ignore_ascii_case(&'P') {
        return None;
    }

    let mut total_seconds = 0.0;
    let mut in_time = false;
    while chars.peek().is_some() {
        if chars.peek().is_some_and(|ch| ch.eq_ignore_ascii_case(&'T')) {
            chars.next();
            in_time = true;
            continue;
        }

        let value = parse_number(&mut chars)?;
        let unit = chars.next()?.to_ascii_uppercase();
        let multiplier = match unit {
            'D' => 86_400.0,
            'H' if in_time => 3_600.0,
            'M' if in_time => 60.0,
            'S' if in_time => 1.0,
            _ => return None,
        };
        total_seconds += value * multiplier;
    }

    Some(total_seconds)
}

fn parse_unit_duration(raw: &str) -> Option<f64> {
    let mut chars = raw.trim().chars().peekable();
    let mut total_seconds = 0.0;
    let mut consumed_any = false;

    while chars.peek().is_some() {
        skip_whitespace(&mut chars);
        if chars.peek().is_none() {
            break;
        }

        let value = parse_number(&mut chars)?;
        skip_whitespace(&mut chars);

        let mut unit = String::new();
        while let Some(ch) = chars.peek().copied() {
            if ch.is_ascii_alphabetic() {
                unit.push(ch.to_ascii_lowercase());
                chars.next();
            } else {
                break;
            }
        }
        if unit.is_empty() {
            return None;
        }

        let multiplier = match unit.as_str() {
            "d" | "day" | "days" => 86_400.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            _ => return None,
        };
        total_seconds += value * multiplier;
        consumed_any = true;

        skip_whitespace(&mut chars);
    }

    consumed_any.then_some(total_seconds)
}

fn parse_number<I>(chars: &mut std::iter::Peekable<I>) -> Option<f64>
where
    I: Iterator<Item = char>,
{
    let mut token = String::new();
    while let Some(ch) = chars.peek().copied() {
        if ch.is_ascii_digit() || ch == '.' {
            token.push(ch);
            chars.next();
        } else {
            break;
        }
    }

    (!token.is_empty())
        .then(|| token.parse::<f64>().ok())
        .flatten()
}

fn skip_whitespace<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
        chars.next();
    }
}

pub(crate) fn tag<'a>(tags: &'a Tags, key: &str) -> Option<&'a str> {
    tags.get(key).map(|value| value.as_str())
}

#[cfg(test)]
mod tests;

use netweevil_core::{
    AccessMask, EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    HighwayClass, RoadClass, SmoothnessClass, SurfaceClass,
};
use osmpbfreader::Tags;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeDirection {
    Both,
    ForwardOnly,
    ReverseOnly,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DirectionalAccess {
    pub(crate) forward_access: AccessMask,
    pub(crate) reverse_access: AccessMask,
    pub(crate) forward_flags: u32,
    pub(crate) reverse_flags: u32,
}

pub(crate) fn classify_way(tags: &Tags) -> Option<(RoadClass, HighwayClass, AccessMask)> {
    if let Some(highway) = tag(tags, "highway") {
        let highway_class = classify_highway(highway)?;
        let road_class = highway_class.road_class();
        return Some((
            road_class,
            highway_class,
            classify_access(tags, highway, tag(tags, "route")),
        ));
    }

    if tag(tags, "route") == Some("ferry") || tag(tags, "ferry").is_some() {
        return Some((
            RoadClass::Ferry,
            HighwayClass::Ferry,
            classify_access(tags, "", Some("ferry")),
        ));
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

fn classify_access(tags: &Tags, highway: &str, route: Option<&str>) -> AccessMask {
    if route == Some("ferry") {
        return apply_access_overrides(
            tags,
            AccessMask::CAR
                | AccessMask::BICYCLE
                | AccessMask::FOOT
                | AccessMask::TRANSIT
                | AccessMask::HGV,
        );
    }

    let bits = match highway {
        "motorway" | "motorway_link" => AccessMask::CAR | AccessMask::HGV,
        "trunk" | "trunk_link" | "primary" | "primary_link" | "secondary" | "secondary_link"
        | "tertiary" | "tertiary_link" | "residential" | "unclassified" | "living_street"
        | "service" => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT | AccessMask::HGV,
        "track" => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT | AccessMask::HGV,
        "path" | "footway" | "pedestrian" | "steps" => AccessMask::FOOT,
        "cycleway" => AccessMask::BICYCLE | AccessMask::FOOT,
        "bridleway" => AccessMask::FOOT,
        _ => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT,
    };
    apply_access_overrides(tags, bits)
}

fn apply_access_overrides(tags: &Tags, base_bits: u16) -> AccessMask {
    let mut bits = base_bits;

    if tag_is_restricted(tags, "access") {
        bits = 0;
    }
    if tag_is_restricted(tags, "vehicle") {
        bits &= !(AccessMask::CAR | AccessMask::BICYCLE | AccessMask::TRANSIT | AccessMask::HGV);
    }
    if tag_is_restricted(tags, "motor_vehicle") {
        bits &= !(AccessMask::CAR | AccessMask::TRANSIT | AccessMask::HGV);
    }
    if tag_is_restricted(tags, "motorcar") {
        bits &= !AccessMask::CAR;
    }
    if tag_is_restricted(tags, "bicycle") {
        bits &= !AccessMask::BICYCLE;
    }
    if tag_is_restricted(tags, "foot") {
        bits &= !AccessMask::FOOT;
    }
    if tag_is_restricted(tags, "psv") || tag_is_restricted(tags, "bus") {
        bits &= !AccessMask::TRANSIT;
    }
    if tag_is_restricted(tags, "hgv") {
        bits &= !AccessMask::HGV;
    }

    if tag_is_allowed(tags, "vehicle") {
        bits |= AccessMask::CAR | AccessMask::BICYCLE | AccessMask::TRANSIT | AccessMask::HGV;
    }
    if tag_is_allowed(tags, "motor_vehicle") {
        bits |= AccessMask::CAR | AccessMask::TRANSIT | AccessMask::HGV;
    }
    if tag_is_allowed(tags, "motorcar") {
        bits |= AccessMask::CAR;
    }
    if tag_is_allowed(tags, "bicycle") {
        bits |= AccessMask::BICYCLE;
    }
    if tag_is_allowed(tags, "foot") {
        bits |= AccessMask::FOOT;
    }
    if tag_is_allowed(tags, "psv") || tag_is_allowed(tags, "bus") {
        bits |= AccessMask::TRANSIT;
    }
    if tag_is_allowed(tags, "hgv") {
        bits |= AccessMask::HGV;
    }

    AccessMask::new(bits)
}

fn tag_is_restricted(tags: &Tags, key: &str) -> bool {
    matches!(tag(tags, key), Some("no" | "private"))
}

fn tag_is_allowed(tags: &Tags, key: &str) -> bool {
    matches!(
        tag(tags, key),
        Some("yes" | "designated" | "official" | "permissive")
    )
}

pub(crate) fn is_traffic_signal_node(tags: &Tags) -> bool {
    tag(tags, "highway") == Some("traffic_signals")
}

fn classify_direction(tags: &Tags, road_class: RoadClass) -> EdgeDirection {
    match tag(tags, "oneway") {
        Some("-1") => EdgeDirection::ReverseOnly,
        Some("yes" | "true" | "1") => EdgeDirection::ForwardOnly,
        Some("no" | "false" | "0") => EdgeDirection::Both,
        _ if road_class == RoadClass::Motorway => EdgeDirection::ForwardOnly,
        _ if tag(tags, "junction") == Some("roundabout") => EdgeDirection::ForwardOnly,
        _ => EdgeDirection::Both,
    }
}

pub(crate) fn classify_directional_access(
    tags: &Tags,
    highway: HighwayClass,
    road_class: RoadClass,
    base_access: AccessMask,
) -> DirectionalAccess {
    let generic_direction = classify_direction(tags, road_class);
    let mut directional = DirectionalAccess {
        forward_access: AccessMask::new(0),
        reverse_access: AccessMask::new(0),
        forward_flags: 0,
        reverse_flags: 0,
    };

    for bit in [AccessMask::CAR, AccessMask::TRANSIT, AccessMask::HGV] {
        if base_access.contains(bit) {
            add_directional_bit(&mut directional, bit, generic_direction);
        }
    }

    if base_access.contains(AccessMask::FOOT) {
        let explicit_direction =
            explicit_oneway_direction(tags, &["oneway:foot", "oneway:pedestrian"])
                .or_else(|| access_forward_backward_direction(tags, "foot"));
        if let Some(foot_direction) = explicit_direction {
            add_directional_bit(&mut directional, AccessMask::FOOT, foot_direction);
        } else {
            add_bidirectional_bit_with_inferred_counterflow(
                &mut directional,
                AccessMask::FOOT,
                generic_direction,
                EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
            );
        }
    }

    if base_access.contains(AccessMask::BICYCLE) {
        let explicit_direction =
            explicit_oneway_direction(tags, &["oneway:bicycle", "bicycle:oneway"])
                .or_else(|| access_forward_backward_direction(tags, "bicycle"));
        if let Some(direction) = explicit_direction {
            add_directional_bit(&mut directional, AccessMask::BICYCLE, direction);
        } else if has_legacy_bicycle_opposite(tags) {
            add_directional_bit(&mut directional, AccessMask::BICYCLE, EdgeDirection::Both);
        } else if highway == HighwayClass::Cycleway {
            add_directional_bit(&mut directional, AccessMask::BICYCLE, generic_direction);
        } else if is_ordinary_bicycle_contraflow_road(highway) {
            add_bidirectional_bit_with_inferred_counterflow(
                &mut directional,
                AccessMask::BICYCLE,
                generic_direction,
                EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW,
            );
        } else {
            add_directional_bit(&mut directional, AccessMask::BICYCLE, generic_direction);
        }
    }

    directional
}

fn add_directional_bit(directional: &mut DirectionalAccess, bit: u16, direction: EdgeDirection) {
    match direction {
        EdgeDirection::Both => {
            directional.forward_access.0 |= bit;
            directional.reverse_access.0 |= bit;
        }
        EdgeDirection::ForwardOnly => {
            directional.forward_access.0 |= bit;
        }
        EdgeDirection::ReverseOnly => {
            directional.reverse_access.0 |= bit;
        }
    }
}

fn add_bidirectional_bit_with_inferred_counterflow(
    directional: &mut DirectionalAccess,
    bit: u16,
    generic_direction: EdgeDirection,
    flag: u32,
) {
    directional.forward_access.0 |= bit;
    directional.reverse_access.0 |= bit;
    match generic_direction {
        EdgeDirection::Both => {}
        EdgeDirection::ForwardOnly => directional.reverse_flags |= flag,
        EdgeDirection::ReverseOnly => directional.forward_flags |= flag,
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

fn has_legacy_bicycle_opposite(tags: &Tags) -> bool {
    matches!(
        tag(tags, "cycleway"),
        Some("opposite" | "opposite_lane" | "opposite_track" | "opposite_share_busway")
    ) || matches!(
        tag(tags, "cycleway:left"),
        Some("opposite" | "opposite_lane" | "opposite_track" | "opposite_share_busway")
    ) || matches!(
        tag(tags, "cycleway:right"),
        Some("opposite" | "opposite_lane" | "opposite_track" | "opposite_share_busway")
    )
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
    if chars.next()?.to_ascii_uppercase() != 'P' {
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
mod tests {
    use super::{
        EdgeDirection, classify_access, classify_direction, classify_directional_access,
        classify_highway, parse_duration_seconds,
    };
    use netweevil_core::{
        AccessMask, EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
        HighwayClass, RoadClass,
    };
    use osmpbfreader::Tags;

    #[test]
    fn classifies_major_highways() {
        assert_eq!(classify_highway("motorway"), Some(HighwayClass::Motorway));
        assert_eq!(
            classify_highway("primary_link"),
            Some(HighwayClass::PrimaryLink)
        );
        assert_eq!(classify_highway("footway"), Some(HighwayClass::Footway));
        assert_eq!(classify_highway("construction"), None);
    }

    #[test]
    fn infers_access_masks() {
        let empty = Tags::new();
        assert_eq!(
            classify_access(&empty, "motorway", None),
            AccessMask::new(AccessMask::CAR | AccessMask::HGV)
        );
        assert_eq!(
            classify_access(&empty, "cycleway", None),
            AccessMask::new(AccessMask::BICYCLE | AccessMask::FOOT)
        );
    }

    #[test]
    fn applies_mode_specific_access_overrides() {
        let restricted = Tags::from_iter([
            ("access".into(), "no".into()),
            ("foot".into(), "yes".into()),
        ]);
        assert_eq!(
            classify_access(&restricted, "residential", None),
            AccessMask::new(AccessMask::FOOT)
        );

        let bicycle_forbidden = Tags::from_iter([("bicycle".into(), "no".into())]);
        assert_eq!(
            classify_access(&bicycle_forbidden, "cycleway", None),
            AccessMask::new(AccessMask::FOOT)
        );

        let ferry_foot_only = Tags::from_iter([
            ("route".into(), "ferry".into()),
            ("motor_vehicle".into(), "no".into()),
            ("bicycle".into(), "no".into()),
        ]);
        assert_eq!(
            classify_access(&ferry_foot_only, "", Some("ferry")),
            AccessMask::new(AccessMask::FOOT)
        );
    }

    #[test]
    fn infers_oneway_behavior() {
        let mut reverse = Tags::new();
        reverse.insert("oneway".into(), "-1".into());
        assert_eq!(
            classify_direction(&reverse, RoadClass::Residential),
            EdgeDirection::ReverseOnly
        );

        let roundabout = Tags::from_iter([("junction".into(), "roundabout".into())]);
        assert_eq!(
            classify_direction(&roundabout, RoadClass::Residential),
            EdgeDirection::ForwardOnly
        );

        let empty = Tags::new();
        assert_eq!(
            classify_direction(&empty, RoadClass::Residential),
            EdgeDirection::Both
        );
    }

    #[test]
    fn builds_mode_specific_oneway_access() {
        let road = Tags::from_iter([
            ("highway".into(), "residential".into()),
            ("oneway".into(), "yes".into()),
        ]);
        let access = classify_directional_access(
            &road,
            HighwayClass::Residential,
            RoadClass::Residential,
            AccessMask::new(AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT),
        );
        assert!(access.forward_access.contains(AccessMask::CAR));
        assert!(!access.reverse_access.contains(AccessMask::CAR));
        assert!(access.reverse_access.contains(AccessMask::FOOT));
        assert!(access.reverse_access.contains(AccessMask::BICYCLE));
        assert_ne!(
            access.reverse_flags & EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
            0
        );
        assert_ne!(
            access.reverse_flags & EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW,
            0
        );

        let cycleway = Tags::from_iter([
            ("highway".into(), "cycleway".into()),
            ("oneway".into(), "yes".into()),
        ]);
        let access = classify_directional_access(
            &cycleway,
            HighwayClass::Cycleway,
            RoadClass::Path,
            AccessMask::new(AccessMask::BICYCLE | AccessMask::FOOT),
        );
        assert!(access.forward_access.contains(AccessMask::BICYCLE));
        assert!(!access.reverse_access.contains(AccessMask::BICYCLE));
        assert!(access.reverse_access.contains(AccessMask::FOOT));
    }

    #[test]
    fn parses_common_duration_formats() {
        assert_eq!(parse_duration_seconds("5400"), Some(5400.0));
        assert_eq!(parse_duration_seconds("01:30"), Some(5400.0));
        assert_eq!(parse_duration_seconds("01:02:03"), Some(3723.0));
        assert_eq!(parse_duration_seconds("PT45M"), Some(2700.0));
        assert_eq!(parse_duration_seconds("1h 15m"), Some(4500.0));
    }
}

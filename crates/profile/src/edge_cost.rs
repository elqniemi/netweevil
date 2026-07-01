use std::collections::BTreeMap;

use anyhow::{Result, bail};
use netweevil_core::{
    DirectedEdge, EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    HighwayClass, RoadClass, SmoothnessClass, SurfaceClass, TravelMode,
};

use crate::schema::{PostedLimitPolicy, ProfileDocument};

pub(crate) fn edge_travel_time_s(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
) -> Option<f64> {
    if edge.road_class == RoadClass::Ferry {
        let scheduled_duration_s = edge.duration_s;
        let inferred_duration_s = profile.ferry.infer_duration_when_missing.then(|| {
            let speed_kph =
                matching_speed(profile, edge, highway).unwrap_or(profile.ferry.default_speed_kph);
            let effective_speed_kph =
                (speed_kph * matching_speed_factor(profile, edge, highway)).max(1.0);
            edge.length_m as f64 / (effective_speed_kph * 1000.0 / 3600.0)
        });

        return scheduled_duration_s
            .or(inferred_duration_s)
            .map(|duration_s| duration_s + profile.ferry.boarding_cost_s);
    }

    let mut speed_kph = default_speed_kph(edge.road_class);
    if let Some(rule_speed) = matching_speed(profile, edge, highway) {
        speed_kph = rule_speed;
    }
    // Posted limits from the source data (OSM maxspeed, Overture
    // speed_limits) only apply to motorized modes — a 50 km/h zone must not
    // speed up pedestrians — and combine per the profile's
    // `speeds.posted_limits` policy.
    if matches!(
        profile.profile.mode,
        TravelMode::Car | TravelMode::Hgv | TravelMode::Transit
    ) && let Some(max_speed_kph) = edge.max_speed_kph
    {
        match profile.speeds.posted_limits {
            PostedLimitPolicy::Prefer => speed_kph = max_speed_kph as f64,
            PostedLimitPolicy::Cap => speed_kph = speed_kph.min(max_speed_kph as f64),
            PostedLimitPolicy::Ignore => {}
        }
    }

    let effective_speed_kph = (speed_kph * matching_speed_factor(profile, edge, highway)).max(1.0);
    Some(edge.length_m as f64 / (effective_speed_kph * 1000.0 / 3600.0))
}

pub(crate) fn ensure_supported_matchers(profile: &ProfileDocument) -> Result<()> {
    for (rule_index, rule) in profile.speed_rules.iter().enumerate() {
        ensure_supported_keys(&rule.r#match.tags, "speed_rules", rule_index)?;
    }
    for (rule_index, rule) in profile.exclude_rules.iter().enumerate() {
        ensure_supported_keys(&rule.r#match.tags, "exclude_rules", rule_index)?;
    }
    for (rule_index, rule) in profile.factors.iter().enumerate() {
        ensure_supported_keys(&rule.r#match.tags, "factors", rule_index)?;
    }
    Ok(())
}

fn ensure_supported_keys(
    tags: &BTreeMap<String, String>,
    rule_group: &str,
    rule_index: usize,
) -> Result<()> {
    for key in tags.keys() {
        match key.as_str() {
            "highway" | "road_class" | "surface" | "smoothness" | "route" | "toll" => {}
            other => bail!(
                "{rule_group}[{rule_index}] uses unsupported match key '{other}'; supported keys are highway, road_class, surface, smoothness, route, toll"
            ),
        }
    }
    Ok(())
}

fn matching_speed(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
) -> Option<f64> {
    profile
        .speed_rules
        .iter()
        .find(|rule| matches_edge(&rule.r#match.tags, edge, highway))
        .map(|rule| rule.speed_kph)
}

fn matching_speed_factor(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
) -> f64 {
    profile
        .factors
        .iter()
        .filter(|rule| matches_edge(&rule.r#match.tags, edge, highway))
        .fold(1.0, |product, rule| product * rule.speed_factor)
}

pub(crate) fn is_directionally_excluded(profile: &ProfileDocument, edge: &DirectedEdge) -> bool {
    (matches!(profile.profile.mode, TravelMode::Foot)
        && edge.flags & EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY != 0
        && !profile.direction.ignore_plain_oneway_for_foot)
        || (matches!(profile.profile.mode, TravelMode::Bicycle)
            && edge.flags & EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW != 0
            && !profile.direction.allow_bicycle_contraflow_on_roads)
}

pub(crate) fn is_excluded(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
) -> bool {
    profile
        .exclude_rules
        .iter()
        .any(|rule| matches_edge(&rule.r#match.tags, edge, highway))
}

fn matches_edge(
    tags: &BTreeMap<String, String>,
    edge: &DirectedEdge,
    highway: HighwayClass,
) -> bool {
    tags.iter().all(|(key, expected)| {
        edge_tag_value(edge, highway, key).is_some_and(|actual| actual == expected)
    })
}

fn edge_tag_value(edge: &DirectedEdge, highway: HighwayClass, key: &str) -> Option<&'static str> {
    match key {
        "highway" => highway_name(highway).or_else(|| road_class_name(edge.road_class)),
        "road_class" => road_class_name(edge.road_class),
        "surface" => surface_name(edge.surface),
        "smoothness" => smoothness_name(edge.smoothness),
        "route" => (edge.road_class == RoadClass::Ferry).then_some("ferry"),
        "toll" => Some(if edge.is_toll { "yes" } else { "no" }),
        _ => None,
    }
}

fn road_class_name(road_class: RoadClass) -> Option<&'static str> {
    match road_class {
        RoadClass::Motorway => Some("motorway"),
        RoadClass::Trunk => Some("trunk"),
        RoadClass::Primary => Some("primary"),
        RoadClass::Secondary => Some("secondary"),
        RoadClass::Tertiary => Some("tertiary"),
        RoadClass::Residential => Some("residential"),
        RoadClass::Service => Some("service"),
        RoadClass::Track => Some("track"),
        RoadClass::Ferry => None,
        RoadClass::Path => Some("path"),
        RoadClass::Unknown => None,
    }
}

fn highway_name(highway: HighwayClass) -> Option<&'static str> {
    match highway {
        HighwayClass::Motorway => Some("motorway"),
        HighwayClass::MotorwayLink => Some("motorway_link"),
        HighwayClass::Trunk => Some("trunk"),
        HighwayClass::TrunkLink => Some("trunk_link"),
        HighwayClass::Primary => Some("primary"),
        HighwayClass::PrimaryLink => Some("primary_link"),
        HighwayClass::Secondary => Some("secondary"),
        HighwayClass::SecondaryLink => Some("secondary_link"),
        HighwayClass::Tertiary => Some("tertiary"),
        HighwayClass::TertiaryLink => Some("tertiary_link"),
        HighwayClass::Residential => Some("residential"),
        HighwayClass::Unclassified => Some("unclassified"),
        HighwayClass::LivingStreet => Some("living_street"),
        HighwayClass::Service => Some("service"),
        HighwayClass::Track => Some("track"),
        HighwayClass::Path => Some("path"),
        HighwayClass::Cycleway => Some("cycleway"),
        HighwayClass::Footway => Some("footway"),
        HighwayClass::Pedestrian => Some("pedestrian"),
        HighwayClass::Steps => Some("steps"),
        HighwayClass::Bridleway => Some("bridleway"),
        HighwayClass::Ferry | HighwayClass::Unknown => None,
    }
}

fn surface_name(surface: SurfaceClass) -> Option<&'static str> {
    match surface {
        SurfaceClass::Asphalt => Some("asphalt"),
        SurfaceClass::Paved => Some("paved"),
        SurfaceClass::Gravel => Some("gravel"),
        SurfaceClass::Cobblestone => Some("cobblestone"),
        SurfaceClass::Ground => Some("ground"),
        SurfaceClass::Dirt => Some("dirt"),
        SurfaceClass::Sand => Some("sand"),
        SurfaceClass::Unknown => None,
    }
}

fn smoothness_name(smoothness: SmoothnessClass) -> Option<&'static str> {
    match smoothness {
        SmoothnessClass::Excellent => Some("excellent"),
        SmoothnessClass::Good => Some("good"),
        SmoothnessClass::Intermediate => Some("intermediate"),
        SmoothnessClass::Bad => Some("bad"),
        SmoothnessClass::VeryBad => Some("very_bad"),
        SmoothnessClass::Horrible => Some("horrible"),
        SmoothnessClass::VeryHorrible => Some("very_horrible"),
        SmoothnessClass::Impassable => Some("impassable"),
        SmoothnessClass::Unknown => None,
    }
}

pub(crate) fn mode_access_bit(mode: TravelMode) -> u16 {
    mode.access_bit()
}

fn default_speed_kph(road_class: RoadClass) -> f64 {
    match road_class {
        RoadClass::Motorway => 110.0,
        RoadClass::Trunk => 90.0,
        RoadClass::Primary => 70.0,
        RoadClass::Secondary => 60.0,
        RoadClass::Tertiary => 50.0,
        RoadClass::Residential => 30.0,
        RoadClass::Service => 20.0,
        RoadClass::Track => 15.0,
        RoadClass::Ferry => 20.0,
        RoadClass::Path => 5.0,
        RoadClass::Unknown => 25.0,
    }
}

pub(crate) fn generalized_cost(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    travel_time_s: f64,
) -> f64 {
    let mut cost = travel_time_s * profile.cost.time_weight
        + edge.length_m as f64 * profile.cost.distance_weight;

    if edge.road_class == RoadClass::Service {
        cost += profile.preferences.service_penalty_s;
    }
    if edge.road_class == RoadClass::Ferry {
        cost *= preference_multiplier(profile.ferry.use_ferry);
    }
    if is_major_highway(edge.road_class) {
        cost *= preference_multiplier(profile.preferences.use_highways);
    }
    if edge.road_class == RoadClass::Track {
        cost *= preference_multiplier(profile.preferences.use_tracks);
    }
    if edge.is_toll {
        cost *= preference_multiplier(profile.preferences.use_tolls);
    }

    cost
}

fn preference_multiplier(value: f64) -> f64 {
    (1.5 - value).clamp(0.5, 1.5)
}

fn is_major_highway(road_class: RoadClass) -> bool {
    matches!(
        road_class,
        RoadClass::Motorway | RoadClass::Trunk | RoadClass::Primary
    )
}

#[cfg(test)]
mod tests {
    use super::edge_travel_time_s;
    use crate::schema::ProfileDocument;
    use netweevil_core::{
        AccessMask, DirectedEdge, EdgeId, HighwayClass, NodeId, RoadClass, SmoothnessClass,
        SurfaceClass,
    };

    fn car_profile(posted_limits: &str) -> ProfileDocument {
        serde_yaml::from_str(&format!(
            "profile:\n  id: test\n  label: test\n  mode: car\n  defaults_pack: test\nspeeds:\n  posted_limits: {posted_limits}\nspeed_rules:\n  - match:\n      highway: primary\n    speed_kph: 80\n"
        ))
        .expect("profile parses")
    }

    fn primary_edge(max_speed_kph: Option<f32>) -> DirectedEdge {
        DirectedEdge {
            edge_id: EdgeId(0),
            from: NodeId(0),
            to: NodeId(1),
            source_way_id: 1,
            length_m: 1_000,
            duration_s: None,
            road_class: RoadClass::Primary,
            surface: SurfaceClass::Asphalt,
            smoothness: SmoothnessClass::Unknown,
            access_mask: AccessMask::new(AccessMask::CAR),
            is_toll: false,
            max_speed_kph,
            lanes: None,
            name_index: None,
            geometry_offset: 0,
            geometry_len: 0,
            flags: 0,
        }
    }

    fn speed_kph(profile: &ProfileDocument, edge: &DirectedEdge) -> f64 {
        let time_s = edge_travel_time_s(profile, edge, HighwayClass::Primary).expect("travel time");
        edge.length_m as f64 / time_s * 3.6
    }

    #[test]
    fn combines_posted_limits_per_policy() {
        // Posted limit below the 80 km/h profile speed.
        let slow_zone = primary_edge(Some(50.0));
        // Posted limit above the profile speed.
        let fast_road = primary_edge(Some(100.0));
        let unposted = primary_edge(None);

        let cap = car_profile("cap");
        assert!((speed_kph(&cap, &slow_zone) - 50.0).abs() < 1e-6);
        assert!((speed_kph(&cap, &fast_road) - 80.0).abs() < 1e-6);

        let prefer = car_profile("prefer");
        assert!((speed_kph(&prefer, &slow_zone) - 50.0).abs() < 1e-6);
        assert!((speed_kph(&prefer, &fast_road) - 100.0).abs() < 1e-6);
        assert!((speed_kph(&prefer, &unposted) - 80.0).abs() < 1e-6);

        let ignore = car_profile("ignore");
        assert!((speed_kph(&ignore, &slow_zone) - 80.0).abs() < 1e-6);
        assert!((speed_kph(&ignore, &fast_road) - 80.0).abs() < 1e-6);
    }

    #[test]
    fn posted_limits_never_apply_to_foot_profiles() {
        let mut profile = car_profile("prefer");
        profile.profile.mode = netweevil_core::TravelMode::Foot;
        profile.speed_rules.clear();
        let edge = primary_edge(Some(100.0));
        // Foot speed comes from the road-class default, not the limit.
        assert!(speed_kph(&profile, &edge) < 100.0);
    }
}

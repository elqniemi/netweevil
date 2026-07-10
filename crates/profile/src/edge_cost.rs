use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use netweevil_core::{
    DirectedEdge, EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    HighwayClass, RoadClass, SmoothnessClass, SurfaceClass, TravelMode,
};

use crate::schema::{FacilityCost, PostedLimitPolicy, ProfileDocument, SlopeModelConfig};

#[derive(Debug, Clone)]
pub(crate) struct EdgeTraversalCost {
    pub(crate) travel_time_s: f64,
    /// Facility-specific named quantities (for example `lift_wait`). These
    /// are merged with explicitly declared component expressions during
    /// profile compilation.
    pub(crate) facility_components: BTreeMap<String, f64>,
}

#[cfg(test)]
pub(crate) fn edge_travel_time_s(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
) -> Option<f64> {
    edge_traversal_cost(profile, edge, highway, 0.0, 0.0, None, &|_, _| false)
        .map(|cost| cost.travel_time_s)
}

pub(crate) fn edge_traversal_cost(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
    ascent_m: f32,
    descent_m: f32,
    facility: Option<&FacilityCost>,
    attribute_matches: &dyn Fn(&str, &str) -> bool,
) -> Option<EdgeTraversalCost> {
    if edge.road_class == RoadClass::Ferry {
        let scheduled_duration_s = edge.duration_s;
        let inferred_duration_s = profile.ferry.infer_duration_when_missing.then(|| {
            let speed_kph = matching_speed(profile, edge, highway, attribute_matches)
                .unwrap_or(profile.ferry.default_speed_kph);
            let effective_speed_kph = (speed_kph
                * matching_speed_factor(profile, edge, highway, attribute_matches))
            .max(1.0);
            edge.length_m as f64 / (effective_speed_kph * 1000.0 / 3600.0)
        });

        return scheduled_duration_s
            .or(inferred_duration_s)
            .map(|duration_s| EdgeTraversalCost {
                travel_time_s: duration_s + profile.ferry.boarding_cost_s,
                facility_components: BTreeMap::new(),
            });
    }

    let mut speed_kph = default_speed_kph(edge.road_class);
    if let Some(rule_speed) = matching_speed(profile, edge, highway, attribute_matches) {
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

    let slope_factor = profile
        .slope_model
        .as_ref()
        .map(|model| slope_speed_factor(model, edge.length_m, ascent_m, descent_m))
        .unwrap_or(1.0);
    let effective_speed_kph = (speed_kph
        * matching_speed_factor(profile, edge, highway, attribute_matches)
        * slope_factor)
        .max(0.1);
    let base_speed_mps = effective_speed_kph / 3.6;
    let base_time_s = edge.length_m as f64 / base_speed_mps;
    let vertical_m = f64::from(ascent_m + descent_m);
    let horizontal_m = horizontal_length_m(edge.length_m, ascent_m, descent_m);
    let mut facility_components = BTreeMap::new();
    let travel_time_s = match facility {
        None => base_time_s,
        Some(FacilityCost::Stairs {
            vertical_speed_mps,
            boarding_penalty_s,
            burden_per_vertical_m,
            burden_component,
        }) => {
            if *burden_per_vertical_m > 0.0 {
                facility_components.insert(
                    burden_component.clone(),
                    vertical_m * *burden_per_vertical_m,
                );
            }
            base_time_s.max(vertical_m / vertical_speed_mps) + boarding_penalty_s
        }
        Some(FacilityCost::Escalator {
            conveyor_speed_mps,
            walking_speed_mps,
            boarding_penalty_s,
        })
        | Some(FacilityCost::Travelator {
            conveyor_speed_mps,
            walking_speed_mps,
            boarding_penalty_s,
        }) => edge.length_m as f64 / (conveyor_speed_mps + walking_speed_mps) + boarding_penalty_s,
        Some(FacilityCost::Lift {
            wait_s,
            vertical_speed_mps,
            wait_component,
        }) => {
            facility_components.insert(wait_component.clone(), *wait_s);
            (vertical_m / vertical_speed_mps).max(horizontal_m / base_speed_mps) + wait_s
        }
        Some(FacilityCost::Ramp {
            speed_factor,
            boarding_penalty_s,
        }) => base_time_s / speed_factor + boarding_penalty_s,
    };

    Some(EdgeTraversalCost {
        travel_time_s,
        facility_components,
    })
}

pub(crate) fn ensure_supported_matchers(
    profile: &ProfileDocument,
    dataset_attributes: &BTreeSet<String>,
    component_match_keys: impl IntoIterator<Item = String>,
) -> Result<()> {
    for (rule_index, rule) in profile.speed_rules.iter().enumerate() {
        ensure_supported_keys(
            &rule.r#match.tags,
            "speed_rules",
            rule_index,
            dataset_attributes,
        )?;
    }
    for (rule_index, rule) in profile.exclude_rules.iter().enumerate() {
        ensure_supported_keys(
            &rule.r#match.tags,
            "exclude_rules",
            rule_index,
            dataset_attributes,
        )?;
    }
    for (rule_index, rule) in profile.factors.iter().enumerate() {
        ensure_supported_keys(
            &rule.r#match.tags,
            "factors",
            rule_index,
            dataset_attributes,
        )?;
    }
    if !profile.facilities.classes.is_empty()
        && !is_builtin_match_key(&profile.facilities.attribute)
        && !dataset_attributes.contains(&normalize_key(&profile.facilities.attribute))
    {
        bail!(
            "facilities.attribute '{}' is not present in the dataset attribute registry",
            profile.facilities.attribute
        );
    }
    for key in component_match_keys {
        if !is_builtin_match_key(&key) && !dataset_attributes.contains(&normalize_key(&key)) {
            bail!(
                "component expression uses attribute '{key}' which is not present in the dataset attribute registry"
            );
        }
    }
    Ok(())
}

fn ensure_supported_keys(
    tags: &BTreeMap<String, String>,
    rule_group: &str,
    rule_index: usize,
    dataset_attributes: &BTreeSet<String>,
) -> Result<()> {
    for key in tags.keys() {
        if !is_builtin_match_key(key) && !dataset_attributes.contains(&normalize_key(key)) {
            bail!(
                "{rule_group}[{rule_index}] uses attribute '{key}' which is not present in the dataset attribute registry"
            );
        }
    }
    Ok(())
}

fn is_builtin_match_key(key: &str) -> bool {
    matches!(
        normalize_key(key).as_str(),
        "highway" | "road_class" | "surface" | "smoothness" | "route" | "toll"
    )
}

fn normalize_key(key: &str) -> String {
    netweevil_core::normalize_attribute_name(key)
}

fn matching_speed(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
    attribute_matches: &dyn Fn(&str, &str) -> bool,
) -> Option<f64> {
    profile
        .speed_rules
        .iter()
        .find(|rule| matches_edge(&rule.r#match.tags, edge, highway, attribute_matches))
        .map(|rule| rule.speed_kph)
}

fn matching_speed_factor(
    profile: &ProfileDocument,
    edge: &DirectedEdge,
    highway: HighwayClass,
    attribute_matches: &dyn Fn(&str, &str) -> bool,
) -> f64 {
    profile
        .factors
        .iter()
        .filter(|rule| matches_edge(&rule.r#match.tags, edge, highway, attribute_matches))
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
    attribute_matches: &dyn Fn(&str, &str) -> bool,
) -> bool {
    profile
        .exclude_rules
        .iter()
        .any(|rule| matches_edge(&rule.r#match.tags, edge, highway, attribute_matches))
}

fn matches_edge(
    tags: &BTreeMap<String, String>,
    edge: &DirectedEdge,
    highway: HighwayClass,
    attribute_matches: &dyn Fn(&str, &str) -> bool,
) -> bool {
    tags.iter().all(|(key, expected)| {
        edge_tag_value(edge, highway, key)
            .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            || (edge_tag_value(edge, highway, key).is_none() && attribute_matches(key, expected))
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

fn horizontal_length_m(length_m: u32, ascent_m: f32, descent_m: f32) -> f64 {
    let length = f64::from(length_m);
    let vertical = f64::from(ascent_m + descent_m);
    (length.mul_add(length, -(vertical * vertical)))
        .max(0.0)
        .sqrt()
}

fn slope_speed_factor(
    model: &SlopeModelConfig,
    length_m: u32,
    ascent_m: f32,
    descent_m: f32,
) -> f64 {
    let horizontal = horizontal_length_m(length_m, ascent_m, descent_m);
    if horizontal <= f64::EPSILON {
        return 1.0;
    }
    let gradient = f64::from(ascent_m - descent_m) / horizontal;
    match model {
        SlopeModelConfig::Tobler {
            exponent,
            optimal_gradient,
            min_speed_factor,
            max_speed_factor,
        } => {
            let flat_penalty = exponent * optimal_gradient.abs();
            (-exponent * (gradient + optimal_gradient).abs() + flat_penalty)
                .exp()
                .clamp(*min_speed_factor, *max_speed_factor)
        }
        SlopeModelConfig::Piecewise { points } => {
            if gradient <= points[0].gradient {
                return points[0].speed_factor;
            }
            if gradient >= points[points.len() - 1].gradient {
                return points[points.len() - 1].speed_factor;
            }
            let upper = points.partition_point(|point| point.gradient < gradient);
            let left = points[upper - 1];
            let right = points[upper];
            let fraction = (gradient - left.gradient) / (right.gradient - left.gradient);
            left.speed_factor + fraction * (right.speed_factor - left.speed_factor)
        }
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
            ascent_m: 0.0,
            descent_m: 0.0,
            feature_row: netweevil_core::NO_FEATURE_ROW,
            source_direction: 0,
            temporal_rule_id: None,
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

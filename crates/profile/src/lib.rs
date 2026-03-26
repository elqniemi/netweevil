use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netan_core::{
    CacheBundleId, CompiledAcceleration, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTurnCostConfig, DatasetAccelerationBundle, DirectedEdge, EDGE_FLAG_ROUNDABOUT,
    EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, RoadClass, SmoothnessClass, SurfaceClass, TopologyBundle,
    TravelMode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDocument {
    pub profile: ProfileHeader,
    #[serde(default)]
    pub cost: CostConfig,
    #[serde(default)]
    pub speed_rules: Vec<SpeedRule>,
    #[serde(default)]
    pub exclude_rules: Vec<ExcludeRule>,
    #[serde(default)]
    pub factors: Vec<FactorRule>,
    #[serde(default)]
    pub turns: TurnConfig,
    #[serde(default)]
    pub ferry: FerryConfig,
    #[serde(default)]
    pub preferences: PreferencesConfig,
    #[serde(default)]
    pub returns: ReturnConfig,
}

impl ProfileDocument {
    pub fn validate(&self) -> Result<()> {
        if self.profile.id.trim().is_empty() {
            bail!("profile.id must not be empty");
        }
        if self.profile.defaults_pack.trim().is_empty() {
            bail!("profile.defaults_pack must not be empty");
        }
        if self.cost.distance_weight < 0.0 || self.cost.time_weight < 0.0 {
            bail!("cost weights must be non-negative");
        }
        for (index, rule) in self.speed_rules.iter().enumerate() {
            if rule.r#match.tags.is_empty() {
                bail!("speed_rules[{index}] must contain at least one match tag");
            }
            if rule.speed_kph <= 0.0 {
                bail!("speed_rules[{index}] speed_kph must be > 0");
            }
        }
        for (index, rule) in self.exclude_rules.iter().enumerate() {
            if rule.r#match.tags.is_empty() {
                bail!("exclude_rules[{index}] must contain at least one match tag");
            }
        }
        for (index, factor) in self.factors.iter().enumerate() {
            if factor.r#match.tags.is_empty() {
                bail!("factors[{index}] must contain at least one match tag");
            }
            if factor.speed_factor <= 0.0 {
                bail!("factors[{index}] speed_factor must be > 0");
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<String> {
        let bytes = serde_yaml::to_string(self).context("serializing profile for hashing")?;
        Ok(hex::encode(Sha256::digest(bytes.as_bytes())))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileHeader {
    pub id: String,
    pub label: String,
    pub mode: TravelMode,
    pub defaults_pack: String,
    #[serde(default)]
    pub extends: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostConfig {
    #[serde(default)]
    pub objective: Objective,
    #[serde(default)]
    pub distance_weight: f64,
    #[serde(default = "default_time_weight")]
    pub time_weight: f64,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            objective: Objective::Fastest,
            distance_weight: 0.0,
            time_weight: 1.0,
        }
    }
}

fn default_time_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Objective {
    #[default]
    Fastest,
    Shortest,
    Generalized,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TagMatch {
    #[serde(flatten)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedRule {
    #[serde(rename = "match")]
    pub r#match: TagMatch,
    pub speed_kph: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludeRule {
    #[serde(rename = "match")]
    pub r#match: TagMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorRule {
    #[serde(rename = "match")]
    pub r#match: TagMatch,
    pub speed_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnConfig {
    #[serde(default)]
    pub left_penalty_s: f64,
    #[serde(default)]
    pub right_penalty_s: f64,
    #[serde(default)]
    pub uturn_penalty_s: f64,
    #[serde(default)]
    pub traffic_signal_penalty_s: f64,
    #[serde(default)]
    pub roundabout_entry_penalty_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FerryConfig {
    #[serde(default = "default_true")]
    pub allow: bool,
    #[serde(default = "default_use_ferry")]
    pub use_ferry: f64,
    #[serde(default)]
    pub boarding_cost_s: f64,
    #[serde(default)]
    pub infer_duration_when_missing: bool,
    #[serde(default = "default_ferry_speed")]
    pub default_speed_kph: f64,
}

impl Default for FerryConfig {
    fn default() -> Self {
        Self {
            allow: true,
            use_ferry: 0.5,
            boarding_cost_s: 0.0,
            infer_duration_when_missing: true,
            default_speed_kph: 20.0,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_use_ferry() -> f64 {
    0.5
}

fn default_ferry_speed() -> f64 {
    20.0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreferencesConfig {
    #[serde(default = "default_pref")]
    pub use_highways: f64,
    #[serde(default = "default_pref")]
    pub use_tolls: f64,
    #[serde(default = "default_pref")]
    pub use_tracks: f64,
    #[serde(default)]
    pub service_penalty_s: f64,
}

fn default_pref() -> f64 {
    0.5
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReturnConfig {
    #[serde(default)]
    pub geometry: ReturnGeometry,
    #[serde(default)]
    pub segment_rows: bool,
    #[serde(default)]
    pub road_type_breakdown: Vec<BreakdownMetric>,
    #[serde(default)]
    pub surface_breakdown: Vec<BreakdownMetric>,
    #[serde(default)]
    pub penalty_breakdown: bool,
    #[serde(default)]
    pub explain_cost_derivation: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReturnGeometry {
    #[default]
    None,
    Full,
    Segments,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakdownMetric {
    TimeS,
    DistanceM,
}

pub fn load_profile(path: impl AsRef<Path>) -> Result<ProfileDocument> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading profile file {}", path.display()))?;
    let parsed = match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => toml::from_str(&raw).context("parsing TOML profile")?,
        Some("yaml") | Some("yml") => serde_yaml::from_str(&raw).context("parsing YAML profile")?,
        other => bail!(
            "unsupported profile extension {:?}; use .yml, .yaml, or .toml",
            other
        ),
    };
    Ok(parsed)
}

pub fn compile_profile_bundle(
    profile: &ProfileDocument,
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
) -> Result<CompiledProfileBundle> {
    compile_profile_bundle_with_acceleration(profile, topology, source_topology_bundle_id, None)
}

pub fn compile_profile_bundle_with_acceleration(
    profile: &ProfileDocument,
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    acceleration: Option<(&DatasetAccelerationBundle, CacheBundleId)>,
) -> Result<CompiledProfileBundle> {
    ensure_supported_matchers(profile)?;
    let profile_hash = profile.fingerprint()?;
    let mode_bit = mode_access_bit(profile.profile.mode);
    let mut edge_metrics = Vec::with_capacity(topology.edges.len());

    for edge in &topology.edges {
        if !edge.access_mask.contains(mode_bit)
            || (edge.road_class == RoadClass::Ferry && !profile.ferry.allow)
            || is_excluded(profile, edge)
        {
            edge_metrics.push(CompiledEdgeMetric {
                edge_id: edge.edge_id,
                travel_time_s: None,
                generalized_cost: None,
            });
            continue;
        }

        let Some(travel_time_s) = edge_travel_time_s(profile, edge) else {
            edge_metrics.push(CompiledEdgeMetric {
                edge_id: edge.edge_id,
                travel_time_s: None,
                generalized_cost: None,
            });
            continue;
        };

        edge_metrics.push(CompiledEdgeMetric {
            edge_id: edge.edge_id,
            travel_time_s: Some(travel_time_s),
            generalized_cost: Some(generalized_cost(profile, edge, travel_time_s)),
        });
    }

    Ok(CompiledProfileBundle {
        schema_version: 3,
        profile_id: profile.profile.id.clone(),
        profile_hash,
        mode: profile.profile.mode,
        turn_costs: CompiledTurnCostConfig {
            left_penalty_s: profile.turns.left_penalty_s,
            right_penalty_s: profile.turns.right_penalty_s,
            uturn_penalty_s: profile.turns.uturn_penalty_s,
            traffic_signal_penalty_s: profile.turns.traffic_signal_penalty_s,
            roundabout_entry_penalty_s: profile.turns.roundabout_entry_penalty_s,
            cost_time_weight: profile.cost.time_weight,
        },
        source_topology_bundle_id,
        acceleration: acceleration.map(|(bundle, bundle_id)| {
            compile_acceleration(bundle, bundle_id, topology, &edge_metrics, profile)
        }),
        edge_metrics,
    })
}

fn compile_acceleration(
    bundle: &DatasetAccelerationBundle,
    source_acceleration_bundle_id: CacheBundleId,
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    profile: &ProfileDocument,
) -> CompiledAcceleration {
    let upward_weight = compile_oriented_transition_weights(
        topology,
        edge_metrics,
        profile,
        &bundle.upward_first_out,
        &bundle.upward_head,
    );
    let downward_weight = compile_oriented_transition_weights(
        topology,
        edge_metrics,
        profile,
        &bundle.downward_first_out,
        &bundle.downward_head,
    );
    let upward_path_first_out = (0..=bundle.upward_head.len() as u32).collect::<Vec<_>>();
    let downward_path_first_out = (0..=bundle.downward_head.len() as u32).collect::<Vec<_>>();

    CompiledAcceleration {
        schema_version: 1,
        source_acceleration_bundle_id,
        algorithm: format!("{}+oriented_paths_v1", bundle.algorithm),
        edge_order: bundle.edge_order.clone(),
        edge_rank: bundle.edge_rank.clone(),
        upward_first_out: bundle.upward_first_out.clone(),
        upward_head: bundle.upward_head.clone(),
        upward_weight,
        upward_path_first_out,
        upward_path_edges: bundle.upward_head.clone(),
        downward_first_out: bundle.downward_first_out.clone(),
        downward_head: bundle.downward_head.clone(),
        downward_weight,
        downward_path_first_out,
        downward_path_edges: bundle.downward_head.clone(),
    }
}

fn compile_oriented_transition_weights(
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    profile: &ProfileDocument,
    first_out: &[u32],
    head: &[u32],
) -> Vec<f64> {
    let mut weights = vec![f64::INFINITY; head.len()];
    if first_out.len() != topology.edges.len() + 1 {
        return weights;
    }
    for edge_index in 0..topology.edges.len() {
        let start = first_out[edge_index] as usize;
        let end = first_out[edge_index + 1] as usize;
        for slot in start..end {
            let next_edge = head[slot] as usize;
            let Some(next_cost) = edge_metrics[next_edge].generalized_cost else {
                continue;
            };
            weights[slot] = next_cost
                + transition_turn_penalty_cost(
                    topology,
                    edge_index,
                    next_edge,
                    profile.turns.left_penalty_s,
                    profile.turns.right_penalty_s,
                    profile.turns.uturn_penalty_s,
                    profile.turns.traffic_signal_penalty_s,
                    profile.turns.roundabout_entry_penalty_s,
                    profile.cost.time_weight,
                );
        }
    }
    weights
}

fn edge_travel_time_s(profile: &ProfileDocument, edge: &DirectedEdge) -> Option<f64> {
    if edge.road_class == RoadClass::Ferry {
        let scheduled_duration_s = edge.duration_s;
        let inferred_duration_s = profile.ferry.infer_duration_when_missing.then(|| {
            let speed_kph =
                matching_speed(profile, edge).unwrap_or(profile.ferry.default_speed_kph);
            let effective_speed_kph = (speed_kph * matching_speed_factor(profile, edge)).max(1.0);
            edge.length_m as f64 / (effective_speed_kph * 1000.0 / 3600.0)
        });

        return scheduled_duration_s
            .or(inferred_duration_s)
            .map(|duration_s| duration_s + profile.ferry.boarding_cost_s);
    }

    let mut speed_kph = default_speed_kph(edge.road_class);
    if let Some(rule_speed) = matching_speed(profile, edge) {
        speed_kph = rule_speed;
    }

    let effective_speed_kph = (speed_kph * matching_speed_factor(profile, edge)).max(1.0);
    Some(edge.length_m as f64 / (effective_speed_kph * 1000.0 / 3600.0))
}

fn ensure_supported_matchers(profile: &ProfileDocument) -> Result<()> {
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
            "highway" | "surface" | "smoothness" | "route" | "toll" => {}
            other => bail!(
                "{rule_group}[{rule_index}] uses unsupported match key '{other}'; supported keys are highway, surface, smoothness, route, toll"
            ),
        }
    }
    Ok(())
}

fn matching_speed(profile: &ProfileDocument, edge: &DirectedEdge) -> Option<f64> {
    profile
        .speed_rules
        .iter()
        .find(|rule| matches_edge(&rule.r#match.tags, edge))
        .map(|rule| rule.speed_kph)
}

fn matching_speed_factor(profile: &ProfileDocument, edge: &DirectedEdge) -> f64 {
    profile
        .factors
        .iter()
        .filter(|rule| matches_edge(&rule.r#match.tags, edge))
        .fold(1.0, |product, rule| product * rule.speed_factor)
}

fn is_excluded(profile: &ProfileDocument, edge: &DirectedEdge) -> bool {
    profile
        .exclude_rules
        .iter()
        .any(|rule| matches_edge(&rule.r#match.tags, edge))
}

fn matches_edge(tags: &BTreeMap<String, String>, edge: &DirectedEdge) -> bool {
    tags.iter()
        .all(|(key, expected)| edge_tag_value(edge, key).is_some_and(|actual| actual == expected))
}

fn edge_tag_value<'a>(edge: &'a DirectedEdge, key: &str) -> Option<&'a str> {
    match key {
        "highway" => road_class_name(edge.road_class),
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

fn mode_access_bit(mode: TravelMode) -> u16 {
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

fn generalized_cost(profile: &ProfileDocument, edge: &DirectedEdge, travel_time_s: f64) -> f64 {
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

fn transition_turn_penalty_cost(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
    left_penalty_s: f64,
    right_penalty_s: f64,
    uturn_penalty_s: f64,
    traffic_signal_penalty_s: f64,
    roundabout_entry_penalty_s: f64,
    cost_time_weight: f64,
) -> f64 {
    transition_turn_penalty_seconds(
        topology,
        previous_edge_index,
        next_edge_index,
        left_penalty_s,
        right_penalty_s,
        uturn_penalty_s,
        traffic_signal_penalty_s,
        roundabout_entry_penalty_s,
    ) * cost_time_weight
}

fn transition_turn_penalty_seconds(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
    left_penalty_s: f64,
    right_penalty_s: f64,
    uturn_penalty_s: f64,
    traffic_signal_penalty_s: f64,
    roundabout_entry_penalty_s: f64,
) -> f64 {
    let previous = &topology.edges[previous_edge_index];
    let next = &topology.edges[next_edge_index];

    if previous.to != next.from {
        return 0.0;
    }

    let mut penalty_s = 0.0;
    if previous.flags & EDGE_FLAG_TARGET_TRAFFIC_SIGNAL != 0 {
        penalty_s += traffic_signal_penalty_s;
    }
    if previous.flags & EDGE_FLAG_ROUNDABOUT == 0 && next.flags & EDGE_FLAG_ROUNDABOUT != 0 {
        penalty_s += roundabout_entry_penalty_s;
    }

    if previous.from == next.to {
        return penalty_s + uturn_penalty_s;
    }

    if previous.source_way_id == next.source_way_id {
        return penalty_s;
    }

    penalty_s
        + match classify_turn(topology, previous_edge_index, next_edge_index) {
            TurnDirection::Straight => 0.0,
            TurnDirection::Left => left_penalty_s,
            TurnDirection::Right => right_penalty_s,
            TurnDirection::Uturn => uturn_penalty_s,
        }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnDirection {
    Straight,
    Left,
    Right,
    Uturn,
}

fn classify_turn(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> TurnDirection {
    const STRAIGHT_THRESHOLD_RAD: f64 = 30.0_f64.to_radians();
    const UTURN_THRESHOLD_RAD: f64 = 150.0_f64.to_radians();

    let previous = &topology.edges[previous_edge_index];
    let next = &topology.edges[next_edge_index];
    let from = &topology.nodes[previous.from.0 as usize];
    let via = &topology.nodes[previous.to.0 as usize];
    let to = &topology.nodes[next.to.0 as usize];

    let in_x = projected_delta_x(from.lon, via.lat, via.lon);
    let in_y = projected_delta_y(from.lat, via.lat);
    let out_x = projected_delta_x(via.lon, via.lat, to.lon);
    let out_y = projected_delta_y(via.lat, to.lat);

    let in_norm = (in_x * in_x + in_y * in_y).sqrt();
    let out_norm = (out_x * out_x + out_y * out_y).sqrt();
    if in_norm <= f64::EPSILON || out_norm <= f64::EPSILON {
        return TurnDirection::Straight;
    }

    let dot = ((in_x * out_x + in_y * out_y) / (in_norm * out_norm)).clamp(-1.0, 1.0);
    let cross = in_x * out_y - in_y * out_x;
    let angle = cross.atan2(dot);
    let abs_angle = angle.abs();

    if abs_angle <= STRAIGHT_THRESHOLD_RAD {
        TurnDirection::Straight
    } else if abs_angle >= UTURN_THRESHOLD_RAD {
        TurnDirection::Uturn
    } else if angle > 0.0 {
        TurnDirection::Left
    } else {
        TurnDirection::Right
    }
}

fn projected_delta_x(from_lon: f64, reference_lat: f64, to_lon: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    let lon_delta = (to_lon - from_lon).to_radians();
    lon_delta * reference_lat.to_radians().cos() * earth_radius_m
}

fn projected_delta_y(from_lat: f64, to_lat: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    (to_lat - from_lat).to_radians() * earth_radius_m
}

#[cfg(test)]
mod tests {
    use super::{
        CostConfig, ExcludeRule, FactorRule, FerryConfig, Objective, PreferencesConfig,
        ProfileDocument, ProfileHeader, ReturnConfig, SpeedRule, TagMatch, compile_profile_bundle,
        compile_profile_bundle_with_acceleration,
    };
    use netan_core::{
        AccessMask, CacheBundleId, DatasetAccelerationBundle, DirectedEdge, EdgeBasedTopology,
        EdgeId, NodeId, RoadClass, SmoothnessClass, SurfaceClass, TopologyBundle, TopologyNode,
        TravelMode,
    };
    use std::collections::BTreeMap;

    #[test]
    fn compiles_edge_metrics_from_topology() {
        let profile = ProfileDocument {
            profile: ProfileHeader {
                id: "car_test".to_string(),
                label: "Car test".to_string(),
                mode: TravelMode::Car,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: CostConfig {
                objective: Objective::Fastest,
                distance_weight: 0.0,
                time_weight: 1.0,
            },
            speed_rules: vec![SpeedRule {
                r#match: tag_match([("highway", "residential")]),
                speed_kph: 40.0,
            }],
            exclude_rules: vec![],
            factors: vec![FactorRule {
                r#match: tag_match([("smoothness", "bad")]),
                speed_factor: 0.5,
            }],
            turns: Default::default(),
            ferry: FerryConfig::default(),
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        };
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edges: vec![DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 1_000,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Bad,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            }],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
        };

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics.len(), 1);
        assert!(compiled.edge_metrics[0].travel_time_s.unwrap() > 170.0);
        assert!(compiled.edge_metrics[0].travel_time_s.unwrap() < 190.0);
        assert_eq!(compiled.source_topology_bundle_id.0, "topology-test");
    }

    #[test]
    fn rejects_unsupported_match_keys() {
        let profile = ProfileDocument {
            profile: ProfileHeader {
                id: "bad_profile".to_string(),
                label: "Bad".to_string(),
                mode: TravelMode::Car,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: Default::default(),
            speed_rules: vec![SpeedRule {
                r#match: tag_match([("lanes", "2")]),
                speed_kph: 50.0,
            }],
            exclude_rules: vec![],
            factors: vec![],
            turns: Default::default(),
            ferry: FerryConfig::default(),
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        };
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edges: vec![],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
        };

        let error =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect_err("unsupported matcher should fail");
        assert!(error.to_string().contains("unsupported match key 'lanes'"));
    }

    #[test]
    fn excludes_matching_edges_from_compiled_metrics() {
        let profile = ProfileDocument {
            profile: ProfileHeader {
                id: "foot_test".to_string(),
                label: "Foot test".to_string(),
                mode: TravelMode::Foot,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: Default::default(),
            speed_rules: vec![],
            exclude_rules: vec![ExcludeRule {
                r#match: tag_match([("highway", "primary")]),
            }],
            factors: vec![],
            turns: Default::default(),
            ferry: FerryConfig::default(),
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        };
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Primary,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::FOOT),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 11,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::FOOT),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
        };

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics[0].travel_time_s, None);
        assert_eq!(compiled.edge_metrics[0].generalized_cost, None);
        assert!(compiled.edge_metrics[1].travel_time_s.is_some());
    }

    #[test]
    fn uses_tagged_ferry_duration_when_available() {
        let profile = ferry_profile(true);
        let topology = ferry_topology(Some(900.0));

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics.len(), 1);
        assert_eq!(compiled.edge_metrics[0].travel_time_s, Some(1_200.0));
        assert_eq!(compiled.edge_metrics[0].generalized_cost, Some(1_560.0));
    }

    #[test]
    fn rejects_ferry_without_duration_when_inference_is_disabled() {
        let profile = ferry_profile(false);
        let topology = ferry_topology(None);

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics.len(), 1);
        assert_eq!(compiled.edge_metrics[0].travel_time_s, None);
        assert_eq!(compiled.edge_metrics[0].generalized_cost, None);
    }

    #[test]
    fn stores_turn_costs_in_compiled_profile_bundle() {
        let mut profile = ferry_profile(true);
        profile.turns.left_penalty_s = 7.0;
        profile.turns.right_penalty_s = 3.0;
        profile.turns.uturn_penalty_s = 25.0;
        profile.turns.traffic_signal_penalty_s = 4.0;
        profile.turns.roundabout_entry_penalty_s = 2.0;
        profile.cost.time_weight = 1.5;

        let compiled = compile_profile_bundle(
            &profile,
            &ferry_topology(Some(900.0)),
            CacheBundleId::new("topology-test"),
        )
        .expect("compile succeeds");

        assert_eq!(compiled.turn_costs.left_penalty_s, 7.0);
        assert_eq!(compiled.turn_costs.right_penalty_s, 3.0);
        assert_eq!(compiled.turn_costs.uturn_penalty_s, 25.0);
        assert_eq!(compiled.turn_costs.traffic_signal_penalty_s, 4.0);
        assert_eq!(compiled.turn_costs.roundabout_entry_penalty_s, 2.0);
        assert_eq!(compiled.turn_costs.cost_time_weight, 1.5);
    }

    #[test]
    fn compiles_acceleration_weights_from_dataset_bundle() {
        let profile = ferry_profile(true);
        let topology = TopologyBundle {
            schema_version: 3,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 0.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 1.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 2.0,
                    lat: 0.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 1_000,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 11,
                    length_m: 1_000,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology {
                node_first_out: vec![0, 1, 2, 2],
                node_edge_order: vec![0, 1],
                edge_transition_first_out: vec![0, 1, 1],
                edge_transition_edges: vec![1],
            },
            spatial_index: None,
        };
        let acceleration = DatasetAccelerationBundle {
            schema_version: 1,
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            algorithm: "edge_based_transition_order_v1".to_string(),
            edge_order: vec![0, 1],
            edge_rank: vec![0, 1],
            upward_first_out: vec![0, 1, 1],
            upward_head: vec![1],
            downward_first_out: vec![0, 0, 0],
            downward_head: vec![],
        };

        let compiled = compile_profile_bundle_with_acceleration(
            &profile,
            &topology,
            CacheBundleId::new("topology-test"),
            Some((&acceleration, CacheBundleId::new("accel-test"))),
        )
        .expect("compile succeeds");

        let compiled_acceleration = compiled.acceleration.expect("acceleration compiled");
        assert_eq!(
            compiled_acceleration.source_acceleration_bundle_id.0,
            "accel-test"
        );
        assert_eq!(compiled_acceleration.upward_head, vec![1]);
        assert_eq!(compiled_acceleration.upward_weight.len(), 1);
        assert!(compiled_acceleration.upward_weight[0].is_finite());
        assert!(compiled_acceleration.downward_weight.is_empty());
    }

    fn tag_match<const N: usize>(pairs: [(&str, &str); N]) -> TagMatch {
        let mut tags = BTreeMap::new();
        for (key, value) in pairs {
            tags.insert(key.to_string(), value.to_string());
        }
        TagMatch { tags }
    }

    fn ferry_profile(infer_duration_when_missing: bool) -> ProfileDocument {
        ProfileDocument {
            profile: ProfileHeader {
                id: "ferry_test".to_string(),
                label: "Ferry test".to_string(),
                mode: TravelMode::Car,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: CostConfig {
                objective: Objective::Fastest,
                distance_weight: 0.0,
                time_weight: 1.0,
            },
            speed_rules: vec![],
            exclude_rules: vec![],
            factors: vec![],
            turns: Default::default(),
            ferry: FerryConfig {
                allow: true,
                use_ferry: 0.2,
                boarding_cost_s: 300.0,
                infer_duration_when_missing,
                default_speed_kph: 20.0,
            },
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        }
    }

    fn ferry_topology(duration_s: Option<f64>) -> TopologyBundle {
        TopologyBundle {
            schema_version: 3,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edges: vec![DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 1_000,
                duration_s,
                road_class: RoadClass::Ferry,
                surface: SurfaceClass::Unknown,
                smoothness: SmoothnessClass::Unknown,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            }],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
        }
    }
}

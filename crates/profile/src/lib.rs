use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netan_core::{
    CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle, DirectedEdge, RoadClass,
    SmoothnessClass, SurfaceClass, TopologyBundle, TravelMode,
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
        schema_version: 2,
        profile_id: profile.profile.id.clone(),
        profile_hash,
        mode: profile.profile.mode,
        source_topology_bundle_id,
        edge_metrics,
    })
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

#[cfg(test)]
mod tests {
    use super::{
        CostConfig, ExcludeRule, FactorRule, FerryConfig, Objective, PreferencesConfig,
        ProfileDocument, ProfileHeader, ReturnConfig, SpeedRule, TagMatch, compile_profile_bundle,
    };
    use netan_core::{
        AccessMask, CacheBundleId, DirectedEdge, EdgeId, NodeId, RoadClass, SmoothnessClass,
        SurfaceClass, TopologyBundle, TravelMode,
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
            spatial_index: None,
        }
    }
}

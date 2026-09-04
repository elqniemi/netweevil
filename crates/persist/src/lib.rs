//! Local state layout and IO for the `.netweevil/` directory: dataset,
//! profile, and run manifests plus binary bundle reading and writing.
//!
//! Large bundles use one sectioned fast-load format whose big primitive
//! arrays load with a single memcpy per array from the mapped file.

mod sectioned;

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use memmap2::Mmap;
use netweevil_core::{
    CompiledProfileBundle, DatasetAccelerationBundle, EdgeNameBundle, TopologyBundle,
};
use netweevil_manifest::{CompiledProfileManifest, DatasetManifest, RunManifest};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub bundles_dir: PathBuf,
    pub topology_bundles_dir: PathBuf,
    pub edge_name_bundles_dir: PathBuf,
    pub acceleration_bundles_dir: PathBuf,
    pub metric_bundles_dir: PathBuf,
    pub transit_bundles_dir: PathBuf,
    pub datasets_dir: PathBuf,
    pub transit_feeds_dir: PathBuf,
    pub compiled_profiles_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub reports_dir: PathBuf,
}

impl WorkspacePaths {
    pub fn discover(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let state_dir = root.join(".netweevil");
        let paths = Self {
            root,
            bundles_dir: state_dir.join("bundles"),
            topology_bundles_dir: state_dir.join("bundles").join("topology"),
            edge_name_bundles_dir: state_dir.join("bundles").join("names"),
            acceleration_bundles_dir: state_dir.join("bundles").join("acceleration"),
            metric_bundles_dir: state_dir.join("bundles").join("metrics"),
            transit_bundles_dir: state_dir.join("bundles").join("transit"),
            datasets_dir: state_dir.join("datasets"),
            transit_feeds_dir: state_dir.join("transit_feeds"),
            compiled_profiles_dir: state_dir.join("compiled_profiles"),
            runs_dir: state_dir.join("runs"),
            reports_dir: state_dir.join("reports"),
            state_dir,
        };
        paths.ensure()?;
        Ok(paths)
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.state_dir,
            &self.bundles_dir,
            &self.topology_bundles_dir,
            &self.edge_name_bundles_dir,
            &self.acceleration_bundles_dir,
            &self.metric_bundles_dir,
            &self.transit_bundles_dir,
            &self.datasets_dir,
            &self.transit_feeds_dir,
            &self.compiled_profiles_dir,
            &self.runs_dir,
            &self.reports_dir,
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating workspace directory {}", dir.display()))?;
        }
        Ok(())
    }
}

pub fn write_json<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    write_json_inner(path, value, true)
}

pub fn write_compact_json<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    write_json_inner(path, value, false)
}

fn write_json_inner<T: Serialize>(path: impl AsRef<Path>, value: &T, pretty: bool) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let writer = BufWriter::new(file);
    if pretty {
        serde_json::to_writer_pretty(writer, value).context("serializing JSON")?;
    } else {
        serde_json::to_writer(writer, value).context("serializing JSON")?;
    }
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn write_topology_bundle(path: impl AsRef<Path>, bundle: &TopologyBundle) -> Result<()> {
    sectioned::write_topology_sectioned(path.as_ref(), bundle)
}

pub fn write_edge_name_bundle(path: impl AsRef<Path>, bundle: &EdgeNameBundle) -> Result<()> {
    write_binary(path, bundle)
}

pub fn write_acceleration_bundle(
    path: impl AsRef<Path>,
    bundle: &DatasetAccelerationBundle,
) -> Result<()> {
    sectioned::write_acceleration_sectioned(path.as_ref(), bundle)
}

pub fn read_topology_bundle(path: impl AsRef<Path>) -> Result<TopologyBundle> {
    let path = path.as_ref();
    let mmap = sectioned::map_file(path)?;
    sectioned::read_topology_sectioned(&mmap, path)
        .with_context(|| format!("parsing topology bundle {}", path.display()))
}

pub fn read_edge_name_bundle(path: impl AsRef<Path>) -> Result<EdgeNameBundle> {
    read_binary_mmap(path)
}

pub fn read_acceleration_bundle(path: impl AsRef<Path>) -> Result<DatasetAccelerationBundle> {
    let path = path.as_ref();
    let mmap = sectioned::map_file(path)?;
    sectioned::read_acceleration_sectioned(&mmap, path)
        .with_context(|| format!("parsing acceleration bundle {}", path.display()))
}

pub fn write_compiled_profile_bundle(
    path: impl AsRef<Path>,
    bundle: &CompiledProfileBundle,
) -> Result<()> {
    sectioned::write_compiled_profile_sectioned(path.as_ref(), bundle)
}

pub fn read_compiled_profile_bundle(path: impl AsRef<Path>) -> Result<CompiledProfileBundle> {
    let path = path.as_ref();
    let mmap = sectioned::map_file(path)?;
    sectioned::read_compiled_profile_sectioned(&mmap, path)
        .with_context(|| format!("parsing compiled profile bundle {}", path.display()))
}

fn write_binary<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    bincode::serialize_into(&mut writer, value)
        .with_context(|| format!("serializing binary {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("flushing binary {}", path.display()))
}

fn read_binary_mmap<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| format!("memory-mapping {}", path.display()))?;
    bincode::deserialize(&mmap).with_context(|| format!("parsing binary bundle {}", path.display()))
}

pub fn write_dataset_manifest(
    paths: &WorkspacePaths,
    manifest: &DatasetManifest,
) -> Result<PathBuf> {
    let path = paths
        .datasets_dir
        .join(format!("{}.json", manifest.dataset_id.0));
    write_json(&path, manifest)?;
    Ok(path)
}

pub fn write_compiled_profile_manifest(
    paths: &WorkspacePaths,
    manifest: &CompiledProfileManifest,
) -> Result<PathBuf> {
    let path = paths
        .compiled_profiles_dir
        .join(format!("{}.json", manifest.compile_id));
    write_json(&path, manifest)?;
    Ok(path)
}

pub fn write_run_manifest(paths: &WorkspacePaths, manifest: &RunManifest) -> Result<PathBuf> {
    let path = paths.runs_dir.join(format!("{}.json", manifest.run_id));
    write_json(&path, manifest)?;
    Ok(path)
}

pub fn list_json_files(dir: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir.as_ref())
        .with_context(|| format!("reading directory {}", dir.as_ref().display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension() == Some(OsStr::new("json")) {
            entries.push(path);
        }
    }
    entries.sort();
    Ok(entries)
}

pub fn read_dataset_manifests(paths: &WorkspacePaths) -> Result<Vec<DatasetManifest>> {
    list_json_files(&paths.datasets_dir)?
        .into_iter()
        .map(read_json)
        .collect()
}

pub fn read_dataset_manifest(paths: &WorkspacePaths, dataset_id: &str) -> Result<DatasetManifest> {
    read_json(paths.datasets_dir.join(format!("{dataset_id}.json")))
}

pub fn read_compiled_profile_manifests(
    paths: &WorkspacePaths,
) -> Result<Vec<CompiledProfileManifest>> {
    list_json_files(&paths.compiled_profiles_dir)?
        .into_iter()
        .map(read_json)
        .collect()
}

pub fn read_run_manifest(path: impl AsRef<Path>) -> Result<RunManifest> {
    read_json(path)
}

#[cfg(test)]
mod tests {
    use super::{
        read_acceleration_bundle, read_compiled_profile_bundle, read_edge_name_bundle,
        read_topology_bundle, write_acceleration_bundle, write_compiled_profile_bundle,
        write_edge_name_bundle, write_topology_bundle,
    };
    use netweevil_core::{
        ACCELERATION_BUNDLE_SCHEMA_VERSION, AccessMask, COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION,
        CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle, CompiledTurnCostConfig,
        DatasetAccelerationBundle, DirectedEdge, EdgeId, EdgeNameBundle, FeatureAttributeColumn,
        FeatureAttributeColumnData, FeatureAttributeDefinition, FeatureAttributeTable,
        FeatureAttributeType, MinuteInterval, NodeId, RoadClass, SmoothnessClass, SurfaceClass,
        TOPOLOGY_BUNDLE_SCHEMA_VERSION, TemporalEffect, TemporalRule, TemporalRuleSet,
        TopologyBundle, TopologyEdgeLayers, TopologyNode, TravelMode, TurnRestriction,
        TurnRestrictionKind,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn round_trips_topology_bundle_binary() {
        let bundle = TopologyBundle {
            schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
            source_path: "dataset.osm.pbf".to_string(),
            source_sha256: "abc123".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    lon: 6.5,
                    lat: 53.2,
                    z: 12.5,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    lon: 6.6,
                    lat: 53.3,
                    z: 18.0,
                },
            ],
            edge_layers: TopologyEdgeLayers::from_directed_edges(&[
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 200,
                    length_m: 123,
                    ascent_m: 5.5,
                    descent_m: 0.0,
                    feature_row: 0,
                    source_direction: 1,
                    temporal_rule_id: Some(0),
                    duration_s: None,
                    road_class: RoadClass::Path,
                    surface: SurfaceClass::Gravel,
                    smoothness: SmoothnessClass::Bad,
                    access_mask: AccessMask::new(AccessMask::FOOT),
                    is_toll: false,
                    max_speed_kph: None,
                    lanes: None,
                    name_index: Some(0),
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(0),
                    source_way_id: 201,
                    length_m: 456,
                    ascent_m: 0.0,
                    descent_m: 5.5,
                    feature_row: 1,
                    source_direction: -1,
                    temporal_rule_id: None,
                    duration_s: Some(45.0),
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR | AccessMask::FOOT),
                    is_toll: true,
                    max_speed_kph: None,
                    lanes: None,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 1,
                },
            ]),
            turn_restrictions: vec![TurnRestriction {
                relation_id: 300,
                kind: TurnRestrictionKind::NoTurn,
                edge_path: vec![EdgeId(0)],
                mode_mask: AccessMask::new(AccessMask::FOOT),
            }],
            names: vec!["path name".to_string()],
            edge_based_topology: Default::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0, 0],
            feature_attributes: FeatureAttributeTable {
                row_count: 2,
                strings: vec!["outdoor".to_string(), "paid".to_string()],
                columns: vec![
                    FeatureAttributeColumn {
                        definition: FeatureAttributeDefinition {
                            name: "feature_id".to_string(),
                            semantic_role: Some("feature_id".to_string()),
                            value_type: FeatureAttributeType::Integer,
                            domain: Default::default(),
                        },
                        data: FeatureAttributeColumnData::Integer(vec![Some(200), Some(201)]),
                    },
                    FeatureAttributeColumn {
                        definition: FeatureAttributeDefinition {
                            name: "Location".to_string(),
                            semantic_role: Some("indoor_location".to_string()),
                            value_type: FeatureAttributeType::String,
                            domain: Default::default(),
                        },
                        data: FeatureAttributeColumnData::String(vec![Some(0), Some(1)]),
                    },
                ],
            },
            temporal_rule_sets: vec![TemporalRuleSet {
                rules: vec![TemporalRule {
                    day_mask: netweevil_core::EVERY_DAY,
                    intervals: vec![MinuteInterval {
                        start_minute: 9 * 60,
                        end_minute: 18 * 60,
                    }],
                    effect: TemporalEffect::OpenOnly,
                }],
            }],
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("netweevil-persist-topology-{unique}.bin"));

        write_topology_bundle(&path, &bundle).expect("bundle should serialize");
        let round_tripped = read_topology_bundle(&path).expect("bundle should deserialize");

        assert_eq!(round_tripped.schema_version, bundle.schema_version);
        assert_eq!(round_tripped.source_path, bundle.source_path);
        assert_eq!(round_tripped.source_sha256, bundle.source_sha256);
        assert_eq!(round_tripped.nodes.len(), bundle.nodes.len());
        assert_eq!(round_tripped.nodes[0].z, 12.5);
        assert_eq!(round_tripped.edge_count(), bundle.edge_count());
        assert_eq!(round_tripped.routing_edge(0).ascent_m, 5.5);
        assert_eq!(round_tripped.routing_edge(1).descent_m, 5.5);
        assert_eq!(round_tripped.routing_edge(0).feature_row, 0);
        assert_eq!(round_tripped.routing_edge(0).source_direction, 1);
        assert_eq!(round_tripped.routing_edge(0).temporal_rule_id, Some(0));
        assert!(
            round_tripped
                .feature_attributes
                .value_matches(1, "indoor_location", "paid")
        );
        assert_eq!(round_tripped.temporal_rule_sets.len(), 1);
        assert_eq!(
            round_tripped.turn_restrictions.len(),
            bundle.turn_restrictions.len()
        );
        assert_eq!(round_tripped.names, bundle.names);
        assert_eq!(round_tripped.edge_profile(0).duration_s, None);
        assert_eq!(round_tripped.edge_profile(1).duration_s, Some(45.0));
        assert!(round_tripped.edge_profile(1).is_toll);

        fs::remove_file(path).expect("temporary bundle should be removed");
    }

    #[test]
    fn round_trips_edge_name_bundle_binary() {
        let bundle = EdgeNameBundle {
            names: vec!["main street".to_string(), "harbor road".to_string()],
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("netweevil-persist-edge-names-{unique}.bin"));

        write_edge_name_bundle(&path, &bundle).expect("bundle should serialize");
        let round_tripped =
            read_edge_name_bundle(&path).expect("edge-name bundle should deserialize");

        assert_eq!(round_tripped.names, bundle.names);

        fs::remove_file(path).expect("temporary bundle should be removed");
    }

    #[test]
    fn round_trips_acceleration_bundle_binary() {
        let bundle = DatasetAccelerationBundle {
            schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            algorithm: netweevil_core::CCH_ALGORITHM.to_string(),
            stats: Default::default(),
            edge_order: vec![0, 2, 1],
            edge_rank: vec![0, 2, 1],
            upward_first_out: vec![0, 1, 1, 1],
            upward_head: vec![2],
            downward_first_out: vec![0, 0, 1, 1],
            downward_head: vec![0],
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("netweevil-persist-acceleration-{unique}.bin"));

        write_acceleration_bundle(&path, &bundle).expect("bundle should serialize");
        let round_tripped =
            read_acceleration_bundle(&path).expect("acceleration bundle should deserialize");

        assert_eq!(round_tripped.schema_version, bundle.schema_version);
        assert_eq!(round_tripped.algorithm, bundle.algorithm);
        assert_eq!(round_tripped.stats, bundle.stats);
        assert_eq!(round_tripped.edge_order, bundle.edge_order);
        assert_eq!(round_tripped.edge_rank, bundle.edge_rank);
        assert_eq!(round_tripped.upward_head, bundle.upward_head);
        assert_eq!(round_tripped.downward_head, bundle.downward_head);

        fs::remove_file(path).expect("temporary bundle should be removed");
    }

    #[test]
    fn round_trips_compiled_profile_bundle_with_turn_costs() {
        let bundle = CompiledProfileBundle {
            schema_version: COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig {
                left_penalty_s: 7.0,
                right_penalty_s: 3.0,
                uturn_penalty_s: 20.0,
                traffic_signal_penalty_s: 4.0,
                roundabout_entry_penalty_s: 2.0,
                cost_time_weight: 1.5,
            },
            components: Vec::new(),
            temporal: Default::default(),
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(12.0),
                generalized_cost: Some(18.0),
            }],
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("netweevil-persist-metrics-turns-{unique}.bin"));

        write_compiled_profile_bundle(&path, &bundle).expect("bundle should serialize");
        let round_tripped = read_compiled_profile_bundle(&path).expect("bundle should deserialize");

        assert_eq!(round_tripped.turn_costs.left_penalty_s, 7.0);
        assert_eq!(round_tripped.turn_costs.cost_time_weight, 1.5);
        assert_eq!(round_tripped.edge_metrics.len(), 1);

        fs::remove_file(path).expect("temporary bundle should be removed");
    }
}

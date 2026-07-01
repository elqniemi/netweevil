//! Local state layout and IO for the `.netweevil/` directory: dataset,
//! profile, and run manifests plus binary bundle reading and writing.
//!
//! Large bundles use a sectioned fast-load format whose
//! big primitive arrays load with one memcpy per array from the mapped
//! file; bundles written by earlier versions fall back to bincode parsing.

mod sectioned;

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;
use netweevil_core::{
    CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle, DatasetAccelerationBundle,
    EdgeNameBundle, TopologyBundle, TopologyEdgeLayers, TravelMode,
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
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
    {
        bail!(
            "legacy gzip topology bundles are no longer supported; re-import the dataset to write the current .bin topology format"
        );
    }
    let mmap = sectioned::map_file(path)?;
    if sectioned::is_sectioned(&mmap) {
        return sectioned::read_topology_sectioned(&mmap)
            .with_context(|| format!("parsing topology bundle {}", path.display()));
    }
    // Bincode-era files carry the old node layout (with import-only OSM
    // ids), so they parse through mirror structs, never the live types.
    bincode::deserialize::<BincodeTopologyBundle>(&mmap)
        .map(TopologyBundle::from)
        .or_else(|_| bincode::deserialize::<LegacyTopologyBundle>(&mmap).map(TopologyBundle::from))
        .with_context(|| format!("parsing topology bundle {}", path.display()))
}

pub fn read_edge_name_bundle(path: impl AsRef<Path>) -> Result<EdgeNameBundle> {
    read_binary_mmap(path)
}

pub fn read_acceleration_bundle(path: impl AsRef<Path>) -> Result<DatasetAccelerationBundle> {
    let path = path.as_ref();
    let mmap = sectioned::map_file(path)?;
    if sectioned::is_sectioned(&mmap) {
        return sectioned::read_acceleration_sectioned(&mmap)
            .with_context(|| format!("parsing acceleration bundle {}", path.display()));
    }
    bincode::deserialize(&mmap)
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
    if sectioned::is_sectioned(&mmap) {
        return sectioned::read_compiled_profile_sectioned(&mmap)
            .with_context(|| format!("parsing compiled profile bundle {}", path.display()));
    }
    bincode::deserialize(&mmap)
        .or_else(|_| {
            bincode::deserialize::<LegacyCompiledProfileBundle>(&mmap)
                .map(CompiledProfileBundle::from)
        })
        .with_context(|| format!("parsing compiled profile bundle {}", path.display()))
}

#[derive(serde::Deserialize)]
struct LegacyCompiledProfileBundle {
    schema_version: u32,
    profile_id: String,
    profile_hash: String,
    #[serde(default)]
    mode: TravelMode,
    source_topology_bundle_id: CacheBundleId,
    edge_metrics: Vec<CompiledEdgeMetric>,
}

impl From<LegacyCompiledProfileBundle> for CompiledProfileBundle {
    fn from(value: LegacyCompiledProfileBundle) -> Self {
        Self {
            schema_version: value.schema_version,
            profile_id: value.profile_id,
            profile_hash: value.profile_hash,
            mode: value.mode,
            turn_costs: Default::default(),
            source_topology_bundle_id: value.source_topology_bundle_id,
            acceleration: None,
            edge_metrics: value.edge_metrics,
        }
    }
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

/// Node layout used by every bincode-era bundle: the OSM node id was
/// persisted although only the import pipeline reads it.
#[derive(serde::Deserialize)]
struct LegacyTopologyNode {
    node_id: netweevil_core::NodeId,
    #[allow(dead_code)]
    osm_node_id: i64,
    lon: f64,
    lat: f64,
}

impl From<LegacyTopologyNode> for netweevil_core::TopologyNode {
    fn from(value: LegacyTopologyNode) -> Self {
        Self {
            node_id: value.node_id,
            lon: value.lon,
            lat: value.lat,
        }
    }
}

/// Mirror of the pre-sectioned `TopologyBundle` bincode layout.
#[derive(serde::Deserialize)]
struct BincodeTopologyBundle {
    schema_version: u32,
    source_path: String,
    source_sha256: String,
    nodes: Vec<LegacyTopologyNode>,
    #[serde(default)]
    edge_layers: TopologyEdgeLayers,
    #[serde(default)]
    edges: Vec<netweevil_core::DirectedEdge>,
    #[serde(default)]
    turn_restrictions: Vec<netweevil_core::TurnRestriction>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    edge_based_topology: netweevil_core::EdgeBasedTopology,
    #[serde(default)]
    spatial_index: Option<netweevil_core::NodeSpatialIndex>,
    #[serde(default)]
    node_component_ids: Vec<u32>,
    #[serde(default)]
    edge_component_ids: Vec<u32>,
}

impl From<BincodeTopologyBundle> for TopologyBundle {
    fn from(value: BincodeTopologyBundle) -> Self {
        Self {
            schema_version: value.schema_version,
            source_path: value.source_path,
            source_sha256: value.source_sha256,
            nodes: value.nodes.into_iter().map(Into::into).collect(),
            edge_layers: value.edge_layers,
            edges: value.edges,
            turn_restrictions: value.turn_restrictions,
            names: value.names,
            edge_based_topology: value.edge_based_topology,
            spatial_index: value.spatial_index,
            node_component_ids: value.node_component_ids,
            edge_component_ids: value.edge_component_ids,
        }
    }
}

#[derive(serde::Deserialize)]
struct LegacyTopologyBundle {
    schema_version: u32,
    source_path: String,
    source_sha256: String,
    nodes: Vec<LegacyTopologyNode>,
    edges: Vec<netweevil_core::DirectedEdge>,
    #[serde(default)]
    turn_restrictions: Vec<netweevil_core::TurnRestriction>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    edge_based_topology: netweevil_core::EdgeBasedTopology,
    #[serde(default)]
    spatial_index: Option<netweevil_core::NodeSpatialIndex>,
    #[serde(default)]
    node_component_ids: Vec<u32>,
    #[serde(default)]
    edge_component_ids: Vec<u32>,
}

impl From<LegacyTopologyBundle> for TopologyBundle {
    fn from(value: LegacyTopologyBundle) -> Self {
        let edge_layers = TopologyEdgeLayers::from_directed_edges(&value.edges);
        Self {
            schema_version: value.schema_version,
            source_path: value.source_path,
            source_sha256: value.source_sha256,
            nodes: value.nodes.into_iter().map(Into::into).collect(),
            edge_layers,
            edges: Vec::new(),
            turn_restrictions: value.turn_restrictions,
            names: value.names,
            edge_based_topology: value.edge_based_topology,
            spatial_index: value.spatial_index,
            node_component_ids: value.node_component_ids,
            edge_component_ids: value.edge_component_ids,
        }
    }
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
        read_topology_bundle, write_acceleration_bundle, write_binary, write_edge_name_bundle,
        write_topology_bundle,
    };
    use netweevil_core::DatasetAccelerationBundle;
    use netweevil_core::{
        AccessMask, CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle,
        CompiledTurnCostConfig, DirectedEdge, EdgeId, EdgeNameBundle, NodeId, RoadClass,
        SmoothnessClass, SurfaceClass, TopologyBundle, TopologyNode, TravelMode, TurnRestriction,
        TurnRestrictionKind,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn round_trips_topology_bundle_binary() {
        let bundle = TopologyBundle {
            schema_version: 4,
            source_path: "dataset.osm.pbf".to_string(),
            source_sha256: "abc123".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    lon: 6.5,
                    lat: 53.2,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    lon: 6.6,
                    lat: 53.3,
                },
            ],
            edge_layers: Default::default(),
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 200,
                    length_m: 123,
                    duration_s: None,
                    road_class: RoadClass::Path,
                    surface: SurfaceClass::Gravel,
                    smoothness: SmoothnessClass::Bad,
                    access_mask: AccessMask::new(AccessMask::FOOT),
                    is_toll: false,
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
                    duration_s: Some(45.0),
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR | AccessMask::FOOT),
                    is_toll: true,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 1,
                },
            ],
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
        assert_eq!(round_tripped.edge_count(), bundle.edge_count());
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
            schema_version: netweevil_core::ACCELERATION_BUNDLE_SCHEMA_VERSION,
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
    fn reads_legacy_compiled_profile_bundle_binary() {
        #[derive(serde::Serialize)]
        struct LegacyCompiledProfileBundle {
            schema_version: u32,
            profile_id: String,
            profile_hash: String,
            mode: TravelMode,
            source_topology_bundle_id: CacheBundleId,
            edge_metrics: Vec<CompiledEdgeMetric>,
        }

        let bundle = LegacyCompiledProfileBundle {
            schema_version: 2,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            edge_metrics: vec![CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(12.0),
                generalized_cost: Some(12.0),
            }],
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("netweevil-persist-metrics-legacy-{unique}.bin"));

        write_binary(&path, &bundle).expect("bundle should serialize");
        let round_tripped =
            read_compiled_profile_bundle(&path).expect("legacy bundle should deserialize");

        assert_eq!(round_tripped.profile_id, "test");
        assert_eq!(round_tripped.turn_costs.left_penalty_s, 0.0);
        assert_eq!(round_tripped.edge_metrics.len(), 1);

        fs::remove_file(path).expect("temporary bundle should be removed");
    }

    #[test]
    fn round_trips_compiled_profile_bundle_with_turn_costs() {
        let bundle = CompiledProfileBundle {
            schema_version: 3,
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

        write_binary(&path, &bundle).expect("bundle should serialize");
        let round_tripped = read_compiled_profile_bundle(&path).expect("bundle should deserialize");

        assert_eq!(round_tripped.turn_costs.left_penalty_s, 7.0);
        assert_eq!(round_tripped.turn_costs.cost_time_weight, 1.5);
        assert_eq!(round_tripped.edge_metrics.len(), 1);

        fs::remove_file(path).expect("temporary bundle should be removed");
    }
}

#[cfg(test)]
mod sectioned_compat_tests {
    use super::*;
    use netweevil_core::DatasetAccelerationBundle;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("netweevil-persist-{label}-{unique}.bin"))
    }

    #[test]
    fn reads_bincode_acceleration_bundles_written_before_the_sectioned_format() {
        let bundle = DatasetAccelerationBundle {
            schema_version: netweevil_core::ACCELERATION_BUNDLE_SCHEMA_VERSION,
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
        let path = temp_path("acceleration-bincode-compat");
        write_binary(&path, &bundle).expect("bincode bundle should serialize");
        let round_tripped =
            read_acceleration_bundle(&path).expect("bincode-format bundle should still load");
        assert_eq!(round_tripped.edge_order, bundle.edge_order);
        assert_eq!(round_tripped.upward_head, bundle.upward_head);
        fs::remove_file(path).expect("temporary bundle should be removed");
    }
}

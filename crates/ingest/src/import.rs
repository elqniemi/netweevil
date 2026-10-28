use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use netweevil_core::{
    BuildStage, CacheBundleId, DatasetAccelerationBundle, DatasetId, EdgeNameBundle, SourceFormat,
    TopologyBundle,
};
use netweevil_persist::manifest::{AccelerationOrdering, BundleRef, DatasetManifest, now_rfc3339};
use netweevil_persist::{
    WorkspacePaths, write_acceleration_bundle, write_dataset_manifest, write_edge_name_bundle,
    write_elevation_raster, write_source_snapshot_bundle, write_topology_bundle,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::acceleration::build_dataset_acceleration_bundle_with_progress;
use crate::dem::DemRaster;
use crate::source::OsmSourceSnapshot;
use crate::topology::{EnrichmentInputs, build_topology_bundle};

#[derive(Debug, Clone, Default)]
pub struct DatasetImportOptions {
    pub name: String,
    pub source: String,
    /// Source file format; detected from the path when not set.
    pub format: Option<SourceFormat>,
    /// Required for GeoPackage sources; ignored by other readers.
    pub mapping: Option<PathBuf>,
    /// Admin boundaries (GeoJSON or OSM PBF) used instead of the
    /// `boundary=administrative` relations in an OSM extract, for extracts
    /// that cut country or state boundaries.
    pub admin_boundaries: Option<PathBuf>,
    /// DEM sampled for node heights and edge grades (OSM and Overture).
    pub dem: Option<DemImportOptions>,
}

/// A DEM to sample during import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DemImportOptions {
    /// ESRI ASCII grid (`.asc`) or GeoTIFF.
    pub path: PathBuf,
    /// Raster CRS as `EPSG:<code>` or a `+proj=` definition; GeoTIFFs
    /// normally carry their own.
    pub crs: Option<String>,
    /// Vertical datum label recorded with the heights (`HKPD`, `EGM2008`).
    pub vertical_datum: Option<String>,
}

/// Tile edge length, in cells, of the stored elevation raster.
const ELEVATION_TILE_SIZE: u32 = 256;

/// Infers the source format from the path: a directory or a
/// `.parquet`/`.geoparquet` file is Overture GeoParquet, `.gpkg` is an OGC
/// GeoPackage, and anything else is treated as an OSM PBF extract.
pub fn detect_source_format(source_path: &Path) -> SourceFormat {
    if source_path.is_dir() {
        return SourceFormat::OvertureParquet;
    }
    match source_path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("parquet") | Some("geoparquet") => SourceFormat::OvertureParquet,
        Some("gpkg") => SourceFormat::GeoPackage,
        _ => SourceFormat::OsmPbf,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetImportStage {
    LoadPreviousBuild,
    ApplyChanges,
    ScanRoutableObjects,
    SampleElevation,
    BuildTopology,
    AssignRegions,
    BuildAcceleration,
    WriteTopologyBundle,
    WriteManifest,
    Complete,
}

impl DatasetImportStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::LoadPreviousBuild => "Load Previous Build",
            Self::ApplyChanges => "Apply Changes",
            Self::ScanRoutableObjects => "Scan Routable Objects",
            Self::SampleElevation => "Sample Elevation",
            Self::BuildTopology => "Build Topology",
            Self::AssignRegions => "Assign Regions",
            Self::BuildAcceleration => "Build Acceleration",
            Self::WriteTopologyBundle => "Write Topology Bundle",
            Self::WriteManifest => "Write Dataset Manifest",
            Self::Complete => "Complete",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatasetImportProgress {
    pub stage: DatasetImportStage,
    pub stage_percent: Option<f64>,
    pub message: String,
}

/// Imports one or more sources into a single dataset, reporting staged
/// progress through `progress`. Pass `|_| {}` when progress is not needed.
pub fn import_dataset<F>(
    paths: &WorkspacePaths,
    source_paths: &[PathBuf],
    options: DatasetImportOptions,
    mut progress: F,
) -> Result<DatasetManifest>
where
    F: FnMut(DatasetImportProgress),
{
    if source_paths.is_empty() {
        bail!("dataset import requires at least one source");
    }
    for source_path in source_paths {
        if !source_path.exists() {
            bail!("dataset source does not exist: {}", source_path.display());
        }
    }
    let source_format = options
        .format
        .unwrap_or_else(|| detect_source_format(&source_paths[0]));
    if source_format != SourceFormat::GeoPackage && source_paths.len() != 1 {
        bail!("multi-source dataset import is currently supported only for GeoPackage inputs");
    }
    if source_paths
        .iter()
        .any(|path| detect_source_format(path) != source_format)
    {
        bail!("all dataset sources must use the selected source format");
    }
    let mapping = match (&options.mapping, source_format) {
        (Some(path), SourceFormat::GeoPackage) => Some(crate::load_gpkg_mapping(path)?),
        (None, SourceFormat::GeoPackage) => bail!("GeoPackage import requires `--mapping <path>`"),
        (Some(_), _) => bail!("an ingest mapping is only valid for GeoPackage sources"),
        (None, _) => None,
    };

    let size = source_paths
        .iter()
        .map(|path| source_size(path))
        .sum::<Result<u64>>()?;
    let enrichment = EnrichmentInputs {
        admin_boundaries: options.admin_boundaries.clone(),
        dem: options.dem.clone(),
    };
    let dataset_id = DatasetId::new(options.name);
    let bundles = DatasetBundleRefs::new(paths, &dataset_id);
    let built = build_topology_bundle(
        source_paths,
        size,
        source_format,
        mapping.as_ref(),
        &enrichment,
        &mut progress,
    )?;
    emit_warnings(&built.warnings, &mut progress);
    let acceleration_bundle = build_dataset_acceleration_bundle_with_progress(
        &built.bundle,
        bundles.topology.bundle_id.clone(),
        &mut progress,
    );
    let snapshot = built.osm_source.as_ref().map(|(state, topology_node_ids)| {
        state.to_snapshot(
            bundles.topology.bundle_id.clone(),
            topology_node_ids,
            &enrichment,
        )
    });
    let elevation_bundle = write_dataset_bundles(
        &bundles,
        &built.bundle,
        &built.names,
        &acceleration_bundle,
        built.dem.as_ref(),
        snapshot.as_ref(),
        &mut progress,
    )?;
    let imported_at = now_rfc3339()?;

    let manifest = DatasetManifest {
        dataset_id,
        label: options.source,
        source_path: source_paths[0].display().to_string(),
        source_paths: source_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        source_size_bytes: size,
        source_format,
        build_stage: BuildStage::TopologyReady,
        topology_bundle: Some(bundles.topology),
        edge_name_bundle: Some(bundles.edge_names),
        acceleration_bundle: Some(bundles.acceleration),
        elevation_bundle,
        source_snapshot_bundle: snapshot.is_some().then_some(bundles.source),
        topology_meta: Some(built.meta),
        acceleration_ordering: Some(AccelerationOrdering {
            ordered_at: imported_at.clone(),
            edge_count: built.bundle.edge_count() as u64,
            shortcut_arc_count: acceleration_bundle.stats.shortcut_arc_count,
            patched_updates: 0,
        }),
        acceleration_stats: Some(acceleration_bundle.stats.clone()),
        imported_at,
        last_update: None,
    };

    emit_progress(
        &mut progress,
        DatasetImportStage::WriteManifest,
        None,
        format!("Writing dataset manifest for '{}'", manifest.dataset_id.0),
    );
    write_dataset_manifest(paths, &manifest)?;
    emit_progress(
        &mut progress,
        DatasetImportStage::Complete,
        Some(100.0),
        format!(
            "Imported dataset '{}' with {} nodes and {} directed edges",
            manifest.dataset_id.0,
            manifest
                .topology_meta
                .as_ref()
                .map(|meta| meta.node_count)
                .unwrap_or_default(),
            manifest
                .topology_meta
                .as_ref()
                .map(|meta| meta.edge_count)
                .unwrap_or_default()
        ),
    );
    Ok(manifest)
}

pub(crate) fn emit_warnings(warnings: &[String], progress: &mut impl FnMut(DatasetImportProgress)) {
    for warning in warnings {
        emit_progress(
            progress,
            DatasetImportStage::AssignRegions,
            None,
            format!("Warning: {warning}"),
        );
    }
}

/// Ids and paths of the bundles one build of a dataset writes. Every build
/// gets fresh ids, so compiled profiles never outlive the topology they were
/// built on and an update writes new files next to the ones a running API
/// may still be reading.
pub(crate) struct DatasetBundleRefs {
    pub(crate) topology: BundleRef,
    pub(crate) edge_names: BundleRef,
    pub(crate) acceleration: BundleRef,
    pub(crate) elevation: BundleRef,
    pub(crate) source: BundleRef,
}

impl DatasetBundleRefs {
    pub(crate) fn new(paths: &WorkspacePaths, dataset_id: &DatasetId) -> Self {
        let build = Uuid::new_v4().simple().to_string();
        let bundle = |dir: &Path, kind: &str| {
            let bundle_id = CacheBundleId::new(format!("{kind}-{}-{}", dataset_id.0, &build[..12]));
            let path = dir.join(format!("{}.bin", bundle_id.0));
            BundleRef {
                bundle_id,
                path: path.display().to_string(),
            }
        };
        Self {
            topology: bundle(&paths.topology_bundles_dir, "topology"),
            edge_names: bundle(&paths.edge_name_bundles_dir, "edge-names"),
            acceleration: bundle(&paths.acceleration_bundles_dir, "acceleration"),
            elevation: bundle(&paths.elevation_bundles_dir, "elevation"),
            source: bundle(&paths.source_bundles_dir, "source"),
        }
    }
}

/// Writes the bundles of one dataset build: the elevation raster when a DEM
/// was sampled (its reference is returned), the snapshot only for OSM
/// sources.
pub(crate) fn write_dataset_bundles(
    refs: &DatasetBundleRefs,
    topology: &TopologyBundle,
    names: &EdgeNameBundle,
    acceleration: &DatasetAccelerationBundle,
    dem: Option<&DemRaster>,
    snapshot: Option<&OsmSourceSnapshot>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<Option<BundleRef>> {
    let mut announce = |what: &str, bundle: &BundleRef| {
        emit_progress(
            progress,
            DatasetImportStage::WriteTopologyBundle,
            None,
            format!("Writing {what} bundle {}", bundle.path),
        )
    };
    announce("topology", &refs.topology);
    write_topology_bundle(&refs.topology.path, topology)?;
    announce("edge-name", &refs.edge_names);
    write_edge_name_bundle(&refs.edge_names.path, names)?;
    announce("acceleration", &refs.acceleration);
    write_acceleration_bundle(&refs.acceleration.path, acceleration)?;
    if let Some(snapshot) = snapshot {
        announce("source snapshot", &refs.source);
        write_source_snapshot_bundle(&refs.source.path, snapshot)?;
    }
    let (Some(dem), Some(summary)) = (dem, topology.elevation.dem.as_ref()) else {
        return Ok(None);
    };
    announce("elevation raster", &refs.elevation);
    let (header, tiles) = dem.to_tiles(summary.source.clone(), ELEVATION_TILE_SIZE);
    write_elevation_raster(&refs.elevation.path, &header, &tiles)?;
    Ok(Some(refs.elevation.clone()))
}

/// Size of the dataset source: a single file, or every file of a directory
/// source.
pub(crate) fn source_size(source_path: &Path) -> Result<u64> {
    let metadata = std::fs::metadata(source_path)
        .with_context(|| format!("reading metadata for {}", source_path.display()))?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total_size = 0_u64;
    for entry in std::fs::read_dir(source_path)
        .with_context(|| format!("reading directory {}", source_path.display()))?
    {
        total_size += source_size(&entry?.path())?;
    }
    Ok(total_size)
}

pub(crate) fn emit_progress(
    progress: &mut impl FnMut(DatasetImportProgress),
    stage: DatasetImportStage,
    stage_percent: Option<f64>,
    message: String,
) {
    progress(DatasetImportProgress {
        stage,
        stage_percent,
        message,
    });
}

pub(crate) struct PercentReporter {
    last_bucket: Option<u32>,
}

impl PercentReporter {
    pub(crate) fn starting_at_zero() -> Self {
        Self {
            last_bucket: Some(0),
        }
    }

    pub(crate) fn emit_if_needed<F>(
        &mut self,
        completed: u64,
        total: u64,
        stage: DatasetImportStage,
        progress: &mut impl FnMut(DatasetImportProgress),
        message: F,
    ) where
        F: FnOnce(f64) -> String,
    {
        if total == 0 {
            return;
        }
        let percent = ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0);
        if percent >= 100.0 {
            return;
        }
        let bucket = (percent / 5.0).floor() as u32;
        if self.last_bucket == Some(bucket) {
            return;
        }
        self.last_bucket = Some(bucket);
        let quantized_percent = (bucket * 5) as f64;
        emit_progress(
            progress,
            stage,
            Some(quantized_percent),
            message(quantized_percent),
        );
    }
}

pub(crate) struct CountingReader<R> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
}

impl<R> CountingReader<R> {
    pub(crate) fn new(inner: R, bytes_read: Arc<AtomicU64>) -> Self {
        Self { inner, bytes_read }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes_read.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

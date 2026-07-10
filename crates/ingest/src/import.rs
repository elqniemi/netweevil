use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use netweevil_core::{BuildStage, CacheBundleId, DatasetId, SourceFormat};
use netweevil_manifest::{BundleRef, DatasetManifest, now_rfc3339};
use netweevil_persist::{
    WorkspacePaths, write_acceleration_bundle, write_dataset_manifest, write_edge_name_bundle,
    write_topology_bundle,
};
use sha2::{Digest, Sha256};

use crate::acceleration::build_dataset_acceleration_bundle_with_progress;
use crate::topology::build_topology_bundle;

#[derive(Debug, Clone)]
pub struct DatasetImportOptions {
    pub name: String,
    pub source: String,
    /// Source file format; detected from the path when not set.
    pub format: Option<SourceFormat>,
    /// Required for GeoPackage sources; ignored by other readers.
    pub mapping: Option<PathBuf>,
}

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
    HashSource,
    ScanRoutableObjects,
    LoadNodeCoords,
    BuildTopology,
    BuildAcceleration,
    WriteTopologyBundle,
    WriteManifest,
    Complete,
}

impl DatasetImportStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::HashSource => "Hash Source",
            Self::ScanRoutableObjects => "Scan Routable Objects",
            Self::LoadNodeCoords => "Load Node Coords",
            Self::BuildTopology => "Build Topology",
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

pub fn import_dataset(
    paths: &WorkspacePaths,
    source_path: impl AsRef<Path>,
    options: DatasetImportOptions,
) -> Result<DatasetManifest> {
    import_dataset_with_progress(paths, source_path, options, |_| {})
}

pub fn import_dataset_with_progress<F>(
    paths: &WorkspacePaths,
    source_path: impl AsRef<Path>,
    options: DatasetImportOptions,
    progress: F,
) -> Result<DatasetManifest>
where
    F: FnMut(DatasetImportProgress),
{
    import_dataset_sources_with_progress(
        paths,
        &[source_path.as_ref().to_path_buf()],
        options,
        progress,
    )
}

pub fn import_dataset_sources(
    paths: &WorkspacePaths,
    source_paths: &[PathBuf],
    options: DatasetImportOptions,
) -> Result<DatasetManifest> {
    import_dataset_sources_with_progress(paths, source_paths, options, |_| {})
}

pub fn import_dataset_sources_with_progress<F>(
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

    emit_progress(
        &mut progress,
        DatasetImportStage::HashSource,
        Some(0.0),
        format!("Hashing {} source(s)", source_paths.len()),
    );
    let (sha256, size) = sha256_sources(source_paths, &mut progress)?;
    let dataset_id = DatasetId::new(options.name);
    let bundle_id = CacheBundleId::new(format!("topology-{}-{}", dataset_id.0, &sha256[..12]));
    let bundle_path = paths
        .topology_bundles_dir
        .join(format!("{}.bin", bundle_id.0));
    let edge_name_bundle_id =
        CacheBundleId::new(format!("edge-names-{}-{}", dataset_id.0, &sha256[..12]));
    let edge_name_bundle_path = paths
        .edge_name_bundles_dir
        .join(format!("{}.bin", edge_name_bundle_id.0));
    let acceleration_bundle_id =
        CacheBundleId::new(format!("acceleration-{}-{}", dataset_id.0, &sha256[..12]));
    let acceleration_bundle_path = paths
        .acceleration_bundles_dir
        .join(format!("{}.bin", acceleration_bundle_id.0));
    let (bundle, edge_name_bundle, topology_meta) = build_topology_bundle(
        source_paths,
        size,
        &sha256,
        source_format,
        mapping.as_ref(),
        &mut progress,
    )?;
    let acceleration_bundle =
        build_dataset_acceleration_bundle_with_progress(&bundle, bundle_id.clone(), &mut progress);
    emit_progress(
        &mut progress,
        DatasetImportStage::WriteTopologyBundle,
        None,
        format!("Writing topology bundle {}", bundle_path.display()),
    );
    write_topology_bundle(&bundle_path, &bundle)?;
    emit_progress(
        &mut progress,
        DatasetImportStage::WriteTopologyBundle,
        None,
        format!(
            "Writing edge-name bundle {}",
            edge_name_bundle_path.display()
        ),
    );
    write_edge_name_bundle(&edge_name_bundle_path, &edge_name_bundle)?;
    emit_progress(
        &mut progress,
        DatasetImportStage::WriteTopologyBundle,
        None,
        format!(
            "Writing acceleration bundle {}",
            acceleration_bundle_path.display()
        ),
    );
    write_acceleration_bundle(&acceleration_bundle_path, &acceleration_bundle)?;

    let manifest = DatasetManifest {
        dataset_id,
        label: options.source,
        source_path: source_paths[0].display().to_string(),
        source_paths: source_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        source_sha256: sha256,
        source_size_bytes: size,
        source_format,
        imported_at: now_rfc3339()?,
        build_stage: BuildStage::TopologyReady,
        topology_bundle: Some(BundleRef {
            bundle_id,
            path: bundle_path.display().to_string(),
        }),
        edge_name_bundle: Some(BundleRef {
            bundle_id: edge_name_bundle_id,
            path: edge_name_bundle_path.display().to_string(),
        }),
        acceleration_bundle: Some(BundleRef {
            bundle_id: acceleration_bundle_id,
            path: acceleration_bundle_path.display().to_string(),
        }),
        topology_meta: Some(topology_meta),
        acceleration_stats: Some(acceleration_bundle.stats.clone()),
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

fn sha256_sources<F>(source_paths: &[PathBuf], progress: &mut F) -> Result<(String, u64)>
where
    F: FnMut(DatasetImportProgress),
{
    if source_paths.len() == 1 {
        return sha256_source(&source_paths[0], progress);
    }
    let mut sources = source_paths.to_vec();
    sources.sort();
    let mut aggregate = Sha256::new();
    let mut total_size = 0_u64;
    for source in sources {
        let (digest, size) = sha256_source(&source, progress)?;
        aggregate.update(source.to_string_lossy().as_bytes());
        aggregate.update([0]);
        aggregate.update(digest.as_bytes());
        aggregate.update([0]);
        total_size = total_size.saturating_add(size);
    }
    emit_progress(
        progress,
        DatasetImportStage::HashSource,
        Some(100.0),
        "Hashing sources 100%".to_string(),
    );
    Ok((hex::encode(aggregate.finalize()), total_size))
}

/// Hashes the dataset source: a single file directly, or every parquet file
/// of a directory source (sorted by relative path, with the path mixed into
/// the digest) so renames and content changes both change the hash.
fn sha256_source<F>(source_path: &Path, progress: &mut F) -> Result<(String, u64)>
where
    F: FnMut(DatasetImportProgress),
{
    if source_path.is_file() {
        let file = File::open(source_path)
            .with_context(|| format!("opening dataset source {}", source_path.display()))?;
        let size = file
            .metadata()
            .with_context(|| format!("reading metadata for {}", source_path.display()))?
            .len();
        return Ok((sha256_file(file, size, progress)?, size));
    }

    let mut files = Vec::new();
    let mut stack = vec![source_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("reading directory {}", dir.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();

    let mut hasher = Sha256::new();
    let mut total_size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    for path in files {
        let relative = path.strip_prefix(source_path).unwrap_or(&path);
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let mut reader = BufReader::new(file);
        loop {
            let read = reader
                .read(&mut buffer)
                .context("reading file for hashing")?;
            if read == 0 {
                break;
            }
            total_size += read as u64;
            hasher.update(&buffer[..read]);
        }
    }
    emit_progress(
        progress,
        DatasetImportStage::HashSource,
        Some(100.0),
        "Hashing source 100%".to_string(),
    );
    Ok((hex::encode(hasher.finalize()), total_size))
}

fn sha256_file<F>(file: File, source_size_bytes: u64, progress: &mut F) -> Result<String>
where
    F: FnMut(DatasetImportProgress),
{
    let mut reader = BufReader::new(file);
    let mut buffer = [0_u8; 64 * 1024];
    let mut hasher = Sha256::new();
    let mut reporter = PercentReporter::starting_at_zero();
    let mut total_read = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .context("reading file for hashing")?;
        if read == 0 {
            break;
        }
        total_read += read as u64;
        hasher.update(&buffer[..read]);
        reporter.emit_if_needed(
            total_read,
            source_size_bytes,
            DatasetImportStage::HashSource,
            progress,
            |percent| format!("Hashing source {:.0}%", percent),
        );
    }
    emit_progress(
        progress,
        DatasetImportStage::HashSource,
        Some(100.0),
        "Hashing source 100%".to_string(),
    );
    Ok(hex::encode(hasher.finalize()))
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
    pub(crate) fn new() -> Self {
        Self { last_bucket: None }
    }

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

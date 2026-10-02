//! Incremental dataset updates: apply OSM change files (or a newer extract)
//! to an imported OSM dataset without repeating the nested-dissection
//! ordering.
//!
//! The source snapshot restores the scanned OSM objects; changes are applied
//! to them and the topology is rebuilt by the import code, so it equals the
//! topology a full import of the changed extract builds. The CCH elimination
//! order of the previous build is carried over edge state by edge state (see
//! [`patch_edge_order`]) and the hierarchy is contracted again with it.
//! Contraction stays complete for any order, so routing results are those of
//! a full import; only the shortcut count can drift. Each update measures
//! that drift against the last full ordering and can reorder from scratch.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use netweevil_core::{
    CacheBundleId, DatasetAccelerationBundle, EdgeNameBundle, SourceFormat, TopologyBundle,
    TopologyBundleMeta,
};
use netweevil_manifest::{AccelerationOrdering, DatasetManifest, DatasetUpdateRecord, now_rfc3339};
use netweevil_persist::{
    WorkspacePaths, read_acceleration_bundle, read_dataset_manifest, read_source_snapshot_bundle,
    read_topology_bundle, write_dataset_manifest,
};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};

use crate::acceleration::{
    PatchedOrder, build_with_order, nested_dissection_order, patch_edge_order,
};
use crate::dem::DemRaster;
use crate::import::{
    DatasetBundleRefs, DatasetImportProgress, DatasetImportStage, emit_progress, sha256_source,
    write_dataset_bundles,
};
use crate::import::{emit_warnings, enrichment_key};
use crate::osm_change::{effective_changes, read_osm_change_file};
use crate::source::{OsmSourceSnapshot, OsmSourceState};
use crate::topology::{EnrichmentInputs, build_enriched_topology};

/// Fill growth above which [`ReorderPolicy::Auto`] reorders by default.
pub const DEFAULT_MAX_FILL_GROWTH: f64 = 1.10;

/// What a dataset update applies.
#[derive(Debug, Clone)]
pub enum DatasetUpdateInput {
    /// osmChange files (`.osc` or `.osc.gz`), applied in the given order to
    /// the dataset's source snapshot.
    OsmChanges(Vec<PathBuf>),
    /// A newer OSM PBF extract of the same area, scanned in full.
    Source(PathBuf),
}

/// When an update runs a full nested-dissection ordering instead of
/// patching the previous order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReorderPolicy {
    /// Patch the previous order, and reorder when the patched hierarchy's
    /// fill growth (see [`AccelerationOrdering::fill_growth`]) exceeds
    /// `max_fill_growth`.
    Auto {
        max_fill_growth: f64,
    },
    Always,
    Never,
}

#[derive(Debug, Clone)]
pub struct DatasetUpdateOptions {
    pub dataset_id: String,
    pub input: DatasetUpdateInput,
    pub reorder: ReorderPolicy,
}

/// Source objects the change files touched, one count per object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChangedObjects {
    pub nodes: u64,
    pub ways: u64,
    pub relations: u64,
}

#[derive(Debug, Clone)]
pub struct DatasetUpdateReport {
    pub manifest: DatasetManifest,
    /// `None` when the update scanned a replacement extract.
    pub changed_objects: Option<ChangedObjects>,
    /// Shortcuts of the patched order, also when the update then reordered.
    pub patched_shortcut_arc_count: u64,
    /// Fill growth of the patched order against the last full ordering.
    pub patched_fill_growth: f64,
    /// Wall-clock seconds per update stage, in execution order.
    pub stage_seconds: Vec<(&'static str, f64)>,
}

/// Applies `options.input` to an imported OSM dataset, writes new bundles
/// next to the previous ones and repoints the dataset manifest at them.
/// Profiles compiled for the previous topology are stale afterwards; the API
/// recompiles them when it reloads the dataset.
pub fn update_dataset<F>(
    paths: &WorkspacePaths,
    options: DatasetUpdateOptions,
    mut progress: F,
) -> Result<DatasetUpdateReport>
where
    F: FnMut(DatasetImportProgress),
{
    let started = Instant::now();
    let mut timer = StageTimer::new();
    let mut manifest = read_dataset_manifest(paths, &options.dataset_id)
        .with_context(|| format!("reading dataset manifest for '{}'", options.dataset_id))?;
    if manifest.source_format != SourceFormat::OsmPbf {
        bail!(
            "dataset '{}' was imported from {}; only OSM PBF datasets can be updated",
            options.dataset_id,
            manifest.source_format.label()
        );
    }
    let reimport = || {
        format!(
            "dataset '{}' has no source snapshot; re-import it to enable updates",
            options.dataset_id
        )
    };
    let snapshot_ref = manifest
        .source_snapshot_bundle
        .clone()
        .with_context(reimport)?;
    let topology_ref = manifest.topology_bundle.clone().with_context(reimport)?;
    let acceleration_ref = manifest
        .acceleration_bundle
        .clone()
        .with_context(reimport)?;

    emit_progress(
        &mut progress,
        DatasetImportStage::LoadPreviousBuild,
        None,
        format!("Reading the bundles of dataset '{}'", options.dataset_id),
    );
    let (topology, (acceleration, snapshot)) = rayon::join(
        || read_topology_bundle(&topology_ref.path),
        || {
            rayon::join(
                || read_acceleration_bundle(&acceleration_ref.path),
                || read_source_snapshot_bundle(&snapshot_ref.path),
            )
        },
    );
    let topology = topology?;
    let acceleration = acceleration?;
    let snapshot: OsmSourceSnapshot = snapshot?;
    if snapshot.source_topology_bundle_id != topology_ref.bundle_id
        || acceleration.source_topology_bundle_id != topology_ref.bundle_id
    {
        bail!(
            "the bundles of dataset '{}' belong to different builds; re-import it",
            options.dataset_id
        );
    }
    let enrichment = verified_enrichment(snapshot.extras.enrichment())?;
    let previous = PreviousBuild {
        topology_node_ids: snapshot.topology_node_ids.clone(),
        edges: topology
            .edge_layers
            .routing
            .iter()
            .map(|edge| (edge.from.0, edge.to.0, edge.source_way_id))
            .collect(),
        edge_rank: acceleration.edge_rank,
    };
    timer.lap("load previous build");

    let previous_sha256 = manifest.source_sha256.clone();
    let (state, source_sha256, kind, inputs, changed_objects) = match &options.input {
        DatasetUpdateInput::OsmChanges(files) => {
            if files.is_empty() {
                bail!("dataset update needs at least one change file");
            }
            let mut state = OsmSourceState::from_snapshot(snapshot, &topology)?;
            let mut chained = previous_sha256.clone();
            let mut changed = ChangedObjects::default();
            for file in files {
                emit_progress(
                    &mut progress,
                    DatasetImportStage::ApplyChanges,
                    None,
                    format!("Applying {}", file.display()),
                );
                let (file_sha256, _) = sha256_source(file, &mut |_| {})?;
                let summary = state.apply_changes(effective_changes(read_osm_change_file(file)?));
                changed.nodes += summary.nodes;
                changed.ways += summary.ways;
                changed.relations += summary.relations;
                chained = chain_sha256(&chained, &file_sha256);
            }
            timer.lap("apply changes");
            (state, chained, "osm_change", files.clone(), Some(changed))
        }
        DatasetUpdateInput::Source(path) => {
            let (sha256, size) = sha256_source(path, &mut progress)?;
            if sha256 == previous_sha256 {
                bail!(
                    "dataset '{}' was already built from {}",
                    options.dataset_id,
                    path.display()
                );
            }
            let state = OsmSourceState::scan_pbf(path, size, &mut progress)?;
            manifest.source_path = path.display().to_string();
            manifest.source_paths = vec![manifest.source_path.clone()];
            manifest.source_size_bytes = size;
            timer.lap("scan source");
            (state, sha256, "source", vec![path.clone()], None)
        }
    };
    drop(topology);

    let refs = DatasetBundleRefs::new(
        paths,
        &manifest.dataset_id,
        &enrichment_key(&source_sha256, &enrichment),
    );
    let ordering = manifest.acceleration_ordering.clone().unwrap_or_else(|| {
        // Datasets without an ordering record measure against the build
        // they had.
        AccelerationOrdering {
            ordered_at: manifest.imported_at.clone(),
            edge_count: previous.edge_rank.len() as u64,
            shortcut_arc_count: manifest
                .acceleration_stats
                .as_ref()
                .map(|stats| stats.shortcut_arc_count)
                .unwrap_or_default(),
            patched_updates: 0,
        }
    });
    let rebuilt = rebuild_dataset(
        &state,
        &previous,
        Path::new(&manifest.source_path),
        &source_sha256,
        &enrichment,
        refs.topology.bundle_id.clone(),
        &ordering,
        options.reorder,
        &mut timer,
        &mut progress,
    )?;
    let snapshot = state.to_snapshot(
        refs.topology.bundle_id.clone(),
        &rebuilt.topology_node_ids,
        &enrichment,
    );
    let elevation_bundle = write_dataset_bundles(
        &refs,
        &rebuilt.topology,
        &rebuilt.names,
        &rebuilt.acceleration,
        rebuilt.dem.as_ref(),
        Some(&snapshot),
        &mut progress,
    )?;
    timer.lap("write bundles");

    let updated_at = now_rfc3339()?;
    let stats = rebuilt.acceleration.stats.clone();
    let edge_count = rebuilt.topology.edge_count() as u64;
    let fill_growth = ordering.fill_growth(edge_count, stats.shortcut_arc_count);
    manifest.acceleration_ordering = Some(if rebuilt.reordered {
        AccelerationOrdering {
            ordered_at: updated_at.clone(),
            edge_count,
            shortcut_arc_count: stats.shortcut_arc_count,
            patched_updates: 0,
        }
    } else {
        AccelerationOrdering {
            patched_updates: ordering.patched_updates + 1,
            ..ordering.clone()
        }
    });
    manifest.source_sha256 = source_sha256;
    manifest.topology_bundle = Some(refs.topology);
    manifest.edge_name_bundle = Some(refs.edge_names);
    manifest.acceleration_bundle = Some(refs.acceleration);
    manifest.elevation_bundle = elevation_bundle;
    manifest.source_snapshot_bundle = Some(refs.source);
    manifest.topology_meta = Some(rebuilt.meta);
    manifest.acceleration_stats = Some(stats);
    manifest.last_update = Some(DatasetUpdateRecord {
        updated_at,
        kind: kind.to_string(),
        inputs: inputs
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        previous_source_sha256: previous_sha256,
        reused_edge_states: rebuilt.patched.reused as u64,
        inserted_edge_states: rebuilt.patched.inserted as u64,
        reordered: rebuilt.reordered,
        fill_growth,
        elapsed_s: started.elapsed().as_secs_f64(),
    });
    emit_progress(
        &mut progress,
        DatasetImportStage::WriteManifest,
        None,
        format!("Writing dataset manifest for '{}'", manifest.dataset_id.0),
    );
    write_dataset_manifest(paths, &manifest)?;
    timer.lap("write manifest");
    emit_progress(
        &mut progress,
        DatasetImportStage::Complete,
        Some(100.0),
        format!(
            "Updated dataset '{}' to {} directed edges",
            manifest.dataset_id.0, edge_count
        ),
    );

    Ok(DatasetUpdateReport {
        manifest,
        changed_objects,
        patched_shortcut_arc_count: rebuilt.patched_shortcut_arc_count,
        patched_fill_growth: ordering.fill_growth(edge_count, rebuilt.patched_shortcut_arc_count),
        stage_seconds: timer.stages,
    })
}

/// What an update keeps from the previous build: the OSM ids of its
/// topology nodes, its edge states and their CCH ranks.
pub(crate) struct PreviousBuild {
    pub(crate) topology_node_ids: Vec<i64>,
    /// Tail node, head node and source way of every edge state.
    pub(crate) edges: Vec<(u32, u32, i64)>,
    pub(crate) edge_rank: Vec<u32>,
}

pub(crate) struct RebuiltDataset {
    pub(crate) topology: TopologyBundle,
    pub(crate) names: EdgeNameBundle,
    pub(crate) meta: TopologyBundleMeta,
    pub(crate) acceleration: DatasetAccelerationBundle,
    /// The cropped DEM when the dataset samples one.
    pub(crate) dem: Option<DemRaster>,
    pub(crate) topology_node_ids: Vec<i64>,
    pub(crate) patched: PatchedOrder,
    pub(crate) patched_shortcut_arc_count: u64,
    pub(crate) reordered: bool,
}

/// Builds the topology of `state` and its CCH from the previous build's
/// order, reordering from scratch as `policy` decides.
pub(crate) fn rebuild_dataset(
    state: &OsmSourceState,
    previous: &PreviousBuild,
    source_path: &Path,
    source_sha256: &str,
    enrichment: &EnrichmentInputs,
    topology_bundle_id: CacheBundleId,
    ordering: &AccelerationOrdering,
    policy: ReorderPolicy,
    timer: &mut StageTimer,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<RebuiltDataset> {
    let (scan, topology_node_ids) = state.scan_output(enrichment.admin_boundaries.is_none());
    let built = build_enriched_topology(
        source_path,
        source_sha256,
        scan,
        SourceFormat::OsmPbf,
        enrichment,
        progress,
    )?;
    emit_warnings(&built.warnings, progress);
    let (topology, names, meta, dem) = (built.bundle, built.names, built.meta, built.dem);
    timer.lap("build topology");

    let patched = patch_edge_order(
        &topology,
        &previous_ranks(previous, &topology, &topology_node_ids),
    );
    timer.lap("patch order");
    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(0.0),
        format!(
            "Contracting with the previous order: {} edge states reused, {} inserted",
            patched.reused, patched.inserted
        ),
    );
    let mut acceleration = build_with_order(
        &topology,
        topology_bundle_id.clone(),
        patched.order.clone(),
        progress,
    );
    timer.lap("contract");
    let patched_shortcut_arc_count = acceleration.stats.shortcut_arc_count;
    let growth = ordering.fill_growth(topology.edge_count() as u64, patched_shortcut_arc_count);
    let reordered = match policy {
        ReorderPolicy::Always => true,
        ReorderPolicy::Never => false,
        ReorderPolicy::Auto { max_fill_growth } => growth > max_fill_growth,
    };
    if reordered {
        emit_progress(
            progress,
            DatasetImportStage::BuildAcceleration,
            Some(0.0),
            format!(
                "Ordering {} edge states by nested dissection (patched fill growth {growth:.3})",
                topology.edge_count()
            ),
        );
        let order = nested_dissection_order(&topology);
        timer.lap("reorder");
        acceleration = build_with_order(&topology, topology_bundle_id, order, progress);
        timer.lap("contract reordered");
    }
    Ok(RebuiltDataset {
        topology,
        names,
        meta,
        acceleration,
        dem,
        topology_node_ids,
        patched,
        patched_shortcut_arc_count,
        reordered,
    })
}

/// The previous CCH rank of every edge state of `topology`.
///
/// Edge states are identified by source way, OSM tail and head node, and
/// occurrence (a way can repeat a node pair). States that lost that identity
/// (way splits and merges) fall back to an unclaimed previous state over the
/// same node pair. Unmatched states are new.
pub(crate) fn previous_ranks(
    previous: &PreviousBuild,
    topology: &TopologyBundle,
    topology_node_ids: &[i64],
) -> Vec<Option<u32>> {
    let keys = |node_ids: &[i64], tails_heads_ways: &mut dyn Iterator<Item = (u32, u32, i64)>| {
        let mut occurrences = FxHashMap::<(i64, i64, i64), u32>::default();
        tails_heads_ways
            .map(|(from, to, way)| {
                let pair = (way, node_ids[from as usize], node_ids[to as usize]);
                let occurrence = occurrences.entry(pair).or_default();
                *occurrence += 1;
                (pair, *occurrence)
            })
            .collect::<Vec<_>>()
    };
    let previous_keys = keys(
        &previous.topology_node_ids,
        &mut previous.edges.iter().copied(),
    );
    let mut by_identity = FxHashMap::default();
    by_identity.reserve(previous_keys.len());
    for (edge, key) in previous_keys.iter().enumerate() {
        by_identity.insert(*key, edge as u32);
    }
    let current_keys = keys(
        topology_node_ids,
        &mut topology
            .edge_layers
            .routing
            .iter()
            .map(|edge| (edge.from.0, edge.to.0, edge.source_way_id)),
    );
    let mut ranks = current_keys
        .iter()
        .map(|key| by_identity.remove(key))
        .collect::<Vec<_>>();
    let mut by_node_pair = FxHashMap::<(i64, i64), Vec<u32>>::default();
    for (((_, from, to), _), edge) in by_identity {
        by_node_pair.entry((from, to)).or_default().push(edge);
    }
    for list in by_node_pair.values_mut() {
        // Deterministic claims, highest previous edge last.
        list.sort_unstable_by(|left, right| right.cmp(left));
    }
    for (rank, ((_, from, to), _)) in ranks.iter_mut().zip(&current_keys) {
        if rank.is_none() {
            *rank = by_node_pair
                .get_mut(&(*from, *to))
                .and_then(|edges| edges.pop());
        }
    }
    ranks
        .into_iter()
        .map(|edge| edge.map(|edge| previous.edge_rank[edge as usize]))
        .collect()
}

/// The boundaries file and DEM the dataset was built with, after checking
/// that they are unchanged: an update must apply the inputs a full import
/// would use.
fn verified_enrichment(enrichment: &EnrichmentInputs) -> Result<EnrichmentInputs> {
    let check = |what: &str, path: &Path, expected: &str| -> Result<()> {
        let (digest, _) = sha256_source(path, &mut |_| {}).with_context(|| {
            format!(
                "reading the {what} {} the dataset was built with",
                path.display()
            )
        })?;
        if digest != expected {
            bail!(
                "the {what} {} changed since the dataset was built; re-import the dataset with it",
                path.display()
            );
        }
        Ok(())
    };
    if let Some((path, sha256)) = &enrichment.admin_boundaries {
        check("admin boundaries file", path, sha256)?;
    }
    if let Some((dem, sha256)) = &enrichment.dem {
        check("DEM", &dem.path, sha256)?;
    }
    Ok(enrichment.clone())
}

fn chain_sha256(previous: &str, change: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(previous.as_bytes());
    hasher.update([0]);
    hasher.update(change.as_bytes());
    hex::encode(hasher.finalize())
}

/// Records wall-clock time per update stage.
pub(crate) struct StageTimer {
    last: Instant,
    pub(crate) stages: Vec<(&'static str, f64)>,
}

impl StageTimer {
    pub(crate) fn new() -> Self {
        Self {
            last: Instant::now(),
            stages: Vec::new(),
        }
    }

    pub(crate) fn lap(&mut self, stage: &'static str) {
        let now = Instant::now();
        self.stages
            .push((stage, now.duration_since(self.last).as_secs_f64()));
        self.last = now;
    }
}

#[cfg(test)]
mod tests;

//! Historical traffic speed bundles and their manifests.
//!
//! Bundles live under `.netweevil/bundles/traffic/` and manifests under
//! `.netweevil/traffic/`. The bundle uses the sectioned format: weekly
//! patterns are interned at import, stored as delta varints (adjacent
//! buckets have similar speeds), and the sorted feature ids as gaps.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use netweevil_core::{TRAFFIC_BUNDLE_SCHEMA_VERSION, TrafficSpeedBundle};
use netweevil_manifest::BundleRef;
use serde::{Deserialize, Serialize};

use crate::sectioned::{SectionReader, SectionWriter, map_file, unsupported_format_error};
use crate::{WorkspacePaths, list_json_files, read_json, write_json};

const TRAFFIC_TAG: &str = "traffic_speeds";

/// Provenance and binding statistics of one imported traffic speed set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficManifest {
    pub traffic_id: String,
    /// Dataset whose edges were matched at import. The bundle itself is keyed
    /// by source feature ids and is re-bound to the topology at load.
    pub dataset_id: String,
    pub timezone: String,
    pub bucket_minutes: u16,
    pub pattern_count: usize,
    pub feature_count: usize,
    pub matched_feature_count: usize,
    pub matched_edge_count: usize,
    pub source_paths: Vec<String>,
    pub source_sha256: String,
    pub created_at: String,
    pub bundle: BundleRef,
}

pub fn traffic_manifest_dir(paths: &WorkspacePaths) -> PathBuf {
    paths.state_dir.join("traffic")
}

pub fn traffic_bundle_path(paths: &WorkspacePaths, traffic_id: &str) -> PathBuf {
    paths
        .bundles_dir
        .join("traffic")
        .join(format!("traffic-{traffic_id}.bin"))
}

pub fn write_traffic_manifest(paths: &WorkspacePaths, manifest: &TrafficManifest) -> Result<()> {
    let dir = traffic_manifest_dir(paths);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    write_json(dir.join(format!("{}.json", manifest.traffic_id)), manifest)
}

pub fn read_traffic_manifest(paths: &WorkspacePaths, traffic_id: &str) -> Result<TrafficManifest> {
    let path = traffic_manifest_dir(paths).join(format!("{traffic_id}.json"));
    if !path.exists() {
        bail!("unknown traffic set '{traffic_id}'; import it with `netweevil traffic import`");
    }
    read_json(path)
}

pub fn read_traffic_manifests(paths: &WorkspacePaths) -> Result<Vec<TrafficManifest>> {
    let dir = traffic_manifest_dir(paths);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    list_json_files(dir)?.into_iter().map(read_json).collect()
}

#[derive(Serialize, Deserialize)]
struct TrafficHeader {
    schema_version: u32,
    traffic_id: String,
    timezone: String,
    bucket_minutes: u16,
}

pub fn write_traffic_bundle(path: impl AsRef<Path>, bundle: &TrafficSpeedBundle) -> Result<()> {
    bundle.validate().map_err(anyhow::Error::msg)?;
    let mut writer = SectionWriter::new();
    writer.write_bincode(&TRAFFIC_TAG)?;
    writer.write_bincode(&TrafficHeader {
        schema_version: bundle.schema_version,
        traffic_id: bundle.traffic_id.clone(),
        timezone: bundle.timezone.clone(),
        bucket_minutes: bundle.bucket_minutes,
    })?;
    writer.write_u32s(
        &bundle
            .pattern_speeds
            .iter()
            .map(|speed| u32::from(*speed))
            .collect::<Vec<_>>(),
    )?;
    writer.write_i64s(&bundle.feature_ids)?;
    writer.write_u32s(
        &bundle
            .directions
            .iter()
            .map(|direction| u32::from(*direction < 0))
            .collect::<Vec<_>>(),
    )?;
    writer.write_u32s(&bundle.feature_patterns)?;
    writer.finish(path.as_ref())
}

pub fn read_traffic_bundle(path: impl AsRef<Path>) -> Result<TrafficSpeedBundle> {
    let path = path.as_ref();
    let mmap = map_file(path)?;
    let mut reader = SectionReader::new(&mmap, path)?;
    let tag: String = reader.read_bincode()?;
    if tag != TRAFFIC_TAG {
        bail!(
            "expected a traffic speed bundle in {}, found '{tag}'",
            path.display()
        );
    }
    let header: TrafficHeader = reader.read_bincode()?;
    if header.schema_version != TRAFFIC_BUNDLE_SCHEMA_VERSION {
        return Err(unsupported_format_error(path));
    }
    let pattern_speeds = reader
        .read_u32s()?
        .into_iter()
        .map(|speed| u16::try_from(speed).context("traffic speed out of range"))
        .collect::<Result<Vec<_>>>()?;
    let feature_ids = reader.read_i64s()?;
    let directions = reader
        .read_u32s()?
        .into_iter()
        .map(|backward| if backward == 0 { 1 } else { -1 })
        .collect();
    let feature_patterns = reader.read_u32s()?;
    let bundle = TrafficSpeedBundle {
        schema_version: header.schema_version,
        traffic_id: header.traffic_id,
        timezone: header.timezone,
        bucket_minutes: header.bucket_minutes,
        pattern_speeds,
        feature_ids,
        directions,
        feature_patterns,
    };
    bundle
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid traffic bundle {}: {error}", path.display()))?;
    Ok(bundle)
}

#[cfg(test)]
mod tests;

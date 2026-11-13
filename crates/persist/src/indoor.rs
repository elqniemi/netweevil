//! Indoor sites: one manifest per site under `.netweevil/indoor_sites/` and
//! a bundle directory per site under `.netweevil/bundles/indoor/<site_id>/`
//! holding the engine input (`input.json`, local metres) and the compiled
//! graph (`graph.bin`).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use netweevil_core::LocalFrame;
use serde::{Deserialize, Serialize};

use crate::{WorkspacePaths, list_json_files, read_json, write_json};

/// Version of the site manifest and bundle layout. Older manifests are
/// rejected; re-import the site.
pub const INDOOR_SITE_SCHEMA_VERSION: u32 = 1;

/// One imported indoor site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndoorSiteManifest {
    pub schema_version: u32,
    pub site_id: String,
    pub label: String,
    /// Placement of the site's metre coordinates on the globe.
    pub anchor: LocalFrame,
    pub source: IndoorSiteSource,
    pub created_at: String,
    pub levels: Vec<IndoorSiteLevel>,
    /// Join points between the site and the street network.
    pub entrances: Vec<IndoorSiteEntrance>,
    pub counts: IndoorSiteCounts,
    /// `[min_lon, min_lat, max_lon, max_lat]` of the site geometry.
    pub bbox: [f64; 4],
    /// Convex hull of the site geometry as a closed `[lon, lat]` ring.
    pub footprint: Vec<[f64; 2]>,
    /// Compile warnings, one line each.
    #[serde(default)]
    pub warnings: Vec<String>,
    pub input_path: String,
    pub graph_path: String,
}

/// Where a site came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndoorSiteSource {
    pub path: String,
    /// `input_json`, `input_json_wgs84`, `input_json_crs` or `osm`.
    pub format: String,
    /// The source CRS for `input_json_crs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs: Option<String>,
    /// The bbox filter of an OSM import, `[min_lon, min_lat, max_lon, max_lat]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndoorSiteLevel {
    pub id: String,
    pub z: f64,
}

/// What a site entrance is in the engine graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndoorEntranceKind {
    Portal,
    Node,
    Poi,
}

/// A join point between a site and the street network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndoorSiteEntrance {
    /// Element id in the engine input.
    pub id: String,
    pub kind: IndoorEntranceKind,
    pub level: Option<String>,
    /// Local position, metres.
    pub at: [f64; 3],
    pub lon: f64,
    pub lat: f64,
    /// The outdoor side of the entrance, where it meets the street network.
    pub outdoor_lon: f64,
    pub outdoor_lat: f64,
    /// Whether the entrance itself is step-free (portals only; others true).
    pub step_free: bool,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndoorSiteCounts {
    pub levels: usize,
    pub surfaces: usize,
    pub edges: usize,
    pub nodes: usize,
    pub portals: usize,
    pub barriers: usize,
    pub verticals: usize,
    pub pois: usize,
    pub entrances: usize,
    pub graph_nodes: usize,
    pub graph_arcs: usize,
    pub triangles: usize,
}

pub fn indoor_site_manifest_path(paths: &WorkspacePaths, site_id: &str) -> PathBuf {
    paths.indoor_sites_dir.join(format!("{site_id}.json"))
}

pub fn indoor_site_bundle_dir(paths: &WorkspacePaths, site_id: &str) -> PathBuf {
    paths.indoor_bundles_dir.join(site_id)
}

/// Site ids name files: letters, digits, `-`, `_` and `.` only.
pub fn validate_indoor_site_id(site_id: &str) -> Result<()> {
    if site_id.is_empty()
        || site_id.starts_with('.')
        || !site_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("invalid site id '{site_id}'; use letters, digits, '-', '_' and '.'");
    }
    Ok(())
}

pub fn write_indoor_site_manifest(
    paths: &WorkspacePaths,
    manifest: &IndoorSiteManifest,
) -> Result<PathBuf> {
    validate_indoor_site_id(&manifest.site_id)?;
    std::fs::create_dir_all(&paths.indoor_sites_dir)
        .with_context(|| format!("creating {}", paths.indoor_sites_dir.display()))?;
    let path = indoor_site_manifest_path(paths, &manifest.site_id);
    write_json(&path, manifest)?;
    Ok(path)
}

pub fn read_indoor_site_manifest(
    paths: &WorkspacePaths,
    site_id: &str,
) -> Result<IndoorSiteManifest> {
    validate_indoor_site_id(site_id)?;
    let path = indoor_site_manifest_path(paths, site_id);
    if !path.exists() {
        bail!("unknown indoor site '{site_id}'; import it with `netweevil indoor import`");
    }
    let manifest: IndoorSiteManifest =
        read_json(&path).with_context(|| format!("reading {}", path.display()))?;
    if manifest.schema_version != INDOOR_SITE_SCHEMA_VERSION {
        bail!(
            "indoor site '{site_id}' has schema version {} (expected {INDOOR_SITE_SCHEMA_VERSION}); import it again",
            manifest.schema_version
        );
    }
    Ok(manifest)
}

/// Every readable site manifest, sorted by id.
pub fn read_indoor_site_manifests(paths: &WorkspacePaths) -> Result<Vec<IndoorSiteManifest>> {
    if !paths.indoor_sites_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for path in list_json_files(&paths.indoor_sites_dir)? {
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(read_indoor_site_manifest(paths, id)?);
    }
    out.sort_by(|a, b| a.site_id.cmp(&b.site_id));
    Ok(out)
}

/// Removes a site's manifest and bundle directory.
pub fn remove_indoor_site(paths: &WorkspacePaths, site_id: &str) -> Result<()> {
    validate_indoor_site_id(site_id)?;
    let manifest = indoor_site_manifest_path(paths, site_id);
    if !manifest.exists() {
        bail!("unknown indoor site '{site_id}'");
    }
    std::fs::remove_file(&manifest).with_context(|| format!("removing {}", manifest.display()))?;
    let bundle = indoor_site_bundle_dir(paths, site_id);
    if bundle.exists() {
        std::fs::remove_dir_all(&bundle)
            .with_context(|| format!("removing {}", bundle.display()))?;
    }
    Ok(())
}

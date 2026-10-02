//! Administrative areas (countries and states) for topology nodes.
//!
//! Boundaries come from `boundary=administrative` relations at
//! `admin_level=2` (country) and `admin_level=4` (state) in the imported OSM
//! extract, or from a separate boundaries file (GeoJSON or OSM PBF) for
//! extracts that cut the boundaries. A relation is used only when every
//! member way is present and every ring closes; a partial boundary would
//! misclassify the nodes inside an open ring. Point-in-polygon tests use the
//! even-odd rule over all rings, so holes and enclaves need no role
//! bookkeeping.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_core::{AdminArea, NO_ADMIN_AREA, TopologyNode};
use osmpbfreader::{OsmId, OsmObj, OsmPbfReader, Relation};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `admin_level` values NetWeevil keeps.
const COUNTRY_LEVEL: u8 = 2;
const STATE_LEVEL: u8 = 4;

/// One assembled boundary.
#[derive(Debug, Clone)]
pub(crate) struct AdminBoundary {
    pub(crate) level: u8,
    /// ISO 3166-1 alpha-2 (countries) or ISO 3166-2 (states) code.
    pub(crate) code: Option<String>,
    /// ISO 3166-1 alpha-2 code a state carries itself, as territories with
    /// their own code do (Hong Kong is `HK` at `admin_level=4`).
    pub(crate) country_code: Option<String>,
    pub(crate) name: Option<String>,
    /// Closed rings of `(lon, lat)`; the first point is repeated last.
    pub(crate) rings: Vec<Vec<(f64, f64)>>,
}

/// An admin relation seen while scanning, before its geometry is loaded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AdminRelationCandidate {
    pub(crate) relation_id: i64,
    pub(crate) level: u8,
    pub(crate) code: Option<String>,
    pub(crate) country_code: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) member_way_ids: Vec<i64>,
}

pub(crate) fn parse_admin_relation(relation: &Relation) -> Option<AdminRelationCandidate> {
    let tag = |key: &str| relation.tags.get(key).map(|value| value.as_str());
    if tag("boundary") != Some("administrative") {
        return None;
    }
    let level = tag("admin_level")?.trim().parse::<u8>().ok()?;
    let country_code = tag("ISO3166-1:alpha2")
        .or_else(|| tag("ISO3166-1"))
        .and_then(normalize_country_code);
    let code = match level {
        COUNTRY_LEVEL => country_code.clone(),
        STATE_LEVEL => tag("ISO3166-2").and_then(normalize_code),
        _ => return None,
    };
    let member_way_ids = relation
        .refs
        .iter()
        .filter(|member| matches!(member.role.as_str(), "outer" | "inner" | ""))
        .filter_map(|member| match member.member {
            OsmId::Way(way_id) => Some(way_id.0),
            _ => None,
        })
        .collect::<Vec<_>>();
    (!member_way_ids.is_empty()).then(|| AdminRelationCandidate {
        relation_id: relation.id.0,
        level,
        code,
        country_code,
        name: tag("name").map(str::to_string),
        member_way_ids,
    })
}

fn normalize_code(raw: &str) -> Option<String> {
    let code = raw.trim().to_ascii_uppercase();
    (!code.is_empty() && code != "-99").then_some(code)
}

fn normalize_country_code(raw: &str) -> Option<String> {
    normalize_code(raw).filter(|code| code.len() == 2)
}

/// Assembles candidates into boundaries from their member ways' node lists
/// and node coordinates. Returns the boundaries and the ids of relations
/// dropped because a way or node is missing or a ring does not close.
pub(crate) fn assemble_boundaries(
    candidates: &[AdminRelationCandidate],
    way_nodes: &FxHashMap<i64, Vec<i64>>,
    node_coords: &FxHashMap<i64, (f64, f64)>,
) -> (Vec<AdminBoundary>, Vec<i64>) {
    let mut boundaries = Vec::new();
    let mut incomplete = Vec::new();
    for candidate in candidates {
        let ways = candidate
            .member_way_ids
            .iter()
            .map(|way_id| way_nodes.get(way_id).cloned())
            .collect::<Option<Vec<_>>>();
        let rings = ways.and_then(|ways| close_rings(ways)).and_then(|rings| {
            rings
                .into_iter()
                .map(|ring| {
                    ring.iter()
                        .map(|node_id| node_coords.get(node_id).copied())
                        .collect::<Option<Vec<_>>>()
                })
                .collect::<Option<Vec<_>>>()
        });
        match rings {
            Some(rings) if !rings.is_empty() => boundaries.push(AdminBoundary {
                level: candidate.level,
                code: candidate.code.clone(),
                country_code: candidate.country_code.clone(),
                name: candidate.name.clone(),
                rings,
            }),
            _ => incomplete.push(candidate.relation_id),
        }
    }
    (boundaries, incomplete)
}

/// Joins way node sequences end to end into closed rings. `None` when any
/// chain cannot be closed.
fn close_rings(ways: Vec<Vec<i64>>) -> Option<Vec<Vec<i64>>> {
    let mut pending = ways
        .into_iter()
        .filter(|way| way.len() >= 2)
        .map(Some)
        .collect::<Vec<_>>();
    let mut by_endpoint: FxHashMap<i64, Vec<usize>> = FxHashMap::default();
    for (index, way) in pending.iter().enumerate() {
        let way = way.as_ref().expect("all ways are pending");
        by_endpoint.entry(way[0]).or_default().push(index);
        by_endpoint
            .entry(*way.last().unwrap())
            .or_default()
            .push(index);
    }
    let mut rings = Vec::new();
    for start in 0..pending.len() {
        let Some(mut ring) = pending[start].take() else {
            continue;
        };
        while ring.first() != ring.last() {
            let tail = *ring.last().unwrap();
            let next = by_endpoint
                .get(&tail)?
                .iter()
                .copied()
                .find(|index| pending[*index].is_some())?;
            let mut way = pending[next].take().unwrap();
            if way[0] != tail {
                way.reverse();
            }
            ring.extend_from_slice(&way[1..]);
        }
        if ring.len() >= 4 {
            rings.push(ring);
        }
    }
    Some(rings)
}

/// Reads the admin boundaries of an OSM PBF: relations first, then their
/// member ways, then the ways' nodes.
pub(crate) fn read_osm_boundaries(path: &Path) -> Result<(Vec<AdminBoundary>, Vec<i64>)> {
    let open = || -> Result<OsmPbfReader<File>> {
        Ok(OsmPbfReader::new(File::open(path).with_context(|| {
            format!("opening boundaries {}", path.display())
        })?))
    };
    let mut candidates = Vec::new();
    for object in open()?.iter() {
        if let OsmObj::Relation(relation) = object.context("reading PBF relation")?
            && let Some(candidate) = parse_admin_relation(&relation)
        {
            candidates.push(candidate);
        }
    }
    let way_nodes = load_boundary_ways(path, &candidates)?;
    let node_ids = boundary_node_ids(&way_nodes);
    let mut node_coords = FxHashMap::default();
    for object in open()?.iter() {
        if let OsmObj::Node(node) = object.context("reading PBF node")?
            && node_ids.contains(&node.id.0)
        {
            node_coords.insert(node.id.0, (node.lon(), node.lat()));
        }
    }
    Ok(assemble_boundaries(&candidates, &way_nodes, &node_coords))
}

/// Node lists of the member ways of `candidates`.
pub(crate) fn load_boundary_ways(
    path: &Path,
    candidates: &[AdminRelationCandidate],
) -> Result<FxHashMap<i64, Vec<i64>>> {
    let wanted = candidates
        .iter()
        .flat_map(|candidate| candidate.member_way_ids.iter().copied())
        .collect::<FxHashSet<_>>();
    let mut way_nodes = FxHashMap::default();
    if wanted.is_empty() {
        return Ok(way_nodes);
    }
    let file =
        File::open(path).with_context(|| format!("opening boundaries {}", path.display()))?;
    for object in OsmPbfReader::new(file).iter() {
        if let OsmObj::Way(way) = object.context("reading PBF way")?
            && wanted.contains(&way.id.0)
        {
            way_nodes.insert(way.id.0, way.nodes.iter().map(|node| node.0).collect());
        }
    }
    Ok(way_nodes)
}

pub(crate) fn boundary_node_ids(way_nodes: &FxHashMap<i64, Vec<i64>>) -> FxHashSet<i64> {
    way_nodes.values().flatten().copied().collect()
}

/// Reads a GeoJSON FeatureCollection of Polygon/MultiPolygon features.
/// `admin_level` selects country (2) or state (4); without it, a feature
/// with an ISO 3166-2 code is a state and otherwise a country.
pub(crate) fn read_geojson_boundaries(path: &Path) -> Result<Vec<AdminBoundary>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading boundaries {}", path.display()))?;
    let document: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing GeoJSON boundaries {}", path.display()))?;
    let features = match document.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => document
            .get("features")
            .and_then(Value::as_array)
            .context("FeatureCollection without features")?
            .iter()
            .collect::<Vec<_>>(),
        Some("Feature") => vec![&document],
        _ => bail!("boundaries GeoJSON must be a FeatureCollection or Feature"),
    };
    let mut boundaries = Vec::new();
    for (index, feature) in features.into_iter().enumerate() {
        let properties = feature.get("properties").unwrap_or(&Value::Null);
        let text = |keys: &[&str]| {
            keys.iter().find_map(|key| match properties.get(*key)? {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                _ => None,
            })
        };
        let state_code = text(&["ISO3166-2", "iso_3166_2", "iso3166_2", "state_code"])
            .and_then(|code| normalize_code(&code))
            .filter(|code| code.contains('-'));
        let level = match text(&["admin_level", "ADMIN_LEVEL"]) {
            Some(level) => level
                .trim()
                .parse::<u8>()
                .with_context(|| format!("feature {index}: invalid admin_level '{level}'"))?,
            None if state_code.is_some() => STATE_LEVEL,
            None => COUNTRY_LEVEL,
        };
        let country_code = text(&[
            "ISO3166-1:alpha2",
            "ISO3166-1",
            "ISO_A2_EH",
            "ISO_A2",
            "iso_a2",
            "country_code",
        ])
        .and_then(|code| normalize_country_code(&code));
        let code = match level {
            COUNTRY_LEVEL => country_code.clone(),
            STATE_LEVEL => state_code,
            _ => continue,
        };
        let geometry = feature
            .get("geometry")
            .with_context(|| format!("feature {index} has no geometry"))?;
        let rings = geojson_rings(geometry)
            .with_context(|| format!("feature {index}: unsupported boundary geometry"))?;
        if rings.is_empty() {
            continue;
        }
        boundaries.push(AdminBoundary {
            level,
            code,
            country_code,
            name: text(&["name", "NAME", "name_en", "NAME_EN"]),
            rings,
        });
    }
    Ok(boundaries)
}

fn geojson_rings(geometry: &Value) -> Result<Vec<Vec<(f64, f64)>>> {
    let coordinates = geometry
        .get("coordinates")
        .context("geometry has no coordinates")?;
    let polygons = match geometry.get("type").and_then(Value::as_str) {
        Some("Polygon") => vec![coordinates],
        Some("MultiPolygon") => coordinates
            .as_array()
            .context("MultiPolygon coordinates must be an array")?
            .iter()
            .collect(),
        other => bail!("expected Polygon or MultiPolygon, found {other:?}"),
    };
    let mut rings = Vec::new();
    for polygon in polygons {
        for ring in polygon.as_array().context("polygon must be an array")? {
            let points = ring
                .as_array()
                .context("ring must be an array")?
                .iter()
                .map(|point| {
                    let lon = point.get(0).and_then(Value::as_f64);
                    let lat = point.get(1).and_then(Value::as_f64);
                    lon.zip(lat).context("ring position must be [lon, lat]")
                })
                .collect::<Result<Vec<_>>>()?;
            if points.len() >= 3 {
                rings.push(points);
            }
        }
    }
    Ok(rings)
}

/// Reads a boundaries file: `.geojson`/`.json` as GeoJSON, anything else as
/// an OSM PBF.
pub(crate) fn read_boundaries_file(path: &Path) -> Result<(Vec<AdminBoundary>, Vec<i64>)> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("geojson") | Some("json") => Ok((read_geojson_boundaries(path)?, Vec::new())),
        _ => read_osm_boundaries(path),
    }
}

/// A polygon with its edges bucketed into latitude bands, so a crossing
/// test only visits the edges spanning the query latitude.
struct IndexedPolygon {
    min_lon: f64,
    min_lat: f64,
    max_lon: f64,
    max_lat: f64,
    band_height: f64,
    bands: Vec<Vec<[f64; 4]>>,
    area: f64,
}

impl IndexedPolygon {
    fn new(rings: &[Vec<(f64, f64)>]) -> Option<Self> {
        let points = rings.iter().flatten();
        let (mut min_lon, mut min_lat) = (f64::INFINITY, f64::INFINITY);
        let (mut max_lon, mut max_lat) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(lon, lat) in points {
            min_lon = min_lon.min(lon);
            min_lat = min_lat.min(lat);
            max_lon = max_lon.max(lon);
            max_lat = max_lat.max(lat);
        }
        if !(min_lat < max_lat && min_lon < max_lon) {
            return None;
        }
        let edges = rings
            .iter()
            .flat_map(|ring| {
                ring.windows(2)
                    .map(|pair| [pair[0].0, pair[0].1, pair[1].0, pair[1].1])
                    .chain(
                        (ring.first() != ring.last())
                            .then(|| {
                                let (first, last) = (ring[0], ring[ring.len() - 1]);
                                [last.0, last.1, first.0, first.1]
                            })
                            .into_iter(),
                    )
            })
            .collect::<Vec<_>>();
        let band_count = (edges.len() / 8).clamp(1, 1 << 16);
        let band_height = (max_lat - min_lat) / band_count as f64;
        let band_of = |lat: f64| {
            (((lat - min_lat) / band_height).floor() as i64).clamp(0, band_count as i64 - 1)
                as usize
        };
        let mut bands = vec![Vec::new(); band_count];
        let mut area = 0.0;
        for edge in edges {
            area += edge[0] * edge[3] - edge[2] * edge[1];
            let (low, high) = (edge[1].min(edge[3]), edge[1].max(edge[3]));
            for band in &mut bands[band_of(low)..=band_of(high)] {
                band.push(edge);
            }
        }
        Some(Self {
            min_lon,
            min_lat,
            max_lon,
            max_lat,
            band_height,
            bands,
            area: (area / 2.0).abs(),
        })
    }

    fn contains(&self, lon: f64, lat: f64) -> bool {
        if lon < self.min_lon || lon > self.max_lon || lat < self.min_lat || lat > self.max_lat {
            return false;
        }
        let band =
            (((lat - self.min_lat) / self.band_height).floor() as usize).min(self.bands.len() - 1);
        let mut inside = false;
        for &[x1, y1, x2, y2] in &self.bands[band] {
            if (y1 > lat) != (y2 > lat) && lon < (x2 - x1) * (lat - y1) / (y2 - y1) + x1 {
                inside = !inside;
            }
        }
        inside
    }
}

/// Country and state polygons for point lookups.
pub(crate) struct AdminIndex {
    countries: Vec<(IndexedPolygon, usize)>,
    states: Vec<(IndexedPolygon, usize)>,
    boundaries: Vec<AdminBoundary>,
}

impl AdminIndex {
    pub(crate) fn new(boundaries: Vec<AdminBoundary>) -> Self {
        let mut countries = Vec::new();
        let mut states = Vec::new();
        for (index, boundary) in boundaries.iter().enumerate() {
            let Some(polygon) = IndexedPolygon::new(&boundary.rings) else {
                continue;
            };
            match boundary.level {
                COUNTRY_LEVEL => countries.push((polygon, index)),
                STATE_LEVEL => states.push((polygon, index)),
                _ => {}
            }
        }
        // Where boundaries overlap the smallest one wins.
        countries.sort_by(|left, right| left.0.area.total_cmp(&right.0.area));
        states.sort_by(|left, right| left.0.area.total_cmp(&right.0.area));
        Self {
            countries,
            states,
            boundaries,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.countries.is_empty() && self.states.is_empty()
    }

    pub(crate) fn lookup(&self, lon: f64, lat: f64) -> Option<AdminArea> {
        let find = |polygons: &[(IndexedPolygon, usize)]| {
            polygons
                .iter()
                .find(|(polygon, _)| polygon.contains(lon, lat))
                .map(|(_, index)| &self.boundaries[*index])
        };
        let country = find(&self.countries);
        let state = find(&self.states);
        if country.is_none() && state.is_none() {
            return None;
        }
        let state_code = state.and_then(|state| state.code.clone());
        let country_code = country
            .and_then(|country| country.code.clone())
            .or_else(|| state.and_then(|state| state.country_code.clone()))
            .or_else(|| {
                state_code
                    .as_deref()
                    .and_then(|code| code.split_once('-'))
                    .map(|(country, _)| country.to_string())
                    .filter(|country| country.len() == 2)
            })
            .unwrap_or_default();
        Some(AdminArea {
            country_code,
            country_name: country.and_then(|country| country.name.clone()),
            state_code,
            state_name: state.and_then(|state| state.name.clone()),
        })
    }

    /// Interned admin areas and the per-node index into them.
    pub(crate) fn assign(&self, nodes: &[TopologyNode]) -> (Vec<AdminArea>, Vec<u32>) {
        let areas = nodes
            .par_iter()
            .map(|node| self.lookup(node.lon, node.lat))
            .collect::<Vec<_>>();
        let mut table = Vec::new();
        let mut lookup = BTreeMap::new();
        let indexes = areas
            .into_iter()
            .map(|area| match area {
                None => NO_ADMIN_AREA,
                Some(area) => *lookup.entry(area.clone()).or_insert_with(|| {
                    table.push(area);
                    (table.len() - 1) as u32
                }),
            })
            .collect();
        (table, indexes)
    }
}

#[cfg(test)]
mod tests;

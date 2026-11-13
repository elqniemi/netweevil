//! Compiling and storing a site: the engine input is compiled, its entrances
//! are listed with their outdoor side, and the manifest, input and graph are
//! written under `.netweevil/`.

use anyhow::{Context, Result};
use netweevil_core::LocalFrame;
use netweevil_indoor::{CompileReport, Element, ElementKind, Input, Router, compile, save_graph};
use netweevil_persist::indoor::{
    INDOOR_SITE_SCHEMA_VERSION, IndoorEntranceKind, IndoorSiteCounts, IndoorSiteEntrance,
    IndoorSiteLevel, IndoorSiteManifest, IndoorSiteSource, indoor_site_bundle_dir,
    validate_indoor_site_id, write_indoor_site_manifest,
};
use netweevil_persist::manifest::now_rfc3339;
use netweevil_persist::{WorkspacePaths, write_json};
use serde_json::Value;

/// The tag (or portal / node kind) that marks an entrance.
pub const ENTRANCE_TAG: &str = "entrance";

/// A stored site and the compile report of its graph.
pub struct BuiltSite {
    pub manifest: IndoorSiteManifest,
    pub report: CompileReport,
}

/// Compiles `input` (local metres placed by `frame`), lists its entrances and
/// writes the site. An existing site with the same id is replaced.
pub fn build_site(
    paths: &WorkspacePaths,
    site_id: &str,
    label: Option<String>,
    input: Input,
    frame: LocalFrame,
    source: IndoorSiteSource,
) -> Result<BuiltSite> {
    validate_indoor_site_id(site_id)?;
    let (graph, report) = compile(&input);
    let router = Router::new(graph);
    let entrances = find_entrances(&router, &input, &frame);
    let (bbox, footprint) = footprint(&input, &frame);
    let counts = IndoorSiteCounts {
        levels: input.levels.len(),
        surfaces: input.surfaces.len(),
        edges: input.edges.len(),
        nodes: input.nodes.len(),
        portals: input.portals.len(),
        barriers: input.barriers.len(),
        verticals: input.verticals.len(),
        pois: input.pois.len(),
        entrances: entrances.len(),
        graph_nodes: report.nodes,
        graph_arcs: report.arcs,
        triangles: report.triangles,
    };
    let dir = indoor_site_bundle_dir(paths, site_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let input_path = dir.join("input.json");
    let graph_path = dir.join("graph.bin");
    write_json(&input_path, &input)?;
    save_graph(&router.graph, &graph_path)?;
    let manifest = IndoorSiteManifest {
        schema_version: INDOOR_SITE_SCHEMA_VERSION,
        site_id: site_id.to_string(),
        label: label.unwrap_or_else(|| site_id.to_string()),
        anchor: frame,
        source,
        created_at: now_rfc3339()?,
        levels: router
            .levels()
            .into_iter()
            .map(|l| IndoorSiteLevel { id: l.id, z: l.z })
            .collect(),
        entrances,
        counts,
        bbox,
        footprint,
        warnings: report.warnings.iter().map(|w| w.to_string()).collect(),
        input_path: input_path.display().to_string(),
        graph_path: graph_path.display().to_string(),
    };
    write_indoor_site_manifest(paths, &manifest)?;
    Ok(BuiltSite { manifest, report })
}

fn is_entrance(e: &Element) -> bool {
    e.tags.iter().any(|t| t == ENTRANCE_TAG) || e.subkind.as_deref() == Some(ENTRANCE_TAG)
}

/// `props.outdoor` (`[x, y]` local metres) of an element, when given.
fn outdoor_override(props: &Value) -> Option<[f64; 2]> {
    let v = props.get("outdoor")?.as_array()?;
    Some([v.first()?.as_f64()?, v.get(1)?.as_f64()?])
}

/// Every node, portal and POI tagged `entrance` (or of kind `entrance`).
///
/// The outdoor side of a portal is its side that is not on an indoor
/// surface (no surface, or a surface without walls and without the
/// `indoor` tag); when both or neither are, the side farther from the site
/// centre. Nodes and POIs are their own outdoor side. `props.outdoor` sets
/// it explicitly.
pub fn find_entrances(
    router: &Router,
    input: &Input,
    frame: &LocalFrame,
) -> Vec<IndoorSiteEntrance> {
    let centre = centre(input);
    let indoor_space = |space: &str| {
        input.surfaces.iter().any(|s| {
            (s.space.as_deref() == Some(space) || s.id.as_deref() == Some(space))
                && (s.walls || s.tags.iter().any(|t| t == "indoor"))
        })
    };
    let mut out = vec![];
    for (kind, ekind) in [
        (ElementKind::Portal, IndoorEntranceKind::Portal),
        (ElementKind::Node, IndoorEntranceKind::Node),
        (ElementKind::Poi, IndoorEntranceKind::Poi),
    ] {
        for e in router.elements(kind) {
            if !is_entrance(&e) {
                continue;
            }
            let Some(at) = e.at else { continue };
            let outdoor = match outdoor_override(&e.props) {
                Some(p) => p,
                None if ekind == IndoorEntranceKind::Portal && e.points.len() == 2 => {
                    let inside: Vec<bool> = e
                        .points
                        .iter()
                        .map(|p| p.space.as_deref().is_some_and(indoor_space))
                        .collect();
                    let far = |p: &netweevil_indoor::ElementPoint| {
                        (p.at[0] - centre[0]).powi(2) + (p.at[1] - centre[1]).powi(2)
                    };
                    let k = match (inside[0], inside[1]) {
                        (true, false) => 1,
                        (false, true) => 0,
                        _ => usize::from(far(&e.points[1]) > far(&e.points[0])),
                    };
                    [e.points[k].at[0], e.points[k].at[1]]
                }
                None => [at[0], at[1]],
            };
            let step_free = match ekind {
                IndoorEntranceKind::Portal => input
                    .portals
                    .iter()
                    .find(|p| p.id.as_deref() == Some(e.id.as_str()))
                    .is_none_or(|p| p.step_free),
                _ => true,
            };
            let [lon, lat] = frame.to_lonlat(at[0], at[1]);
            let [outdoor_lon, outdoor_lat] = frame.to_lonlat(outdoor[0], outdoor[1]);
            out.push(IndoorSiteEntrance {
                id: e.id.clone(),
                kind: ekind,
                level: e.level.clone(),
                at,
                lon,
                lat,
                outdoor_lon,
                outdoor_lat,
                step_free,
                name: e
                    .props
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            });
        }
    }
    out
}

fn points(input: &Input) -> Vec<[f64; 2]> {
    let mut pts = vec![];
    for s in &input.surfaces {
        for ring in s.polygon.iter().flatten() {
            pts.extend(ring.iter().filter(|v| v.len() >= 2).map(|v| [v[0], v[1]]));
        }
        for poly in s.multipolygon.iter().flatten() {
            for ring in poly {
                pts.extend(ring.iter().filter(|v| v.len() >= 2).map(|v| [v[0], v[1]]));
            }
        }
    }
    if pts.is_empty() {
        for e in &input.edges {
            pts.extend(e.line.iter().filter(|v| v.len() >= 2).map(|v| [v[0], v[1]]));
        }
        for n in &input.nodes {
            pts.extend(n.at.iter().filter(|v| v.len() >= 2).map(|v| [v[0], v[1]]));
        }
    }
    pts
}

fn centre(input: &Input) -> [f64; 2] {
    let pts = points(input);
    if pts.is_empty() {
        return [0.0, 0.0];
    }
    let n = pts.len() as f64;
    [
        pts.iter().map(|p| p[0]).sum::<f64>() / n,
        pts.iter().map(|p| p[1]).sum::<f64>() / n,
    ]
}

/// Bbox and convex hull (closed ring) of the site, in lon/lat.
fn footprint(input: &Input, frame: &LocalFrame) -> ([f64; 4], Vec<[f64; 2]>) {
    let hull = convex_hull(points(input));
    let ring: Vec<[f64; 2]> = hull
        .iter()
        .chain(hull.first())
        .map(|p| frame.to_lonlat(p[0], p[1]))
        .collect();
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for p in &ring {
        b = [
            b[0].min(p[0]),
            b[1].min(p[1]),
            b[2].max(p[0]),
            b[3].max(p[1]),
        ];
    }
    if !b[0].is_finite() {
        b = [frame.lon, frame.lat, frame.lon, frame.lat];
    }
    (b, ring)
}

/// Andrew's monotone chain; counter-clockwise, without the closing point.
pub(crate) fn convex_hull(mut pts: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut lower: Vec<[f64; 2]> = vec![];
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<[f64; 2]> = vec![];
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Whether `[lon, lat]` lies inside a closed ring (even-odd).
pub fn ring_contains(ring: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut inside = false;
    let n = ring.len();
    if n < 4 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

//! Indoor site sources: engine input documents in local metres, in WGS84
//! or in a projected CRS, and OpenStreetMap data with Simple Indoor Tagging
//! (`indoor/osm.rs`). Every source becomes one engine [`Input`] in local
//! metres plus the [`LocalFrame`] that places it on the globe.

mod osm;
#[cfg(test)]
mod tests;

use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_core::{LocalFrame, RasterCrs};
use netweevil_indoor::Input;
use netweevil_persist::indoor::IndoorSiteSource;
use proj4rs::Proj;

pub use osm::{OsmIndoorStats, STOREY_HEIGHT_M, osm_indoor_input, read_osm_indoor};

/// How to place and filter an indoor source.
#[derive(Debug, Clone, Default)]
pub struct IndoorSourceOptions {
    /// The site origin. Required for an input in local metres; otherwise
    /// the centroid of the source is used.
    pub anchor: Option<LocalFrame>,
    /// Projected CRS of an input document (`EPSG:<code>` or `+proj=`).
    pub crs: Option<String>,
    /// `[min_lon, min_lat, max_lon, max_lat]` filter for OSM sources.
    pub bbox: Option<[f64; 4]>,
}

/// A source read into local metres.
pub struct IndoorSourceInput {
    pub input: Input,
    pub frame: LocalFrame,
    pub source: IndoorSiteSource,
    /// Feature counts of an OSM source.
    pub osm: Option<OsmIndoorStats>,
}

/// Reads an indoor source: `.osm.pbf`/`.pbf` and `.osm` with Simple Indoor
/// Tagging, or an engine input document (`.json`).
pub fn read_indoor_source(path: &Path, options: &IndoorSourceOptions) -> Result<IndoorSourceInput> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let display = path.display().to_string();
    if name.ends_with(".pbf") || name.ends_with(".osm") || name.ends_with(".osm.xml") {
        if options.crs.is_some() {
            bail!("--crs applies to engine input documents, not to OSM data");
        }
        let (input, frame, stats) = read_osm_indoor(path, options.anchor, options.bbox)?;
        return Ok(IndoorSourceInput {
            input,
            frame,
            source: IndoorSiteSource {
                path: display,
                format: "osm".into(),
                crs: None,
                bbox: options.bbox,
            },
            osm: Some(stats),
        });
    }
    if options.bbox.is_some() {
        bail!("--bbox applies to OSM sources only");
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let input = netweevil_indoor::parse_input(&text)
        .with_context(|| format!("parsing indoor input {}", path.display()))?;
    let (input, frame, format, crs) = place_input(input, options)?;
    Ok(IndoorSourceInput {
        input,
        frame,
        source: IndoorSiteSource {
            path: display,
            format,
            crs,
            bbox: None,
        },
        osm: None,
    })
}

fn is_wgs84(crs: &str) -> bool {
    matches!(
        crs.trim().to_ascii_uppercase().as_str(),
        "EPSG:4326" | "WGS84" | "CRS84" | "OGC:CRS84" | "URN:OGC:DEF:CRS:OGC:1.3:CRS84"
    )
}

/// Moves an input document into local metres. Returns the input, its frame,
/// the source format name and the source CRS.
pub fn place_input(
    mut input: Input,
    options: &IndoorSourceOptions,
) -> Result<(Input, LocalFrame, String, Option<String>)> {
    let doc_crs = input
        .crs
        .clone()
        .filter(|c| !c.trim().is_empty() && !c.eq_ignore_ascii_case("local"));
    let crs = options.crs.clone().or(doc_crs);
    match crs {
        Some(c) if is_wgs84(&c) => {
            let frame = match options.anchor {
                Some(a) => a,
                None => {
                    let [lon, lat] =
                        centroid(&mut input).context("the input has no coordinates")?;
                    LocalFrame::new(lon, lat)
                }
            };
            map_input(&mut input, &|x, y| frame.to_local(x, y));
            input.crs = Some("local".into());
            Ok((
                input,
                frame,
                "input_json_wgs84".into(),
                Some("EPSG:4326".into()),
            ))
        }
        Some(c) => {
            let to_wgs84 = ProjectedToWgs84::new(&c)?;
            let frame = match options.anchor {
                Some(a) => a,
                None => {
                    let [x, y] = centroid(&mut input).context("the input has no coordinates")?;
                    let [lon, lat] = to_wgs84.apply(x, y)?;
                    LocalFrame::new(lon, lat)
                }
            };
            // Check every vertex once so a failed transform is an error, not a NaN.
            let mut failed = None;
            for_each_vertex(&mut input, &mut |v| {
                if failed.is_none()
                    && let Err(e) = to_wgs84.apply(v[0], v[1])
                {
                    failed = Some(e);
                }
            });
            if let Some(e) = failed {
                return Err(e);
            }
            map_input(&mut input, &|x, y| {
                let [lon, lat] = to_wgs84.apply(x, y).unwrap_or([f64::NAN, f64::NAN]);
                frame.to_local(lon, lat)
            });
            input.crs = Some("local".into());
            Ok((input, frame, "input_json_crs".into(), Some(c)))
        }
        None => {
            let frame = options.anchor.context(
                "the input is in local metres; give --anchor lon,lat (or --crs for projected coordinates)",
            )?;
            input.crs = Some("local".into());
            Ok((input, frame, "input_json".into(), None))
        }
    }
}

struct ProjectedToWgs84 {
    from: Proj,
    to: Proj,
}

impl ProjectedToWgs84 {
    fn new(spec: &str) -> Result<Self> {
        let definition = match crate::dem::resolve_crs(spec)? {
            RasterCrs::Geographic => bail!("CRS '{spec}' is geographic; use EPSG:4326"),
            RasterCrs::Proj4 { definition, .. } => definition,
        };
        Ok(Self {
            from: Proj::from_proj_string(&definition)
                .with_context(|| format!("invalid CRS '{definition}'"))?,
            to: Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs")
                .context("building the WGS84 CRS")?,
        })
    }

    fn apply(&self, x: f64, y: f64) -> Result<[f64; 2]> {
        let mut p = (x, y, 0.0);
        proj4rs::transform::transform(&self.from, &self.to, &mut p)
            .map_err(|e| anyhow::anyhow!("transforming ({x}, {y}): {e:?}"))?;
        let (lon, lat) = (p.0.to_degrees(), p.1.to_degrees());
        if !lon.is_finite() || !lat.is_finite() {
            bail!("transforming ({x}, {y}) gave no position");
        }
        Ok([lon, lat])
    }
}

/// Centre of the bounding box of every vertex.
fn centroid(input: &mut Input) -> Option<[f64; 2]> {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for_each_vertex(input, &mut |v| {
        b[0] = b[0].min(v[0]);
        b[1] = b[1].min(v[1]);
        b[2] = b[2].max(v[0]);
        b[3] = b[3].max(v[1]);
    });
    b[0].is_finite()
        .then(|| [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5])
}

/// Calls `f` on every vertex (`[x, y(, z)]`, at least two values) of the input.
pub fn for_each_vertex(input: &mut Input, f: &mut dyn FnMut(&mut Vec<f64>)) {
    let mut each = |v: &mut Vec<f64>| {
        if v.len() >= 2 {
            f(v)
        }
    };
    for s in &mut input.surfaces {
        for ring in s.polygon.iter_mut().flatten() {
            ring.iter_mut().for_each(&mut each);
        }
        for poly in s.multipolygon.iter_mut().flatten() {
            for ring in poly {
                ring.iter_mut().for_each(&mut each);
            }
        }
    }
    for e in &mut input.edges {
        e.line.iter_mut().for_each(&mut each);
    }
    for n in &mut input.nodes {
        n.at.iter_mut().for_each(&mut each);
    }
    for p in &mut input.portals {
        p.segment.iter_mut().flatten().for_each(&mut each);
        p.at.iter_mut().for_each(&mut each);
    }
    for b in &mut input.barriers {
        b.line.iter_mut().for_each(&mut each);
    }
    for v in &mut input.verticals {
        for s in &mut v.stops {
            each(&mut s.at);
        }
        v.line.iter_mut().flatten().for_each(&mut each);
    }
    for p in &mut input.pois {
        p.at.iter_mut().for_each(&mut each);
        for a in &mut p.access {
            a.at.iter_mut().for_each(&mut each);
        }
    }
}

/// Maps every horizontal coordinate of the input through `f` (z is kept);
/// portal normals are carried through the same map.
pub fn map_input(input: &mut Input, f: &dyn Fn(f64, f64) -> [f64; 2]) {
    for p in &mut input.portals {
        let base =
            p.at.clone()
                .or_else(|| p.segment.as_ref().and_then(|s| s.first().cloned()));
        if let (Some(n), Some(at)) = (p.normal.as_mut(), base)
            && n.len() >= 2
            && at.len() >= 2
        {
            let d = LocalFrame::transform_direction(f, [at[0], at[1]], [n[0], n[1]]);
            n[0] = d[0];
            n[1] = d[1];
        }
    }
    for_each_vertex(input, &mut |v| {
        let [x, y] = f(v[0], v[1]);
        v[0] = x;
        v[1] = y;
    });
}

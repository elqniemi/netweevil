//! Indoor routes by geographic coordinates: points are `{lon, lat, level}`,
//! `{poi}` or `{space}`; results are in WGS84 with `[lon, lat, z]` geometry.

use anyhow::{Context, Result, bail};
use netweevil_indoor::{IntoAnyhow, Loc, Profile, Route, RouteMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::IndoorSite;

/// Built-in engine profiles.
pub const BUILTIN_PROFILES: [&str; 5] = ["walk", "wheelchair", "fastest", "bike", "evacuation"];

/// A level by id (`"1"`, `"L1"`) or by number (`1`, `-1`, `0.5`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LevelRef {
    Id(String),
    Number(f64),
}

/// One end of an indoor route: a point (`lon`, `lat`, optional `level`), a
/// POI (id, name or ref) or a space (id, or the name or ref of a POI).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndoorPoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<LevelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// A built-in profile name or an inline profile object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IndoorProfileSpec {
    Name(String),
    Inline(Box<Profile>),
}

impl Default for IndoorProfileSpec {
    fn default() -> Self {
        IndoorProfileSpec::Name("walk".into())
    }
}

impl IndoorProfileSpec {
    /// The validated profile with `mode` applied when given.
    pub fn resolve(&self, mode: Option<RouteMode>) -> Result<Profile> {
        let mut p = match self {
            IndoorProfileSpec::Name(name) => Profile::builtin(name).with_context(|| {
                format!(
                    "unknown indoor profile '{name}'; use one of {} or an inline profile object",
                    BUILTIN_PROFILES.join(", ")
                )
            })?,
            IndoorProfileSpec::Inline(p) => {
                p.validate().anyhow()?;
                (**p).clone()
            }
        };
        if let Some(m) = mode {
            p.mode = m;
        }
        Ok(p)
    }

    pub fn label(&self) -> String {
        match self {
            IndoorProfileSpec::Name(n) => n.clone(),
            IndoorProfileSpec::Inline(p) if !p.name.is_empty() => p.name.clone(),
            IndoorProfileSpec::Inline(_) => "inline".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndoorRouteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    pub site_id: String,
    pub origin: IndoorPoint,
    pub destination: IndoorPoint,
    #[serde(default)]
    pub profile: IndoorProfileSpec,
    /// Overrides the profile's mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RouteMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndoorRouteSegment {
    /// `walk`, `edge`, `portal`, `vertical` or `connector`.
    #[serde(rename = "type")]
    pub segment_type: String,
    /// `walk`, `edge`, the portal kind or the vertical kind.
    pub kind: String,
    pub id: String,
    pub level: Option<String>,
    pub space: Option<String>,
    pub class: Option<String>,
    /// Index range into the route geometry.
    pub from_index: usize,
    pub to_index: usize,
    pub length_m: f64,
    pub duration_s: f64,
    pub props: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndoorRouteSummary {
    pub duration_s: f64,
    pub length_m: f64,
    pub climb_m: f64,
    pub descent_m: f64,
    /// Search cost (seconds weighted by the profile's preferences).
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndoorRouteResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    pub site_id: String,
    pub profile: String,
    pub mode: RouteMode,
    pub summary: IndoorRouteSummary,
    /// `[lon, lat, z]`; z is metres above the site's base.
    pub geometry: Vec<[f64; 3]>,
    pub segments: Vec<IndoorRouteSegment>,
    /// Spaces crossed, in order.
    pub spaces: Vec<String>,
    /// POIs the route starts or ends at (POI ends only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_poi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_poi: Option<String>,
    pub warnings: Vec<String>,
}

fn level_number_id(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The level id a reference names in this site.
pub fn resolve_level(site: &IndoorSite, level: &LevelRef) -> Result<String> {
    let levels = &site.manifest.levels;
    let found = match level {
        LevelRef::Id(id) => levels.iter().find(|l| l.id == *id).or_else(|| {
            id.trim()
                .parse::<f64>()
                .ok()
                .and_then(|n| levels.iter().find(|l| l.id == level_number_id(n)))
        }),
        LevelRef::Number(n) => levels
            .iter()
            .find(|l| l.id == level_number_id(*n))
            .or_else(|| {
                levels
                    .iter()
                    .find(|l| l.id.trim_start_matches(['L', 'l']) == level_number_id(*n))
            }),
    };
    match found {
        Some(l) => Ok(l.id.clone()),
        None => bail!(
            "site '{}' has no level {}; levels: {}",
            site.site_id(),
            match level {
                LevelRef::Id(s) => format!("'{s}'"),
                LevelRef::Number(n) => n.to_string(),
            },
            levels
                .iter()
                .map(|l| l.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// POI ids whose id, `props.name` or `props.ref` equals `key` (ignoring case).
fn match_pois(site: &IndoorSite, key: &str) -> Vec<String> {
    let key = key.trim();
    if site.router.graph.poi_ids.contains_key(key) {
        return vec![key.to_string()];
    }
    let eq = |v: Option<&Value>| {
        v.and_then(|v| v.as_str())
            .is_some_and(|s| s.trim().eq_ignore_ascii_case(key))
    };
    site.input
        .pois
        .iter()
        .filter(|p| eq(p.props.get("name")) || eq(p.props.get("ref")))
        .filter_map(|p| p.id.clone())
        .collect()
}

/// The engine locator of a point.
pub fn locate_point(site: &IndoorSite, p: &IndoorPoint) -> Result<Loc> {
    let mut loc = Loc::default();
    if let Some(key) = &p.poi {
        let ids = match_pois(site, key);
        match ids.len() {
            0 => bail!(
                "site '{}' has no poi '{key}' (by id, name or ref)",
                site.site_id()
            ),
            1 => loc.poi = ids.into_iter().next(),
            _ => loc.pois = ids,
        }
        return Ok(loc);
    }
    if let (Some(lon), Some(lat)) = (p.lon, p.lat) {
        let [x, y] = site.frame().to_local(lon, lat);
        loc.at = Some(vec![x, y]);
    } else if p.lon.is_some() != p.lat.is_some() {
        bail!("a point needs both lon and lat");
    }
    if let Some(level) = &p.level {
        loc.level = Some(resolve_level(site, level)?);
    }
    if let Some(space) = &p.space {
        if site.router.graph.space_tri.contains_key(space.as_str()) {
            loc.space = Some(space.clone());
        } else {
            let ids = match_pois(site, space);
            match ids.len() {
                0 => bail!("site '{}' has no space '{space}'", site.site_id()),
                1 => loc.poi = ids.into_iter().next(),
                _ => loc.pois = ids,
            }
            loc.at = None;
        }
        return Ok(loc);
    }
    if loc.at.is_none() {
        bail!("a point needs lon and lat, poi or space");
    }
    Ok(loc)
}

/// `[lon, lat, z]` of a local point.
pub fn to_wgs84(site: &IndoorSite, p: [f64; 3]) -> [f64; 3] {
    let f = site.frame();
    let [lon, lat] = f.to_lonlat(p[0], p[1]);
    [lon, lat, f.elevation_m + p[2]]
}

/// Routes one indoor request.
pub fn route_indoor(site: &IndoorSite, request: &IndoorRouteRequest) -> Result<IndoorRouteResult> {
    let profile = request.profile.resolve(request.mode)?;
    let from = locate_point(site, &request.origin).context("origin")?;
    let to = locate_point(site, &request.destination).context("destination")?;
    let route = route_locs(site, &profile, &from, &to)?;
    Ok(route_result(
        site,
        request.route_id.clone(),
        request.profile.label(),
        profile.mode,
        &route,
    ))
}

/// The engine route between two locators.
pub fn route_locs(site: &IndoorSite, profile: &Profile, from: &Loc, to: &Loc) -> Result<Route> {
    let cp = site.router.profile(profile);
    site.with_ctx(|ctx| site.router.route(ctx, &cp, from, to))
        .map_err(anyhow::Error::msg)
}

/// An engine route in WGS84.
pub fn route_result(
    site: &IndoorSite,
    route_id: Option<String>,
    profile: String,
    mode: RouteMode,
    route: &Route,
) -> IndoorRouteResult {
    let geometry = route.coords.iter().map(|p| to_wgs84(site, *p)).collect();
    let segments = route
        .segments
        .iter()
        .map(|s| IndoorRouteSegment {
            segment_type: s.typ.to_string(),
            kind: s.kind.clone(),
            id: s.id.clone(),
            level: s.level.clone(),
            space: s.space.clone(),
            class: s.class.clone(),
            from_index: s.from_idx,
            to_index: s.to_idx,
            length_m: s.length,
            duration_s: s.duration,
            props: s
                .props
                .as_ref()
                .and_then(|r| serde_json::from_str(r.get()).ok())
                .unwrap_or(Value::Null),
        })
        .collect();
    IndoorRouteResult {
        route_id,
        site_id: site.site_id().to_string(),
        profile,
        mode,
        summary: IndoorRouteSummary {
            duration_s: route.duration,
            length_m: route.length,
            climb_m: route.climb,
            descent_m: route.descent,
            cost: route.cost,
        },
        geometry,
        segments,
        spaces: route.spaces.clone(),
        origin_poi: route.from_poi.as_ref().map(|p| p.id.clone()),
        destination_poi: route.to_poi.as_ref().map(|p| p.id.clone()),
        warnings: route.warnings.clone(),
    }
}

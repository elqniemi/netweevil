//! Door-to-door routes: an indoor leg out of the origin site, a street leg,
//! and an indoor leg into the destination site.
//!
//! Each end is a street point or an indoor point. For an indoor origin the
//! indoor costs from the point to every entrance of its site are computed in
//! one search; for an indoor destination, from every entrance to the point.
//! The [`MAX_ENTRANCE_CANDIDATES`] fastest entrances of each side are
//! snapped to the street graph at their outdoor side; an entrance that does
//! not snap, or that the indoor profile cannot reach or pass, is dropped.
//! One many-to-many street query (or one route when each side has one
//! candidate) gives the street times between the candidates, and the pair
//! with the least `indoor out + street + indoor in` travel time wins. The
//! choice is exact over these candidates. Only the chosen pair is routed
//! with geometry. The street leg keeps its own generalized cost.
//!
//! When both ends are in the same site, the pure indoor route is returned
//! when it is no slower than leaving and entering again.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use netweevil_indoor::{Loc, Profile, RouteMode};
use netweevil_persist::indoor::{IndoorEntranceKind, IndoorSiteEntrance};
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use netweevil_query::{
    LabeledPoint, PointSetDocument, PreparedRoutingEngine, RouteRequest, RouteResult, SnapOptions,
    SnappedPoint,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::geojson::indoor_route_features;
use crate::route::{
    IndoorPoint, IndoorProfileSpec, IndoorRouteResult, locate_point, route_locs, route_result,
};
use crate::runtime::{IndoorSite, SiteCatalog};

/// Entrances kept per side, the fastest by indoor travel time.
pub const MAX_ENTRANCE_CANDIDATES: usize = 8;
/// Default distance within which an entrance's outdoor side snaps to a street (m).
pub const ENTRANCE_SNAP_M: f64 = 50.0;

/// One end: a street point (`lon`, `lat`), or an indoor point with an
/// optional `site_id` (`{site_id, lon, lat, level}`, `{site_id, poi}`,
/// `{site_id, space}`). A point with a `level` and no `site_id` is in the
/// site whose footprint contains it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoorToDoorPoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_id: Option<String>,
    #[serde(flatten)]
    pub point: IndoorPoint,
}

impl DoorToDoorPoint {
    /// Whether this end is indoors.
    pub fn is_indoor(&self) -> bool {
        self.site_id.is_some()
            || self.point.level.is_some()
            || self.point.poi.is_some()
            || self.point.space.is_some()
    }
}

/// Indoor routing settings of a door-to-door request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoorToDoorSettings {
    #[serde(default)]
    pub profile: IndoorProfileSpec,
    /// Overrides the indoor profile's mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RouteMode>,
    /// Snap distance for the outdoor side of entrances (m).
    #[serde(default = "default_entrance_snap")]
    pub entrance_snap_m: f64,
    /// Entrances kept per side (at most [`MAX_ENTRANCE_CANDIDATES`]).
    #[serde(default = "default_candidates")]
    pub max_entrances: usize,
}

fn default_entrance_snap() -> f64 {
    ENTRANCE_SNAP_M
}

fn default_candidates() -> usize {
    MAX_ENTRANCE_CANDIDATES
}

impl Default for DoorToDoorSettings {
    fn default() -> Self {
        Self {
            profile: IndoorProfileSpec::default(),
            mode: None,
            entrance_snap_m: ENTRANCE_SNAP_M,
            max_entrances: MAX_ENTRANCE_CANDIDATES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoorToDoorRequest {
    pub route_id: String,
    pub origin: DoorToDoorPoint,
    pub destination: DoorToDoorPoint,
    #[serde(default)]
    pub indoor: DoorToDoorSettings,
    /// Snap options of street ends.
    #[serde(default)]
    pub snap: SnapOptions,
}

/// The entrance a route uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChosenEntrance {
    pub site_id: String,
    #[serde(flatten)]
    pub entrance: IndoorSiteEntrance,
    /// Indoor travel time between the end and the entrance (s).
    pub indoor_duration_s: f64,
    /// Snap distance of the outdoor side to the street (m).
    pub snap_distance_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoorToDoorLegType {
    Indoor,
    Street,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoorToDoorLeg {
    pub leg_type: DoorToDoorLegType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_id: Option<String>,
    pub duration_s: f64,
    pub length_m: f64,
    /// `[lon, lat, z]`.
    pub geometry: Vec<[f64; 3]>,
    /// Indoor legs: the indoor route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indoor: Option<IndoorRouteResult>,
    /// Street legs: the street route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<RouteResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoorToDoorSummary {
    pub duration_s: f64,
    pub length_m: f64,
    pub indoor_duration_s: f64,
    pub street_duration_s: f64,
    /// Generalized cost of the street leg, as the street engine reports it.
    pub street_generalized_cost: f64,
    /// Entrance candidates compared on each side (0 for a street end).
    pub origin_candidates: usize,
    pub destination_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoorToDoorResult {
    pub route_id: String,
    pub legs: Vec<DoorToDoorLeg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_entrance: Option<ChosenEntrance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_entrance: Option<ChosenEntrance>,
    pub summary: DoorToDoorSummary,
    pub warnings: Vec<String>,
}

/// A resolved end.
enum End {
    Street(LabeledPoint),
    Indoor { site: Arc<IndoorSite>, loc: Loc },
}

/// An entrance of one side with its indoor time and street snap.
struct Candidate {
    entrance: IndoorSiteEntrance,
    loc: Loc,
    indoor_s: f64,
    snapped: Vec<SnappedPoint>,
    point: LabeledPoint,
}

fn resolve_end(catalog: &SiteCatalog, p: &DoorToDoorPoint, side: &str) -> Result<End> {
    if !p.is_indoor() {
        let (Some(lon), Some(lat)) = (p.point.lon, p.point.lat) else {
            bail!("{side}: a street point needs lon and lat");
        };
        return Ok(End::Street(LabeledPoint {
            id: p.id.clone().unwrap_or_else(|| side.to_string()),
            lon,
            lat,
            z: None,
        }));
    }
    let site = match (&p.site_id, p.point.lon, p.point.lat) {
        (Some(id), _, _) => catalog.get(id)?,
        (None, Some(lon), Some(lat)) => catalog.site_at(lon, lat).context(side.to_string())?,
        _ => bail!("{side}: an indoor point with poi or space needs site_id"),
    };
    let loc = locate_point(&site, &p.point).with_context(|| side.to_string())?;
    Ok(End::Indoor { site, loc })
}

fn entrance_loc(e: &IndoorSiteEntrance) -> Loc {
    let mut l = Loc::default();
    match e.kind {
        IndoorEntranceKind::Portal => l.portal = Some(e.id.clone()),
        IndoorEntranceKind::Node => l.node = Some(e.id.clone()),
        IndoorEntranceKind::Poi => l.poi = Some(e.id.clone()),
    }
    l
}

/// Whether the profile may pass the entrance itself (step-free and width).
fn passable(site: &IndoorSite, e: &IndoorSiteEntrance, profile: &Profile) -> bool {
    if profile.step_free && !e.step_free {
        return false;
    }
    if let Some(min) = profile.min_width
        && e.kind == IndoorEntranceKind::Portal
        && let Some(w) = site
            .input
            .portals
            .iter()
            .find(|p| p.id.as_deref() == Some(e.id.as_str()))
            .and_then(|p| p.width)
    {
        return w >= min;
    }
    true
}

/// The entrances of one indoor end, fastest first, snapped to the street.
fn candidates(
    engine: &PreparedRoutingEngine,
    site: &IndoorSite,
    loc: &Loc,
    profile: &Profile,
    settings: &DoorToDoorSettings,
    outbound: bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<Candidate>> {
    let entrances: Vec<&IndoorSiteEntrance> = site
        .manifest
        .entrances
        .iter()
        .filter(|e| passable(site, e, profile))
        .collect();
    if entrances.is_empty() {
        bail!(
            "site '{}' has no entrance that the indoor profile can use",
            site.site_id()
        );
    }
    let locs: Vec<Loc> = entrances.iter().map(|e| entrance_loc(e)).collect();
    let cp = site.router.profile(profile);
    let costs = site
        .with_ctx(|ctx| {
            if outbound {
                site.router.costs_to(ctx, &cp, loc, &locs)
            } else {
                site.router.costs_from(ctx, &cp, &locs, loc)
            }
        })
        .map_err(anyhow::Error::msg)?;
    let mut reachable: Vec<(usize, f64)> = costs
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.map(|c| (i, c.duration)))
        .collect();
    if reachable.is_empty() {
        bail!(
            "no entrance of site '{}' is reachable from the {} point",
            site.site_id(),
            if outbound { "origin" } else { "destination" }
        );
    }
    reachable.sort_by(|a, b| a.1.total_cmp(&b.1));
    reachable.truncate(settings.max_entrances.clamp(1, MAX_ENTRANCE_CANDIDATES));
    let snap = SnapOptions {
        max_distance_m: settings.entrance_snap_m,
        ..SnapOptions::default()
    };
    let mut out = vec![];
    for (i, indoor_s) in reachable {
        let e = entrances[i];
        let point = LabeledPoint {
            id: format!("{}:{}", site.site_id(), e.id),
            lon: e.outdoor_lon,
            lat: e.outdoor_lat,
            z: None,
        };
        let snapped = engine
            .snap_route_candidates_with_options(&point, &snap, outbound)
            .unwrap_or_default();
        if snapped.is_empty() {
            warnings.push(format!(
                "entrance '{}' of site '{}' is more than {} m from a street the profile can use",
                e.id,
                site.site_id(),
                settings.entrance_snap_m
            ));
            continue;
        }
        out.push(Candidate {
            entrance: e.clone(),
            loc: locs[i].clone(),
            indoor_s,
            snapped,
            point,
        });
    }
    if out.is_empty() {
        bail!(
            "no reachable entrance of site '{}' is within {} m of a street",
            site.site_id(),
            settings.entrance_snap_m
        );
    }
    Ok(out)
}

fn full_geometry() -> ReturnConfig {
    ReturnConfig {
        geometry: ReturnGeometry::Full,
        ..ReturnConfig::default()
    }
}

/// Street travel times `[origin][destination]` between candidate points.
fn street_times(
    engine: &PreparedRoutingEngine,
    origins: &[(LabeledPoint, SnapOptions)],
    destinations: &[(LabeledPoint, SnapOptions)],
) -> Result<Vec<Vec<Option<f64>>>> {
    let doc = |pts: &[(LabeledPoint, SnapOptions)]| PointSetDocument {
        points: pts.iter().map(|p| p.0.clone()).collect(),
        snap: pts[0].1.clone(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::None,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let result = engine.execute_matrix(&doc(origins), &doc(destinations))?;
    let index = |pts: &[(LabeledPoint, SnapOptions)]| -> HashMap<String, usize> {
        pts.iter()
            .enumerate()
            .map(|(i, p)| (p.0.id.clone(), i))
            .collect()
    };
    let (oi, di) = (index(origins), index(destinations));
    let mut out = vec![vec![None; destinations.len()]; origins.len()];
    for cell in &result.cells {
        if let (Some(&o), Some(&d)) = (oi.get(&cell.origin_id), di.get(&cell.destination_id)) {
            out[o][d] = cell.total_travel_time_s;
        }
    }
    Ok(out)
}

fn indoor_leg(site: &IndoorSite, result: IndoorRouteResult) -> DoorToDoorLeg {
    DoorToDoorLeg {
        leg_type: DoorToDoorLegType::Indoor,
        site_id: Some(site.site_id().to_string()),
        duration_s: result.summary.duration_s,
        length_m: result.summary.length_m,
        geometry: result.geometry.clone(),
        indoor: Some(result),
        street: None,
    }
}

/// Routes one door-to-door request over the street engine and the sites of
/// `catalog`.
pub fn door_to_door_route(
    engine: &PreparedRoutingEngine,
    catalog: &SiteCatalog,
    request: &DoorToDoorRequest,
) -> Result<DoorToDoorResult> {
    let origin = resolve_end(catalog, &request.origin, "origin")?;
    let destination = resolve_end(catalog, &request.destination, "destination")?;
    if !request.origin.is_indoor() && !request.destination.is_indoor() {
        bail!("door-to-door: neither end is indoors; use the route endpoint");
    }
    let settings = &request.indoor;
    let profile = settings.profile.resolve(settings.mode)?;
    let label = settings.profile.label();
    let mut warnings = vec![];

    let side = |end: &End, outbound: bool, warnings: &mut Vec<String>| -> Result<Vec<Candidate>> {
        match end {
            End::Street(_) => Ok(vec![]),
            End::Indoor { site, loc } => {
                candidates(engine, site, loc, &profile, settings, outbound, warnings)
            }
        }
    };
    // A same-site trip may stay indoors.
    let same_site = match (&origin, &destination) {
        (End::Indoor { site: a, loc: la }, End::Indoor { site: b, loc: lb })
            if a.site_id() == b.site_id() =>
        {
            Some((a.clone(), la.clone(), lb.clone()))
        }
        _ => None,
    };
    let pure_indoor = same_site
        .as_ref()
        .and_then(|(site, a, b)| route_locs(site, &profile, a, b).ok().map(|r| (site, r)));

    let out_c = side(&origin, true, &mut warnings);
    let in_c = side(&destination, false, &mut warnings);
    let (out_c, in_c) = match (out_c, in_c, &pure_indoor) {
        (Ok(o), Ok(i), _) => (o, i),
        (Err(e), _, None) | (_, Err(e), None) => return Err(e),
        (_, _, Some((site, route))) => {
            let r = route_result(site, None, label, profile.mode, route);
            return Ok(single_indoor(request, site, r, warnings));
        }
    };

    let street_point = |end: &End| match end {
        End::Street(p) => Some(p.clone()),
        End::Indoor { .. } => None,
    };
    let entrance_snap = SnapOptions {
        max_distance_m: settings.entrance_snap_m,
        ..SnapOptions::default()
    };
    // (point, snap options, indoor seconds, candidate index)
    let points =
        |c: &[Candidate], end: &End| -> Vec<(LabeledPoint, SnapOptions, f64, Option<usize>)> {
            match street_point(end) {
                Some(p) => vec![(p, request.snap.clone(), 0.0, None)],
                None => c
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.point.clone(), entrance_snap.clone(), c.indoor_s, Some(i)))
                    .collect(),
            }
        };
    let op = points(&out_c, &origin);
    let dp = points(&in_c, &destination);

    // Street times between the sides: one route for one pair, else one matrix.
    let times: Vec<Vec<Option<f64>>> = if op.len() == 1 && dp.len() == 1 {
        vec![vec![None]]
    } else {
        let o: Vec<_> = op.iter().map(|p| (p.0.clone(), p.1.clone())).collect();
        let d: Vec<_> = dp.iter().map(|p| (p.0.clone(), p.1.clone())).collect();
        street_times(engine, &o, &d)?
    };
    let snapped_of =
        |c: &[Candidate], end: &End, k: usize, is_origin: bool| -> Result<Vec<SnappedPoint>> {
            match street_point(end) {
                Some(p) => engine.snap_route_candidates_with_options(&p, &request.snap, is_origin),
                None => Ok(c[k].snapped.clone()),
            }
        };
    let street_request = |a: &LabeledPoint, b: &LabeledPoint| RouteRequest {
        route_id: request.route_id.clone(),
        origin: a.clone(),
        destination: b.clone(),
        snap: request.snap.clone(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: full_geometry(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    // The best pair by travel time.
    let mut best: Option<(f64, usize, usize, Option<RouteResult>)> = None;
    if op.len() == 1 && dp.len() == 1 {
        let a = snapped_of(&out_c, &origin, 0, true)?;
        let b = snapped_of(&in_c, &destination, 0, false)?;
        if a.is_empty() || b.is_empty() {
            bail!(
                "the street end is more than {} m from a street",
                request.snap.max_distance_m
            );
        }
        let route =
            engine.execute_route_between_candidates(&street_request(&op[0].0, &dp[0].0), &a, &b)?;
        let t = op[0].2 + route.summary.total_travel_time_s + dp[0].2;
        best = Some((t, 0, 0, Some(route)));
    } else {
        for (i, o) in op.iter().enumerate() {
            for (j, d) in dp.iter().enumerate() {
                let Some(s) = times[i][j] else { continue };
                let t = o.2 + s + d.2;
                if best.as_ref().is_none_or(|b| t < b.0) {
                    best = Some((t, i, j, None));
                }
            }
        }
    }
    let Some((total, i, j, route)) = best else {
        if let Some((site, r)) = &pure_indoor {
            let r = route_result(site, None, label, profile.mode, r);
            return Ok(single_indoor(request, site, r, warnings));
        }
        bail!("no street route joins the entrances of the origin and the destination");
    };
    if let Some((site, r)) = &pure_indoor
        && r.duration <= total
    {
        let r = route_result(site, None, label, profile.mode, r);
        return Ok(single_indoor(request, site, r, warnings));
    }

    let street = match route {
        Some(r) => r,
        None => {
            let a = snapped_of(&out_c, &origin, op[i].3.unwrap_or(0), true)?;
            let b = snapped_of(&in_c, &destination, dp[j].3.unwrap_or(0), false)?;
            engine
                .execute_route_between_candidates(&street_request(&op[i].0, &dp[j].0), &a, &b)
                .context("street leg")?
        }
    };

    let mut legs = vec![];
    let mut origin_entrance = None;
    let mut destination_entrance = None;
    if let (End::Indoor { site, loc }, Some(k)) = (&origin, op[i].3) {
        let c = &out_c[k];
        let r = route_locs(site, &profile, loc, &c.loc).context("indoor leg from the origin")?;
        legs.push(indoor_leg(
            site,
            route_result(site, None, label.clone(), profile.mode, &r),
        ));
        origin_entrance = Some(chosen(site, c));
    }
    legs.push(DoorToDoorLeg {
        leg_type: DoorToDoorLegType::Street,
        site_id: None,
        duration_s: street.summary.total_travel_time_s,
        length_m: street.summary.total_distance_m as f64,
        geometry: street.geometry.clone().unwrap_or_default(),
        indoor: None,
        street: Some(street),
    });
    if let (End::Indoor { site, loc }, Some(k)) = (&destination, dp[j].3) {
        let c = &in_c[k];
        let r = route_locs(site, &profile, &c.loc, loc).context("indoor leg to the destination")?;
        legs.push(indoor_leg(
            site,
            route_result(site, None, label.clone(), profile.mode, &r),
        ));
        destination_entrance = Some(chosen(site, c));
    }
    let mut summary = summarize(&legs);
    summary.origin_candidates = out_c.len();
    summary.destination_candidates = in_c.len();
    Ok(DoorToDoorResult {
        route_id: request.route_id.clone(),
        legs,
        origin_entrance,
        destination_entrance,
        summary,
        warnings,
    })
}

fn chosen(site: &IndoorSite, c: &Candidate) -> ChosenEntrance {
    ChosenEntrance {
        site_id: site.site_id().to_string(),
        entrance: c.entrance.clone(),
        indoor_duration_s: c.indoor_s,
        snap_distance_m: c.snapped.first().map_or(0.0, |s| s.snap_distance_m),
    }
}

fn summarize(legs: &[DoorToDoorLeg]) -> DoorToDoorSummary {
    let mut s = DoorToDoorSummary::default();
    for l in legs {
        s.duration_s += l.duration_s;
        s.length_m += l.length_m;
        match l.leg_type {
            DoorToDoorLegType::Indoor => s.indoor_duration_s += l.duration_s,
            DoorToDoorLegType::Street => {
                s.street_duration_s += l.duration_s;
                s.street_generalized_cost += l
                    .street
                    .as_ref()
                    .map_or(0.0, |r| r.summary.total_generalized_cost);
            }
        }
    }
    s
}

fn single_indoor(
    request: &DoorToDoorRequest,
    site: &IndoorSite,
    result: IndoorRouteResult,
    warnings: Vec<String>,
) -> DoorToDoorResult {
    let legs = vec![indoor_leg(site, result)];
    DoorToDoorResult {
        route_id: request.route_id.clone(),
        summary: summarize(&legs),
        legs,
        origin_entrance: None,
        destination_entrance: None,
        warnings,
    }
}

/// One feature per leg with `leg_type`, `leg_index`, `duration_s`,
/// `length_m` and `site_id`.
pub fn door_to_door_geojson(result: &DoorToDoorResult) -> Value {
    let features: Vec<Value> = result
        .legs
        .iter()
        .enumerate()
        .map(|(k, l)| {
            json!({
                "type": "Feature",
                "geometry": { "type": "LineString", "coordinates": l.geometry },
                "properties": {
                    "leg_index": k,
                    "leg_type": l.leg_type,
                    "site_id": l.site_id,
                    "duration_s": l.duration_s,
                    "length_m": l.length_m,
                },
            })
        })
        .collect();
    json!({
        "type": "FeatureCollection",
        "features": features,
        "properties": {
            "route_id": result.route_id,
            "summary": result.summary,
            "origin_entrance": result.origin_entrance,
            "destination_entrance": result.destination_entrance,
            "warnings": result.warnings,
        },
    })
}

/// The indoor segments of every indoor leg as features (for detailed maps).
pub fn door_to_door_segment_features(result: &DoorToDoorResult) -> Vec<Value> {
    result
        .legs
        .iter()
        .enumerate()
        .filter_map(|(k, l)| {
            l.indoor
                .as_ref()
                .map(|r| indoor_route_features(r, &json!({ "leg_index": k, "leg_type": "indoor" })))
        })
        .flatten()
        .collect()
}

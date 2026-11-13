//! Transit itineraries that start or end inside an indoor site.
//!
//! [`IndoorStreetEstimator`] wraps the host's street estimator. For an
//! indoor origin it prices the way from the point to a stop as the least,
//! over the site's entrances, of the indoor route to the entrance plus the
//! street path from the entrance's outdoor side to the stop; an indoor
//! destination likewise from the stop to the point. The returned path
//! carries the indoor part ([`TransitIndoorPart`]), which the transit router
//! shows as an `indoor` leg before the access leg or after the egress leg.
//! Transfers between stops and isochrones use the wrapped estimator
//! unchanged.

use std::sync::Arc;

use anyhow::{Context, Result};
use netweevil_core::geo::haversine_meters;
use netweevil_persist::indoor::IndoorSiteEntrance;
use netweevil_transit::{
    AccessMode, StreetTimeEstimator, TransitIndoorPart, TransitIsochroneFeature, TransitPoint,
    TransitServiceAreaRequest, TransitStop, TransitStreetPath,
};

use crate::door_to_door::{DoorToDoorPoint, DoorToDoorSettings, MAX_ENTRANCE_CANDIDATES};
use crate::route::{
    IndoorPoint, IndoorRouteResult, LevelRef, locate_point, route_locs, route_result,
};
use crate::runtime::{IndoorSite, SiteCatalog};

/// Walking speed on the gap between an entrance and the street it snaps to (m/s).
const GAP_SPEED_MPS: f64 = 1.33;

/// One entrance of an indoor end with the indoor route through it.
pub struct EntranceRoute {
    pub entrance: IndoorSiteEntrance,
    /// From the point to the entrance (origin) or from the entrance to the
    /// point (destination).
    pub route: IndoorRouteResult,
}

/// An indoor end of a transit request: its site and its entrance routes.
pub struct IndoorTransitEnd {
    pub site: Arc<IndoorSite>,
    pub entrances: Vec<EntranceRoute>,
    /// WGS84 position of the indoor point.
    pub lon: f64,
    pub lat: f64,
}

/// Whether a transit point names an indoor end.
pub fn is_indoor_point(p: &TransitPoint) -> bool {
    p.site_id.is_some() || p.level.is_some() || p.poi.is_some() || p.space.is_some()
}

fn level_ref(v: &serde_json::Value) -> Result<LevelRef> {
    match v {
        serde_json::Value::String(s) => Ok(LevelRef::Id(s.clone())),
        serde_json::Value::Number(n) => Ok(LevelRef::Number(
            n.as_f64().context("level is not a number")?,
        )),
        other => anyhow::bail!("level must be a string or a number, not {other}"),
    }
}

/// Resolves an indoor end of a transit request and routes from the point to
/// its best entrances (or from them to the point). Returns None for a
/// street point. `outbound` is true for the origin.
pub fn prepare_indoor_end(
    catalog: &SiteCatalog,
    point: &TransitPoint,
    settings: &DoorToDoorSettings,
    outbound: bool,
) -> Result<Option<IndoorTransitEnd>> {
    if !is_indoor_point(point) {
        return Ok(None);
    }
    let finite = point.lon.is_finite() && point.lat.is_finite();
    let p = DoorToDoorPoint {
        id: Some(point.id.clone()),
        site_id: point.site_id.clone(),
        point: IndoorPoint {
            lon: finite.then_some(point.lon),
            lat: finite.then_some(point.lat),
            level: point.level.as_ref().map(level_ref).transpose()?,
            poi: point.poi.clone(),
            space: point.space.clone(),
        },
    };
    let side = if outbound { "origin" } else { "destination" };
    let site = match (&p.site_id, p.point.lon, p.point.lat) {
        (Some(id), _, _) => catalog.get(id)?,
        (None, Some(lon), Some(lat)) => catalog.site_at(lon, lat).context(side)?,
        _ => anyhow::bail!("{side}: an indoor point with poi or space needs site_id"),
    };
    let loc = locate_point(&site, &p.point).context(side)?;
    let profile = settings.profile.resolve(settings.mode)?;
    let label = settings.profile.label();
    let cp = site.router.profile(&profile);
    let entrances: Vec<&IndoorSiteEntrance> = site
        .manifest
        .entrances
        .iter()
        .filter(|e| crate::door_to_door::passable(&site, e, &profile))
        .collect();
    let locs: Vec<_> = entrances
        .iter()
        .map(|e| crate::door_to_door::entrance_loc(e))
        .collect();
    let costs = site
        .with_ctx(|ctx| {
            if outbound {
                site.router.costs_to(ctx, &cp, &loc, &locs)
            } else {
                site.router.costs_from(ctx, &cp, &locs, &loc)
            }
        })
        .map_err(anyhow::Error::msg)
        .context(side)?;
    let mut order: Vec<(usize, f64)> = costs
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.map(|c| (i, c.duration)))
        .collect();
    order.sort_by(|a, b| a.1.total_cmp(&b.1));
    order.truncate(settings.max_entrances.clamp(1, MAX_ENTRANCE_CANDIDATES));
    let mut out = vec![];
    for (i, _) in order {
        let r = if outbound {
            route_locs(&site, &profile, &loc, &locs[i])
        } else {
            route_locs(&site, &profile, &locs[i], &loc)
        };
        if let Ok(r) = r {
            out.push(EntranceRoute {
                entrance: entrances[i].clone(),
                route: route_result(&site, None, label.clone(), profile.mode, &r),
            });
        }
    }
    if out.is_empty() {
        anyhow::bail!(
            "{side}: no entrance of site '{}' is reachable with the indoor profile",
            site.site_id()
        );
    }
    let at = if outbound {
        out[0].route.geometry.first()
    } else {
        out[0].route.geometry.last()
    };
    let [lon, lat] = match at {
        Some(p) => [p[0], p[1]],
        None => [out[0].entrance.lon, out[0].entrance.lat],
    };
    Ok(Some(IndoorTransitEnd {
        site,
        entrances: out,
        lon,
        lat,
    }))
}

/// A street estimator that adds indoor legs at indoor request ends.
pub struct IndoorStreetEstimator<'a> {
    inner: &'a dyn StreetTimeEstimator,
    origin: Option<IndoorTransitEnd>,
    destination: Option<IndoorTransitEnd>,
}

impl<'a> IndoorStreetEstimator<'a> {
    pub fn new(
        inner: &'a dyn StreetTimeEstimator,
        origin: Option<IndoorTransitEnd>,
        destination: Option<IndoorTransitEnd>,
    ) -> Self {
        Self {
            inner,
            origin,
            destination,
        }
    }

    /// The best entrance path for one stop: `street(entrance)` gives the
    /// street path from (or to) the entrance's outdoor side.
    fn best(
        end: &IndoorTransitEnd,
        access: bool,
        street: impl Fn(&TransitPoint) -> Option<TransitStreetPath>,
    ) -> Option<TransitStreetPath> {
        let mut best: Option<TransitStreetPath> = None;
        for e in &end.entrances {
            let outdoor = TransitPoint {
                id: e.entrance.id.clone(),
                lon: e.entrance.outdoor_lon,
                lat: e.entrance.outdoor_lat,
                z: None,
                site_id: None,
                level: None,
                poi: None,
                space: None,
            };
            let Some(path) = street(&outdoor) else {
                continue;
            };
            let combined = combine(end, e, path, access);
            if best
                .as_ref()
                .is_none_or(|b| combined.travel_time_s < b.travel_time_s)
            {
                best = Some(combined);
            }
        }
        best
    }
}

/// Joins an indoor route and a street path into one access (indoor first)
/// or egress (indoor last) path.
fn combine(
    end: &IndoorTransitEnd,
    e: &EntranceRoute,
    mut street: TransitStreetPath,
    access: bool,
) -> TransitStreetPath {
    let indoor = &e.route;
    // The walk between the entrance and the point where the street path starts.
    let joint = if access {
        indoor.geometry.last().zip(street.geometry.first())
    } else {
        street.geometry.last().zip(indoor.geometry.first())
    };
    let gap_m = joint.map_or(0.0, |(a, b)| haversine_meters(a[0], a[1], b[0], b[1]));
    let gap_s = (gap_m / GAP_SPEED_MPS).round() as u32;
    let indoor_s = indoor.summary.duration_s.round() as u32;
    let part = TransitIndoorPart {
        site_id: end.site.site_id().to_string(),
        entrance_id: e.entrance.id.clone(),
        entrance_name: e
            .entrance
            .name
            .clone()
            .unwrap_or_else(|| e.entrance.id.clone()),
        travel_time_s: indoor_s,
        distance_m: indoor.summary.length_m,
        geometry: indoor.geometry.clone(),
        detail: serde_json::to_value(indoor).unwrap_or_default(),
    };
    street.travel_time_s = street.travel_time_s.saturating_add(indoor_s + gap_s);
    street.distance_m = street
        .distance_m
        .map(|d| d + gap_m + indoor.summary.length_m);
    let mut geometry = Vec::with_capacity(indoor.geometry.len() + street.geometry.len());
    if access {
        geometry.extend_from_slice(&indoor.geometry);
        geometry.append(&mut street.geometry);
    } else {
        geometry.append(&mut street.geometry);
        geometry.extend_from_slice(&indoor.geometry);
    }
    street.geometry = geometry;
    if gap_m > 0.01 {
        street
            .components
            .insert("entrance_connection_distance_m".into(), gap_m);
    }
    street.indoor = Some(Box::new(part));
    street
}

impl StreetTimeEstimator for IndoorStreetEstimator<'_> {
    fn street_isochrone(
        &self,
        origin: &TransitPoint,
        stops: &[(&TransitStop, u32)],
        request: &TransitServiceAreaRequest,
    ) -> Result<Vec<TransitIsochroneFeature>> {
        self.inner.street_isochrone(origin, stops, request)
    }

    fn street_time_s(
        &self,
        mode: AccessMode,
        egress: bool,
        from_lon: f64,
        from_lat: f64,
        to_lon: f64,
        to_lat: f64,
    ) -> Option<u32> {
        self.inner
            .street_time_s(mode, egress, from_lon, from_lat, to_lon, to_lat)
    }

    fn point_to_stop_path(
        &self,
        mode: AccessMode,
        from_lon: f64,
        from_lat: f64,
        stop: &TransitStop,
    ) -> Option<TransitStreetPath> {
        match &self.origin {
            Some(end) => Self::best(end, true, |p| {
                self.inner.point_to_stop_path(mode, p.lon, p.lat, stop)
            }),
            None => self
                .inner
                .point_to_stop_path(mode, from_lon, from_lat, stop),
        }
    }

    fn stop_to_point_path(
        &self,
        mode: AccessMode,
        stop: &TransitStop,
        to_lon: f64,
        to_lat: f64,
    ) -> Option<TransitStreetPath> {
        match &self.destination {
            Some(end) => Self::best(end, false, |p| {
                self.inner.stop_to_point_path(mode, stop, p.lon, p.lat)
            }),
            None => self.inner.stop_to_point_path(mode, stop, to_lon, to_lat),
        }
    }

    fn point_to_stop_path_3d(
        &self,
        mode: AccessMode,
        from: &TransitPoint,
        stop: &TransitStop,
    ) -> Option<TransitStreetPath> {
        match &self.origin {
            Some(end) => Self::best(end, true, |p| {
                self.inner.point_to_stop_path_3d(mode, p, stop)
            }),
            None => self.inner.point_to_stop_path_3d(mode, from, stop),
        }
    }

    fn stop_to_point_path_3d(
        &self,
        mode: AccessMode,
        stop: &TransitStop,
        to: &TransitPoint,
    ) -> Option<TransitStreetPath> {
        match &self.destination {
            Some(end) => Self::best(end, false, |p| {
                self.inner.stop_to_point_path_3d(mode, stop, p)
            }),
            None => self.inner.stop_to_point_path_3d(mode, stop, to),
        }
    }

    fn point_to_stop_path_with_snap_limit(
        &self,
        mode: AccessMode,
        from: &TransitPoint,
        stop: &TransitStop,
        max_endpoint_snap_distance_m: f64,
    ) -> Option<TransitStreetPath> {
        match &self.origin {
            Some(end) => Self::best(end, true, |p| {
                self.inner.point_to_stop_path_with_snap_limit(
                    mode,
                    p,
                    stop,
                    max_endpoint_snap_distance_m,
                )
            }),
            None => self.inner.point_to_stop_path_with_snap_limit(
                mode,
                from,
                stop,
                max_endpoint_snap_distance_m,
            ),
        }
    }

    fn stop_to_point_path_with_snap_limit(
        &self,
        mode: AccessMode,
        stop: &TransitStop,
        to: &TransitPoint,
        max_endpoint_snap_distance_m: f64,
    ) -> Option<TransitStreetPath> {
        match &self.destination {
            Some(end) => Self::best(end, false, |p| {
                self.inner.stop_to_point_path_with_snap_limit(
                    mode,
                    stop,
                    p,
                    max_endpoint_snap_distance_m,
                )
            }),
            None => self.inner.stop_to_point_path_with_snap_limit(
                mode,
                stop,
                to,
                max_endpoint_snap_distance_m,
            ),
        }
    }

    fn stop_to_stop_path(
        &self,
        mode: AccessMode,
        from: &TransitStop,
        to: &TransitStop,
    ) -> Option<TransitStreetPath> {
        self.inner.stop_to_stop_path(mode, from, to)
    }
}

/// Resolves the indoor ends of a transit request and moves their `lon`,
/// `lat` to the indoor points, so the router's stop search and attachment
/// checks start there. Returns the origin and destination ends.
pub fn prepare_indoor_transit_request(
    catalog: &SiteCatalog,
    origin: &mut TransitPoint,
    destination: &mut TransitPoint,
    settings: &DoorToDoorSettings,
) -> Result<(Option<IndoorTransitEnd>, Option<IndoorTransitEnd>)> {
    let o = prepare_indoor_end(catalog, origin, settings, true)?;
    let d = prepare_indoor_end(catalog, destination, settings, false)?;
    for (end, point) in [(&o, origin), (&d, destination)] {
        if let Some(end) = end {
            point.lon = end.lon;
            point.lat = end.lat;
            point.z = None;
        }
    }
    Ok((o, d))
}

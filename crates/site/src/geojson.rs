//! Sites and indoor routes as GeoJSON (WGS84 `[lon, lat]`).

use netweevil_indoor::input::Vtx;
use serde_json::{Value, json};

use crate::route::IndoorRouteResult;
use crate::runtime::IndoorSite;

fn coord(site: &IndoorSite, v: &Vtx) -> Value {
    let [lon, lat] = site.frame().to_lonlat(v[0], v[1]);
    json!([lon, lat])
}

fn line(site: &IndoorSite, vs: &[Vtx]) -> Value {
    Value::Array(
        vs.iter()
            .filter(|v| v.len() >= 2)
            .map(|v| coord(site, v))
            .collect(),
    )
}

fn feature(geometry: Value, properties: Value) -> Value {
    json!({ "type": "Feature", "geometry": geometry, "properties": properties })
}

fn name(props: &Value) -> Value {
    props.get("name").cloned().unwrap_or(Value::Null)
}

/// Surfaces, barriers, edges, portals, verticals, POIs and entrances of a
/// site, each with a `feature_type` property. With `level`, only the
/// features on that level (verticals: with a stop on it).
pub fn site_geojson(site: &IndoorSite, level: Option<&str>) -> Value {
    let on = |l: &Option<String>| level.is_none() || l.as_deref() == level;
    let input = &site.input;
    let mut features = vec![];
    for s in input.surfaces.iter().filter(|s| on(&s.level)) {
        let polys: Vec<&Vec<Vec<Vtx>>> = match (&s.polygon, &s.multipolygon) {
            (Some(p), _) => vec![p],
            (None, Some(mp)) => mp.iter().collect(),
            _ => continue,
        };
        let coords: Vec<Value> = polys
            .iter()
            .map(|p| Value::Array(p.iter().map(|r| line(site, r)).collect()))
            .collect();
        features.push(feature(
            json!({ "type": "MultiPolygon", "coordinates": coords }),
            json!({
                "feature_type": "surface", "id": s.id, "level": s.level, "space": s.space,
                "class": s.class, "walls": s.walls, "tags": s.tags, "name": name(&s.props),
            }),
        ));
    }
    for b in input.barriers.iter().filter(|b| on(&b.level)) {
        features.push(feature(
            json!({ "type": "LineString", "coordinates": line(site, &b.line) }),
            json!({ "feature_type": "barrier", "id": b.id, "level": b.level, "tags": b.tags }),
        ));
    }
    for e in input.edges.iter().filter(|e| on(&e.level)) {
        features.push(feature(
            json!({ "type": "LineString", "coordinates": line(site, &e.line) }),
            json!({
                "feature_type": "edge", "id": e.id, "level": e.level, "class": e.class,
                "oneway": e.oneway, "step_free": e.step_free, "tags": e.tags, "name": name(&e.props),
            }),
        ));
    }
    let entrances: Vec<&str> = site
        .manifest
        .entrances
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    for p in input.portals.iter().filter(|p| on(&p.level)) {
        let geometry = match (&p.segment, &p.at) {
            (Some(seg), _) => json!({ "type": "LineString", "coordinates": line(site, seg) }),
            (None, Some(at)) if at.len() >= 2 => {
                json!({ "type": "Point", "coordinates": coord(site, at) })
            }
            _ => continue,
        };
        let id = p.id.as_deref().unwrap_or_default();
        features.push(feature(
            geometry,
            json!({
                "feature_type": "portal", "id": p.id, "level": p.level, "kind": p.kind,
                "width": p.width, "step_free": p.step_free, "entrance": entrances.contains(&id),
                "tags": p.tags, "name": name(&p.props),
            }),
        ));
    }
    for v in &input.verticals {
        let stops: Vec<_> = v.stops.iter().filter(|s| on(&s.level)).collect();
        if stops.is_empty() {
            continue;
        }
        for s in stops {
            if s.at.len() < 2 {
                continue;
            }
            features.push(feature(
                json!({ "type": "Point", "coordinates": coord(site, &s.at) }),
                json!({
                    "feature_type": "vertical", "id": v.id, "kind": v.kind, "level": s.level,
                    "levels": v.stops.iter().map(|s| s.level.clone()).collect::<Vec<_>>(),
                    "step_free": v.step_free, "direction": v.direction, "name": name(&v.props),
                }),
            ));
        }
    }
    for e in input.nodes.iter().filter(|n| on(&n.level)) {
        let Some(at) = e.at.as_ref().filter(|a| a.len() >= 2) else {
            continue;
        };
        features.push(feature(
            json!({ "type": "Point", "coordinates": coord(site, at) }),
            json!({ "feature_type": "node", "id": e.id, "level": e.level, "tags": e.tags,
                    "name": name(&e.props) }),
        ));
    }
    for e in site.router.elements(netweevil_indoor::ElementKind::Poi) {
        if !on(&e.level) {
            continue;
        }
        let Some(at) = e.at else { continue };
        let [lon, lat] = site.frame().to_lonlat(at[0], at[1]);
        features.push(feature(
            json!({ "type": "Point", "coordinates": [lon, lat] }),
            json!({
                "feature_type": "poi", "id": e.id, "level": e.level, "category": e.subkind,
                "name": name(&e.props), "ref": e.props.get("ref").cloned().unwrap_or(Value::Null),
            }),
        ));
    }
    for e in site.manifest.entrances.iter().filter(|e| on(&e.level)) {
        features.push(feature(
            json!({ "type": "Point", "coordinates": [e.outdoor_lon, e.outdoor_lat] }),
            json!({
                "feature_type": "entrance", "id": e.id, "kind": e.kind, "level": e.level,
                "step_free": e.step_free, "name": e.name,
            }),
        ));
    }
    json!({
        "type": "FeatureCollection",
        "features": features,
        "properties": {
            "site_id": site.site_id(),
            "label": site.manifest.label,
            "level": level,
            "levels": site.manifest.levels,
        },
    })
}

/// One LineString feature per route segment, with the segment's properties.
pub fn indoor_route_features(result: &IndoorRouteResult, extra: &Value) -> Vec<Value> {
    result
        .segments
        .iter()
        .map(|s| {
            let coords: Vec<[f64; 3]> = result
                .geometry
                .get(s.from_index..=s.to_index.min(result.geometry.len().saturating_sub(1)))
                .unwrap_or_default()
                .to_vec();
            let mut props = json!({
                "segment_type": s.segment_type, "kind": s.kind, "id": s.id, "level": s.level,
                "space": s.space, "class": s.class, "length_m": s.length_m,
                "duration_s": s.duration_s, "site_id": result.site_id,
            });
            if let (Value::Object(p), Value::Object(e)) = (&mut props, extra) {
                p.extend(e.clone());
            }
            feature(
                json!({ "type": "LineString", "coordinates": coords }),
                props,
            )
        })
        .collect()
}

/// An indoor route as a FeatureCollection of its segments.
pub fn indoor_route_geojson(result: &IndoorRouteResult) -> Value {
    json!({
        "type": "FeatureCollection",
        "features": indoor_route_features(result, &json!({})),
        "properties": {
            "route_id": result.route_id,
            "site_id": result.site_id,
            "profile": result.profile,
            "mode": result.mode,
            "summary": result.summary,
            "spaces": result.spaces,
            "warnings": result.warnings,
        },
    })
}

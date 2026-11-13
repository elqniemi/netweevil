use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use netweevil_core::LocalFrame;
use netweevil_indoor::{RouteMode, parse_input};
use netweevil_persist::WorkspacePaths;
use netweevil_persist::indoor::{IndoorEntranceKind, IndoorSiteSource};

use crate::geojson::{indoor_route_geojson, site_geojson};
use crate::route::{IndoorPoint, IndoorProfileSpec, IndoorRouteRequest, LevelRef, route_indoor};
use crate::runtime::SiteCatalog;
use crate::store::build_site;

pub(crate) const ANCHOR: LocalFrame = LocalFrame {
    lon: 6.5665,
    lat: 53.2194,
    rotation_deg: 0.0,
    elevation_m: 0.0,
};

pub(crate) fn temp_workspace() -> WorkspacePaths {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "netweevil-site-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    WorkspacePaths::discover(&dir).unwrap()
}

pub(crate) fn demo_input() -> netweevil_indoor::Input {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/indoor/demo_site.json"
    );
    parse_input(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The demo site stored in a fresh workspace, anchored at `frame`.
pub(crate) fn demo_catalog(frame: LocalFrame) -> SiteCatalog {
    let paths = temp_workspace();
    let source = IndoorSiteSource {
        path: "demo_site.json".into(),
        format: "input_json".into(),
        crs: None,
        bbox: None,
    };
    let built = build_site(&paths, "demo", None, demo_input(), frame, source).unwrap();
    assert!(
        built.report.warnings.is_empty(),
        "{:?}",
        built.manifest.warnings
    );
    SiteCatalog::new(paths)
}

#[test]
fn stored_demo_site_lists_its_entrances_with_the_outdoor_side() {
    let catalog = demo_catalog(ANCHOR);
    let site = catalog.get("demo").unwrap();
    let m = &site.manifest;
    assert_eq!(m.levels.len(), 2);
    assert_eq!(m.counts.surfaces, 11);
    let ids: Vec<&str> = m.entrances.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["west_entrance", "east_entrance"]);
    let west = &m.entrances[0];
    assert_eq!(west.kind, IndoorEntranceKind::Portal);
    assert!(!west.step_free);
    assert_eq!(west.name.as_deref(), Some("West entrance"));
    // The outdoor side of the west entrance lies west of the wall at x = 0.
    let wall = ANCHOR.to_lonlat(0.0, 10.0);
    assert!(west.outdoor_lon < wall[0] && west.lon > west.outdoor_lon - 1e-4);
    let east = &m.entrances[1];
    assert!(east.outdoor_lon > ANCHOR.to_lonlat(40.0, 10.0)[0]);
    // The footprint contains the building and not the street outside.
    let inside = ANCHOR.to_lonlat(20.0, 10.0);
    let outside = ANCHOR.to_lonlat(-30.0, 10.0);
    assert!(site.contains(inside[0], inside[1]));
    assert!(!site.contains(outside[0], outside[1]));
    assert_eq!(
        catalog.site_at(inside[0], inside[1]).unwrap().site_id(),
        "demo"
    );
    assert!(catalog.site_at(outside[0], outside[1]).is_err());
    // A cached site is the same object.
    assert!(std::sync::Arc::ptr_eq(&site, &catalog.get("demo").unwrap()));
}

#[test]
fn indoor_routes_by_poi_name_ref_and_coordinates() {
    let catalog = demo_catalog(ANCHOR);
    let site = catalog.get("demo").unwrap();
    let mut request = IndoorRouteRequest {
        route_id: Some("r".into()),
        site_id: "demo".into(),
        origin: IndoorPoint {
            poi: Some("Workshop".into()),
            ..IndoorPoint::default()
        },
        destination: IndoorPoint {
            poi: Some("1.14".into()),
            ..IndoorPoint::default()
        },
        profile: IndoorProfileSpec::Name("wheelchair".into()),
        mode: None,
    };
    let r = route_indoor(&site, &request).unwrap();
    let verticals: Vec<&str> = r
        .segments
        .iter()
        .filter(|s| s.segment_type == "vertical")
        .map(|s| s.kind.as_str())
        .collect();
    assert_eq!(verticals, ["lift"]);
    assert_eq!(r.destination_poi.as_deref(), Some("room_1.14"));
    assert!(r.summary.climb_m > 3.0);
    // Geometry is WGS84 near the anchor, with z on the upper floor at the end.
    let last = r.geometry.last().unwrap();
    assert!((last[0] - ANCHOR.lon).abs() < 0.001 && (last[1] - ANCHOR.lat).abs() < 0.001);
    assert!((last[2] - 3.5).abs() < 1e-6);
    let walk = route_indoor(
        &site,
        &IndoorRouteRequest {
            profile: IndoorProfileSpec::Name("walk".into()),
            ..request.clone()
        },
    )
    .unwrap();
    assert!(walk.segments.iter().any(|s| s.kind == "stairs"));
    assert!(walk.summary.duration_s < r.summary.duration_s);

    // A point by coordinates and numeric level, to a space id.
    let [lon, lat] = ANCHOR.to_lonlat(15.0, 4.0);
    request.origin = IndoorPoint {
        lon: Some(lon),
        lat: Some(lat),
        level: Some(LevelRef::Number(0.0)),
        ..IndoorPoint::default()
    };
    request.destination = IndoorPoint {
        space: Some("room_1.20".into()),
        ..IndoorPoint::default()
    };
    let r = route_indoor(&site, &request).unwrap();
    assert_eq!(r.spaces.first().map(String::as_str), Some("room_0.02"));
    assert_eq!(r.spaces.last().map(String::as_str), Some("room_1.20"));
    // Edges-only mode has no edges to walk in this site.
    request.mode = Some(RouteMode::EdgesOnly);
    assert!(route_indoor(&site, &request).is_err());
    // Unknown names and levels are errors that name the site.
    request.mode = None;
    request.origin.level = Some(LevelRef::Id("7".into()));
    let e = format!("{:#}", route_indoor(&site, &request).unwrap_err());
    assert!(e.contains("no level '7'"), "{e}");

    let gj = indoor_route_geojson(&r);
    assert_eq!(gj["features"].as_array().unwrap().len(), r.segments.len());
}

#[test]
fn site_geojson_filters_by_level() {
    let catalog = demo_catalog(ANCHOR);
    let site = catalog.get("demo").unwrap();
    let all = site_geojson(&site, None);
    let l1 = site_geojson(&site, Some("1"));
    let count = |v: &serde_json::Value, t: &str| {
        v["features"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["properties"]["feature_type"] == t)
            .count()
    };
    assert_eq!(count(&all, "surface"), 11);
    assert_eq!(count(&l1, "surface"), 5);
    assert_eq!(count(&all, "entrance"), 2);
    assert_eq!(count(&l1, "entrance"), 0);
    assert_eq!(count(&l1, "vertical"), 2);
}

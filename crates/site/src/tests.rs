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

mod door_to_door {
    use std::sync::Arc;

    use netweevil_query::PreparedRoutingEngine;
    use netweevil_query::testing::{edge, metrics, topology};
    use serde_json::json;

    use super::*;
    use crate::door_to_door::{
        DoorToDoorLegType, DoorToDoorPoint, DoorToDoorRequest, DoorToDoorSettings,
        door_to_door_geojson, door_to_door_route,
    };

    /// A 60 x 4 m hall with an entrance in each end wall.
    pub(super) fn hall() -> netweevil_indoor::Input {
        serde_json::from_value(json!({
            "levels": [{"id": "0", "z": 0.0}],
            "surfaces": [{"id": "hall", "level": "0", "space": "hall", "walls": true,
                "tags": ["indoor"], "polygon": [[[0, 0], [60, 0], [60, 4], [0, 4], [0, 0]]]}],
            "portals": [
                {"id": "west", "kind": "entrance", "segment": [[0, 1], [0, 3]], "level": "0",
                 "tags": ["entrance"], "width": 2.0},
                {"id": "east", "kind": "entrance", "segment": [[60, 1], [60, 3]], "level": "0",
                 "tags": ["entrance"], "width": 2.0}
            ],
            "pois": [{"id": "desk", "at": [20, 2], "level": "0", "props": {"name": "Desk"}}]
        }))
        .unwrap()
    }

    /// A street along the hall from x = -5 to x = 65 (slow: 1000 s), then
    /// on to x = 200 (100 s).
    fn engine() -> PreparedRoutingEngine {
        let p = |x: f64, y: f64| {
            let [lon, lat] = ANCHOR.to_lonlat(x, y);
            (lon, lat)
        };
        let t = topology(
            &[p(-5.0, 2.0), p(65.0, 2.0), p(200.0, 2.0)],
            &[
                edge(0, 0, 1, 1, 70),
                edge(1, 1, 0, 1, 70),
                edge(2, 1, 2, 2, 135),
                edge(3, 2, 1, 2, 135),
            ],
        );
        PreparedRoutingEngine::new(
            Arc::new(t),
            Arc::new(metrics(&[1000.0, 1000.0, 100.0, 100.0])),
            None,
        )
        .unwrap()
    }

    pub(super) fn catalog() -> SiteCatalog {
        let paths = temp_workspace();
        let source = IndoorSiteSource {
            path: "hall.json".into(),
            format: "input_json".into(),
            crs: None,
            bbox: None,
        };
        build_site(&paths, "hall", None, hall(), ANCHOR, source).unwrap();
        SiteCatalog::new(paths)
    }

    fn street(x: f64, y: f64) -> DoorToDoorPoint {
        let [lon, lat] = ANCHOR.to_lonlat(x, y);
        DoorToDoorPoint {
            point: IndoorPoint {
                lon: Some(lon),
                lat: Some(lat),
                ..IndoorPoint::default()
            },
            ..DoorToDoorPoint::default()
        }
    }

    fn desk() -> DoorToDoorPoint {
        DoorToDoorPoint {
            site_id: Some("hall".into()),
            point: IndoorPoint {
                poi: Some("Desk".into()),
                ..IndoorPoint::default()
            },
            ..DoorToDoorPoint::default()
        }
    }

    fn request(origin: DoorToDoorPoint, destination: DoorToDoorPoint) -> DoorToDoorRequest {
        DoorToDoorRequest {
            route_id: "d2d".into(),
            origin,
            destination,
            indoor: DoorToDoorSettings::default(),
            snap: Default::default(),
        }
    }

    #[test]
    fn the_nearer_entrance_loses_when_its_street_is_slow() {
        let (engine, catalog) = (engine(), catalog());
        // The desk is 20 m from the west entrance and 40 m from the east one.
        let r =
            door_to_door_route(&engine, &catalog, &request(desk(), street(200.0, 2.0))).unwrap();
        let types: Vec<_> = r.legs.iter().map(|l| l.leg_type).collect();
        assert_eq!(
            types,
            [DoorToDoorLegType::Indoor, DoorToDoorLegType::Street]
        );
        let e = r.origin_entrance.as_ref().unwrap();
        assert_eq!(e.entrance.id, "east");
        assert_eq!(r.summary.origin_candidates, 2);
        assert!(r.summary.street_duration_s < 300.0, "{:?}", r.summary);
        let indoor = r.legs[0].indoor.as_ref().unwrap();
        assert!(
            indoor.summary.length_m > 35.0,
            "{}",
            indoor.summary.length_m
        );
        assert!(
            (r.summary.duration_s - r.legs.iter().map(|l| l.duration_s).sum::<f64>()).abs() < 1e-9
        );
        // The street leg keeps its own generalized cost.
        let leg = r.legs[1].street.as_ref().unwrap();
        assert_eq!(
            r.summary.street_generalized_cost,
            leg.summary.total_generalized_cost
        );

        // Inbound: from the street to the desk, by the same entrance.
        let r =
            door_to_door_route(&engine, &catalog, &request(street(200.0, 2.0), desk())).unwrap();
        let types: Vec<_> = r.legs.iter().map(|l| l.leg_type).collect();
        assert_eq!(
            types,
            [DoorToDoorLegType::Street, DoorToDoorLegType::Indoor]
        );
        assert_eq!(r.destination_entrance.as_ref().unwrap().entrance.id, "east");

        let gj = door_to_door_geojson(&r);
        let legs: Vec<_> = gj["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["properties"]["leg_type"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(legs, ["street", "indoor"]);
    }

    #[test]
    fn a_point_with_a_level_finds_its_site_and_same_site_trips_stay_indoors() {
        let (engine, catalog) = (engine(), catalog());
        let mut inside = street(50.0, 2.0);
        inside.point.level = Some(LevelRef::Number(0.0));
        let r = door_to_door_route(&engine, &catalog, &request(desk(), inside)).unwrap();
        assert_eq!(r.legs.len(), 1);
        assert_eq!(r.legs[0].leg_type, DoorToDoorLegType::Indoor);
        assert!(r.origin_entrance.is_none());
        // Two street points are not a door-to-door request.
        assert!(
            door_to_door_route(
                &engine,
                &catalog,
                &request(street(0.0, -10.0), street(200.0, 2.0))
            )
            .is_err()
        );
        // A step-free profile cannot use an entrance with steps, even the faster one.
        let mut input = hall();
        input.portals[1].step_free = false;
        let source = IndoorSiteSource {
            path: "hall.json".into(),
            format: "input_json".into(),
            crs: None,
            bbox: None,
        };
        build_site(catalog.paths(), "hall", None, input, ANCHOR, source).unwrap();
        let mut req = request(desk(), street(200.0, 2.0));
        let r = door_to_door_route(&engine, &catalog, &req).unwrap();
        assert_eq!(r.origin_entrance.unwrap().entrance.id, "east");
        req.indoor.profile = IndoorProfileSpec::Name("wheelchair".into());
        let r = door_to_door_route(&engine, &catalog, &req).unwrap();
        assert_eq!(r.origin_entrance.unwrap().entrance.id, "west");
        assert_eq!(r.summary.origin_candidates, 1);
    }
}

mod transit_access {
    use netweevil_transit::{
        AccessMode, StreetTimeEstimator, TransitPoint, TransitStop, TransitStreetPath,
    };
    use serde_json::json;

    use super::door_to_door::catalog;
    use super::*;
    use crate::door_to_door::DoorToDoorSettings;
    use crate::transit_access::{IndoorStreetEstimator, prepare_indoor_transit_request};

    /// Street paths whose time grows with the distance west of a stop far to
    /// the east: 1000 s from the west entrance, 100 s from the east one.
    struct EastIsFast;

    impl StreetTimeEstimator for EastIsFast {
        fn street_time_s(
            &self,
            _: AccessMode,
            _: bool,
            _: f64,
            _: f64,
            _: f64,
            _: f64,
        ) -> Option<u32> {
            None
        }

        fn point_to_stop_path(
            &self,
            _: AccessMode,
            lon: f64,
            lat: f64,
            stop: &TransitStop,
        ) -> Option<TransitStreetPath> {
            let west = lon < ANCHOR.to_lonlat(30.0, 0.0)[0];
            let mut path = TransitStreetPath::time_only(if west { 1000 } else { 100 });
            path.distance_m = Some(150.0);
            path.geometry = vec![[lon, lat, 0.0], [stop.lon, stop.lat, 0.0]];
            Some(path)
        }
    }

    fn point(id: &str) -> TransitPoint {
        serde_json::from_value(json!({"id": id})).unwrap()
    }

    #[test]
    fn indoor_origins_take_the_entrance_with_the_least_total_time() {
        let catalog = catalog();
        let mut origin = point("o");
        origin.site_id = Some("hall".into());
        origin.poi = Some("Desk".into());
        let mut destination = point("d");
        (destination.lon, destination.lat) = (6.6, 53.22);
        let (o, d) = prepare_indoor_transit_request(
            &catalog,
            &mut origin,
            &mut destination,
            &DoorToDoorSettings::default(),
        )
        .unwrap();
        assert!(d.is_none());
        let o = o.unwrap();
        assert_eq!(o.entrances.len(), 2);
        // The request origin moves to the desk.
        let desk = ANCHOR.to_lonlat(20.0, 2.0);
        assert!((origin.lon - desk[0]).abs() < 1e-5 && (origin.lat - desk[1]).abs() < 1e-5);

        let estimator = IndoorStreetEstimator::new(&EastIsFast, Some(o), None);
        let [slon, slat] = ANCHOR.to_lonlat(500.0, 0.0);
        let stop = TransitStop {
            stop_id: "s".into(),
            name: "Stop".into(),
            lon: slon,
            lat: slat,
            binding: None,
        };
        let path = estimator
            .point_to_stop_path_with_snap_limit(AccessMode::Walk, &origin, &stop, 50.0)
            .unwrap();
        let part = path.indoor.as_ref().unwrap();
        assert_eq!(part.entrance_id, "east");
        assert_eq!(part.site_id, "hall");
        assert!(
            part.travel_time_s > 20 && part.travel_time_s < 60,
            "{}",
            part.travel_time_s
        );
        assert!(path.travel_time_s >= 100 + part.travel_time_s);
        // The geometry starts at the desk and runs through the entrance to the stop.
        let first = path.geometry.first().unwrap();
        assert!((first[0] - desk[0]).abs() < 1e-5);
        assert_eq!(path.geometry.last().unwrap()[0], slon);
        assert_eq!(
            &path.geometry[..part.geometry.len()],
            part.geometry.as_slice()
        );
        // Points without indoor fields pass through to the wrapped estimator.
        let plain = IndoorStreetEstimator::new(&EastIsFast, None, None);
        let p = plain
            .point_to_stop_path_with_snap_limit(AccessMode::Walk, &destination, &stop, 50.0)
            .unwrap();
        assert!(p.indoor.is_none());
    }
}

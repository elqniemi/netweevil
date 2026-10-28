use super::*;
use crate::tests::fixtures::{edge, exact_engine, metrics, point, route_request, topology};
use netweevil_core::{AdminArea, DirectedEdge, TopologyElevation};

/// Four nodes 0.001 degrees apart along latitude 53 at 10, 12, 11 and 15 m;
/// nodes 0-1 are in Groningen (Amsterdam time), 2-3 in Germany (Berlin).
fn hilly_border_topology() -> TopologyBundle {
    let rise = |id: u32, from: u32, ascent_m: f32, descent_m: f32| DirectedEdge {
        ascent_m,
        descent_m,
        ..edge(id, from, from + 1, 1, 67)
    };
    let mut topology = topology(
        "annotations",
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0), (6.003, 53.0)],
        &[
            rise(0, 0, 2.0, 0.0),
            rise(1, 1, 0.0, 1.0),
            rise(2, 2, 4.0, 0.0),
        ],
    );
    for (node, z) in topology.nodes.iter_mut().zip([10.0, 12.0, 11.0, 15.0]) {
        node.z = z;
    }
    topology.regions.admin_areas = vec![
        AdminArea {
            country_code: "NL".to_string(),
            state_code: Some("NL-GR".to_string()),
            state_name: Some("Groningen".to_string()),
            ..AdminArea::default()
        },
        AdminArea {
            country_code: "DE".to_string(),
            ..AdminArea::default()
        },
    ];
    topology.regions.node_admin_area = vec![0, 0, 1, 1];
    topology.regions.timezones = vec!["Europe/Amsterdam".into(), "Europe/Berlin".into()];
    topology.regions.node_timezone = vec![0, 0, 1, 1];
    topology.elevation = TopologyElevation {
        dem: None,
        edge_max_uphill_permille: vec![30, 0, 70],
        edge_max_downhill_permille: vec![0, 15, 0],
    };
    topology
}

fn annotated_route(topology: TopologyBundle) -> RouteResult {
    let engine = exact_engine(topology, metrics(&[10.0, 10.0, 10.0]));
    let mut request = route_request("r", point("a", 6.0, 53.0), point("b", 6.003, 53.0));
    request.returns.admin_areas = true;
    request.returns.elevation = true;
    engine.execute_route(&request).expect("route succeeds")
}

#[test]
fn routes_report_their_areas_border_crossings_and_zones() {
    let route = annotated_route(hilly_border_topology());
    let regions = route.regions.expect("regions requested");
    assert_eq!(regions.border_crossing_count, 1);
    assert_eq!(regions.timezones, ["Europe/Amsterdam", "Europe/Berlin"]);
    let summary = regions
        .spans
        .iter()
        .map(|span| {
            (
                span.country_code.as_deref(),
                span.state_code.as_deref(),
                span.driving_side,
                span.start_distance_m,
                span.distance_m,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![
            (
                Some("NL"),
                Some("NL-GR"),
                Some(DrivingSide::Right),
                0.0,
                134.0
            ),
            (Some("DE"), None, Some(DrivingSide::Right), 134.0, 67.0),
        ]
    );
}

#[test]
fn routes_report_an_elevation_profile_with_grades() {
    let route = annotated_route(hilly_border_topology());
    let profile = route.elevation.expect("elevation requested");
    let heights = profile
        .points
        .iter()
        .map(|(_, height)| *height)
        .collect::<Vec<_>>();
    assert_eq!(heights, [Some(10.0), Some(12.0), Some(11.0), Some(15.0)]);
    assert_eq!(profile.points[0].0, 0.0);
    assert!(
        (profile.points[3].0 - 200.7).abs() < 0.5,
        "{:?}",
        profile.points
    );
    assert_eq!((profile.ascent_m, profile.descent_m), (6.0, 1.0));
    assert_eq!(
        (profile.min_elevation_m, profile.max_elevation_m),
        (Some(10.0), Some(15.0))
    );
    // The stored steepest grades exceed the vertex-to-vertex ones.
    assert_eq!(profile.max_uphill_grade, 0.07);
    assert!((profile.max_downhill_grade - 0.015).abs() < 1e-3);
    assert!(route.edge_path.len() == 3 && route.geometry.is_none());
}

#[test]
fn datasets_without_heights_report_unknown_elevations_and_no_areas() {
    let mut topology = hilly_border_topology();
    for node in &mut topology.nodes {
        node.z = 0.0;
    }
    topology.regions = Default::default();
    let route = annotated_route(topology);
    let profile = route.elevation.unwrap();
    assert!(profile.points.iter().all(|(_, height)| height.is_none()));
    assert_eq!(profile.min_elevation_m, None);
    let regions = route.regions.unwrap();
    assert!(regions.spans.is_empty() && regions.timezones.is_empty());
    assert_eq!(regions.border_crossing_count, 0);
}

fn country_codes(regions: &RouteRegions) -> Vec<&str> {
    regions
        .spans
        .iter()
        .filter_map(|span| span.country_code.as_deref())
        .collect()
}

#[test]
fn waypoint_legs_report_their_own_areas_and_elevation() {
    let engine = exact_engine(hilly_border_topology(), metrics(&[10.0, 10.0, 10.0]));
    let stop = |id, lon| crate::Waypoint {
        point: point(id, lon, 53.0),
        kind: crate::WaypointKind::Break,
    };
    let mut request = crate::WaypointRequest {
        route_id: "w".into(),
        waypoints: vec![stop("a", 6.0), stop("b", 6.002), stop("c", 6.003)],
        snap: Default::default(),
        returns: Default::default(),
        optimize_order: false,
        temporal: Default::default(),
    };
    request.returns.admin_areas = true;
    request.returns.elevation = true;
    let result = engine.execute_waypoints(&request).expect("waypoints route");

    let [first, second] = &result.legs[..] else {
        panic!("two legs expected");
    };
    let first_regions = first.regions.as_ref().expect("regions requested");
    assert_eq!(country_codes(first_regions), ["NL"]);
    assert_eq!(first_regions.border_crossing_count, 1);
    let second_regions = second.regions.as_ref().expect("regions requested");
    assert_eq!(country_codes(second_regions), ["DE"]);
    assert_eq!(second_regions.border_crossing_count, 0);

    let first_elevation = first.elevation.as_ref().expect("elevation requested");
    assert_eq!(
        (first_elevation.ascent_m, first_elevation.descent_m),
        (2.0, 1.0)
    );
    let second_elevation = second.elevation.as_ref().expect("elevation requested");
    assert_eq!(
        (second_elevation.ascent_m, second_elevation.max_uphill_grade),
        (4.0, 0.07)
    );
}

#[test]
fn alternative_routes_report_their_own_areas_and_elevation() {
    // A detour from node 1 through node 4 (in Germany, 13 m) to node 3.
    let base = hilly_border_topology();
    let mut edges = (0..base.edge_count())
        .map(|index| base.edge(index))
        .collect::<Vec<_>>();
    for (id, from, to, ascent_m) in [(3, 1, 4, 1.0), (4, 4, 3, 2.0)] {
        edges.push(DirectedEdge {
            ascent_m,
            ..edge(id, from, to, 2, 67)
        });
    }
    let mut nodes = base
        .nodes
        .iter()
        .map(|node| (node.lon, node.lat))
        .collect::<Vec<_>>();
    nodes.push((6.0015, 53.0005));
    let mut detoured = topology("annotations-detour", &nodes, &edges);
    for (node, z) in detoured
        .nodes
        .iter_mut()
        .zip([10.0, 12.0, 11.0, 15.0, 13.0])
    {
        node.z = z;
    }
    detoured.regions = base.regions;
    detoured.regions.node_admin_area.push(1);
    detoured.regions.node_timezone.push(1);
    detoured.elevation = base.elevation;

    let engine = exact_engine(detoured, metrics(&[10.0, 10.0, 10.0, 10.0, 15.0]));
    let mut request = route_request("r", point("a", 6.0, 53.0), point("b", 6.003, 53.0));
    request.returns.admin_areas = true;
    request.returns.elevation = true;
    request.alternatives.max_routes = 2;
    let route = engine.execute_route(&request).expect("route succeeds");

    assert_eq!(route.edge_path, [0, 1, 2]);
    let [alternative] = &route.alternatives[..] else {
        panic!("one alternative expected");
    };
    assert_eq!(alternative.edge_path, [0, 3, 4]);
    let regions = alternative.regions.as_ref().expect("regions requested");
    assert_eq!(country_codes(regions), ["NL", "DE"]);
    assert_eq!(regions.border_crossing_count, 1);
    let elevation = alternative.elevation.as_ref().expect("elevation requested");
    assert_eq!(elevation.ascent_m, 2.0 + 1.0 + 2.0);
    assert_eq!(elevation.max_elevation_m, Some(15.0));
    // The main route keeps its own profile.
    assert_eq!(route.elevation.expect("elevation requested").ascent_m, 6.0);
}

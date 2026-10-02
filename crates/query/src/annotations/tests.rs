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

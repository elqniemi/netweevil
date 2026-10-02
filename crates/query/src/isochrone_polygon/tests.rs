use super::*;

const LAT: f64 = 53.0;

/// Longitude/latitude offset from (6.0, 53.0) by metres east and north.
fn offset(east_m: f64, north_m: f64) -> [f64; 2] {
    [
        6.0 + east_m / (METERS_PER_DEGREE_LAT * LAT.to_radians().cos()),
        LAT + north_m / METERS_PER_DEGREE_LAT,
    ]
}

/// Even-odd containment for a closed lon/lat ring.
fn ring_contains(ring: &[[f64; 2]], point: [f64; 2]) -> bool {
    let mut inside = false;
    for pair in ring.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if (a[1] > point[1]) != (b[1] > point[1]) {
            let intersect = a[0] + (point[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if point[0] < intersect {
                inside = !inside;
            }
        }
    }
    inside
}

#[test]
fn traces_a_single_segment_into_one_polygon() {
    let polygons = trace_reachable_polygons(&[(offset(0.0, 0.0), offset(500.0, 0.0))], 25.0, 0.0);
    assert_eq!(polygons.len(), 1);
    let polygon = &polygons[0];
    assert!(polygon.holes.is_empty());
    assert!(polygon.exterior.len() >= 5);
    assert_eq!(polygon.exterior.first(), polygon.exterior.last());
    // The polygon must hug the segment: no vertex farther than a few cells
    // from the segment's latitude.
    for vertex in &polygon.exterior {
        assert!(
            (vertex[1] - LAT).abs() <= offset(0.0, 100.0)[1] - LAT,
            "vertex {vertex:?} strays from the segment"
        );
    }
}

#[test]
fn a_ring_road_produces_a_hole_over_the_unreachable_middle() {
    // A square ring of roads, 1 km per side: the enclosed block is not
    // reachable network and must become a polygon hole, not be covered.
    let side_m = 1_000.0;
    let corners = [
        offset(0.0, 0.0),
        offset(side_m, 0.0),
        offset(side_m, side_m),
        offset(0.0, side_m),
    ];
    let segments = (0..4)
        .map(|side| (corners[side], corners[(side + 1) % 4]))
        .collect::<Vec<_>>();
    let polygons = trace_reachable_polygons(&segments, 25.0, 0.0);
    assert_eq!(polygons.len(), 1);
    assert_eq!(polygons[0].holes.len(), 1);
    assert!(
        ring_contains(&polygons[0].holes[0], offset(side_m / 2.0, side_m / 2.0)),
        "ring centre must lie inside the traced hole"
    );
}

#[test]
fn disconnected_segments_produce_separate_polygons() {
    let near = (offset(0.0, 0.0), offset(200.0, 0.0));
    let far = (offset(5_000.0, 0.0), offset(5_200.0, 0.0));
    assert_eq!(trace_reachable_polygons(&[near, far], 25.0, 0.0).len(), 2);
}

#[test]
fn thin_cavities_become_holes_regardless_of_ring_start() {
    // Two parallel roads 110 m apart, closed at both ends, leave a
    // one-cell-wide cavity. Varying its height yields both odd and even
    // long boundary edges; the hole test point must stay inside it.
    for height_m in [310.0, 335.0, 360.0, 385.0] {
        let corners = [
            offset(0.0, 0.0),
            offset(110.0, 0.0),
            offset(110.0, height_m),
            offset(0.0, height_m),
        ];
        let segments = (0..4)
            .map(|side| (corners[side], corners[(side + 1) % 4]))
            .collect::<Vec<_>>();
        let polygons = trace_reachable_polygons(&segments, 25.0, 0.0);
        assert_eq!(polygons.len(), 1);
        assert_eq!(
            polygons[0].holes.len(),
            1,
            "the thin cavity of a {height_m} m loop must be a hole"
        );
    }
}

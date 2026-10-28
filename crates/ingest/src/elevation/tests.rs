use super::*;
use crate::test_util::pending_way;
use netweevil_core::{RasterCrs, RasterGrid};

const CELL_DEG: f64 = 0.0001;
const LAT: f64 = 0.00055;

/// A V-shaped valley: 100 columns of 0.0001 degree cells at the equator
/// with height `|column - 50|`.
fn valley() -> DemRaster {
    let grid = RasterGrid {
        columns: 100,
        rows: 10,
        min_x: 0.0,
        max_y: 0.001,
        cell_width: CELL_DEG,
        cell_height: CELL_DEG,
    };
    let values = (0..10)
        .flat_map(|_| (0..100).map(|column: i32| (column - 50).abs() as f32))
        .collect();
    DemRaster::from_values(grid, RasterCrs::Geographic, values).unwrap()
}

fn lon(column: f64) -> f64 {
    (column + 0.5) * CELL_DEG
}

#[test]
fn bridge_nodes_interpolate_between_their_ground_anchors() {
    let dem = valley();
    let sampling = DemSampling::new(&dem, LAT);
    assert!((sampling.spacing_m - 11.132).abs() < 0.01);
    let mut node_coords = FxHashMap::from_iter(
        [
            (1, 5.0),
            (2, 10.0),
            (3, 30.0),
            (4, 70.0),
            (5, 80.0),
            (6, 95.0),
            (7, 50.0),
            (8, 51.0),
        ]
        .map(|(id, column)| (id, (lon(column), LAT, 0.0))),
    );
    let ways = vec![
        pending_way(100, &[1, 2]),
        pending_way(101, &[2, 3, 4, 5]),
        pending_way(102, &[5, 6]),
        // A structure that touches no ground node.
        pending_way(103, &[7, 8]),
    ];
    let structures = FxHashSet::from_iter([101, 103]);
    let stats = assign_node_elevations(&sampling, &mut node_coords, &ways, &structures);
    assert_eq!(
        stats,
        NodeElevationStats {
            sampled: 8,
            missing: 0,
            structure: 2,
        }
    );
    let z = |id: i64| node_coords[&id].2;
    assert_eq!((z(1), z(2), z(5), z(6)), (45.0, 40.0, 30.0, 45.0));
    // Path distances 20 and 60 cells of 70 from node 2 (40 m) to node 5 (30 m).
    assert_eq!(z(3), 37.1);
    assert_eq!(z(4), 31.4);
    // Without anchors the terrain sample stays.
    assert_eq!((z(7), z(8)), (0.0, 1.0));
}

#[test]
fn nodes_outside_the_dem_have_unknown_height() {
    let dem = valley();
    let sampling = DemSampling::new(&dem, LAT);
    let mut node_coords = FxHashMap::from_iter([(1, (lon(5.0), LAT, 0.0)), (2, (1.0, 1.0, 0.0))]);
    let stats = assign_node_elevations(
        &sampling,
        &mut node_coords,
        &[pending_way(1, &[1, 2])],
        &FxHashSet::default(),
    );
    assert_eq!((stats.sampled, stats.missing), (1, 1));
    assert!(node_coords[&2].2.is_nan());
}

#[test]
fn ground_segments_are_densified_and_structures_use_their_ends() {
    let dem = valley();
    let sampling = DemSampling::new(&dem, LAT);
    let from = (lon(5.0), LAT, 45.0);
    let to = (lon(95.0), LAT, 45.0);
    let ground = segment_elevation(&sampling, from, to, false);
    assert!((ground.ascent_m - 45.0).abs() <= 0.2, "{ground:?}");
    assert!((ground.descent_m - 45.0).abs() <= 0.2, "{ground:?}");
    // One metre per 11.13 m cell.
    assert_eq!(ground.max_uphill_permille, 90);
    assert_eq!(ground.max_downhill_permille, 90);
    assert_eq!(ground.reversed().ascent_m, ground.descent_m);

    let bridge = segment_elevation(&sampling, from, (lon(95.0), LAT, 46.0), true);
    assert_eq!((bridge.ascent_m, bridge.descent_m), (1.0, 0.0));
    assert_eq!(bridge.max_downhill_permille, 0);
    assert_eq!(bridge.max_uphill_permille, 1);

    let unknown = segment_elevation(&sampling, from, (lon(95.0), LAT, f64::NAN), false);
    assert_eq!(unknown, SegmentElevation::default());
}

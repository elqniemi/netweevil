use super::*;

/// 3x2 grid of 10 m cells with its top-left corner at (100, 50).
fn grid() -> RasterGrid {
    RasterGrid {
        columns: 3,
        rows: 2,
        min_x: 100.0,
        max_y: 50.0,
        cell_width: 10.0,
        cell_height: 10.0,
    }
}

/// Row-major values; `None` marks nodata.
fn lookup(values: &[Option<f32>]) -> impl Fn(u32, u32) -> Option<f32> + '_ {
    move |column, row| values[(row * 3 + column) as usize]
}

#[test]
fn cell_positions_measure_from_cell_centres() {
    let grid = grid();
    assert_eq!(grid.cell_position(105.0, 45.0), Some((0.0, 0.0)));
    assert_eq!(grid.cell_position(125.0, 35.0), Some((2.0, 1.0)));
    assert_eq!(grid.cell_position(100.0, 50.0), Some((-0.5, -0.5)));
    assert_eq!(grid.cell_position(99.9, 45.0), None);
    assert_eq!(grid.cell_position(105.0, 29.9), None);
    assert_eq!(grid.cell_center(1, 1), (115.0, 35.0));
    assert_eq!((grid.max_x(), grid.min_y()), (130.0, 30.0));
}

#[test]
fn bilinear_reproduces_centres_and_interpolates_between_them() {
    let grid = grid();
    let values = [0.0, 10.0, 20.0, 100.0, 110.0, 120.0].map(Some);
    let at = |x: f64, y: f64| {
        let (column, row) = grid.cell_position(x, y).unwrap();
        bilinear(&grid, column, row, lookup(&values)).unwrap()
    };
    assert_eq!(at(105.0, 45.0), 0.0);
    assert_eq!(at(125.0, 35.0), 120.0);
    assert_eq!(at(110.0, 45.0), 5.0);
    assert_eq!(at(110.0, 40.0), 55.0);
    // The outer half cell clamps to the edge cells.
    assert_eq!(at(100.0, 50.0), 0.0);
    assert_eq!(at(130.0, 40.0), 70.0);
}

#[test]
fn bilinear_drops_nodata_neighbours_but_not_a_nodata_own_cell() {
    let grid = grid();
    let values = [
        Some(0.0),
        None,
        Some(20.0),
        Some(100.0),
        Some(110.0),
        Some(120.0),
    ];
    let at = |x: f64, y: f64| {
        let (column, row) = grid.cell_position(x, y).unwrap();
        bilinear(&grid, column, row, lookup(&values))
    };
    // Own cell (0, 0) is valid; the missing (1, 0) neighbour is dropped.
    let value = at(108.0, 42.0).unwrap();
    let expected = (0.0 * 0.7 * 0.7 + 100.0 * 0.7 * 0.3 + 110.0 * 0.3 * 0.3) / (1.0 - 0.3 * 0.7);
    assert!((value - expected).abs() < 1e-9);
    // Own cell (1, 0) is nodata.
    assert_eq!(at(113.0, 47.0), None);
}

#[test]
fn grades_and_decimetres_quantize_as_stored() {
    assert_eq!(grade_to_permille(0.0856), 86);
    assert_eq!(grade_to_permille(-0.2), 0);
    assert_eq!(grade_to_permille(f64::NAN), 0);
    assert_eq!(grade_to_permille(1.0e9), u16::MAX);
    assert_eq!(quantize_decimetres(12.34), 12.3);
    assert_eq!(quantize_decimetres(0.05), 0.1);
    assert_eq!(quantize_decimetres(f64::INFINITY), 0.0);
    assert_eq!(round_height_decimetres(37.142_857), 37.1);
    assert!(round_height_decimetres(f64::NAN).is_nan());
    let elevation = TopologyElevation {
        dem: None,
        edge_max_uphill_permille: vec![120],
        edge_max_downhill_permille: vec![5],
    };
    assert_eq!(elevation.edge_max_grades(0), Some((0.12, 0.005)));
    assert_eq!(elevation.edge_max_grades(1), None);
}

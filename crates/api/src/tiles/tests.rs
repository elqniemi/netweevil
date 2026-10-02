use super::*;

#[test]
fn tile_bbox_corners_project_to_the_tile_extent() {
    let tile = TileId {
        z: 15,
        x: 16_930,
        y: 10_740,
    };
    let [west, south, east, north] = tile.bbox(0.0);
    let (x0, y0) = tile.project(west, north);
    let (x1, y1) = tile.project(east, south);
    let extent = f64::from(TILE_EXTENT);
    for (value, expected) in [(x0, 0.0), (y0, 0.0), (x1, extent), (y1, extent)] {
        assert!((value - expected).abs() < 1e-6, "{value} vs {expected}");
    }
    let [buffered_west, _, _, buffered_north] = tile.bbox(TILE_BUFFER);
    let (bx, by) = tile.project(buffered_west, buffered_north);
    assert!((bx + TILE_BUFFER).abs() < 1e-6 && (by + TILE_BUFFER).abs() < 1e-6);
}

#[test]
fn clip_segment_keeps_the_inside_part_only() {
    let inside = ((10.0, 10.0), (20.0, 30.0));
    assert_eq!(clip_segment(inside.0, inside.1, 0.0, 100.0), Some(inside));
    assert_eq!(
        clip_segment((-50.0, 50.0), (150.0, 50.0), 0.0, 100.0),
        Some(((0.0, 50.0), (100.0, 50.0)))
    );
    assert_eq!(clip_segment((-50.0, -5.0), (150.0, -5.0), 0.0, 100.0), None);
    assert_eq!(clip_segment((200.0, 0.0), (300.0, 100.0), 0.0, 100.0), None);
}

#[test]
fn low_zooms_keep_major_classes_only() {
    assert!(class_visible(RoadClass::Residential, 14));
    assert!(class_visible(RoadClass::Residential, 12));
    assert!(!class_visible(RoadClass::Service, 13));
    assert!(!class_visible(RoadClass::Tertiary, 11));
    assert!(class_visible(RoadClass::Motorway, 5));
    assert!(!class_visible(RoadClass::Primary, 9));
}

use super::*;
use crate::osm_change::{effective_changes, parse_osm_change};
use crate::test_util::{TempDir, apply_edits, osm_grid, osm_grid_edits, write_osc, write_pbf};
use crate::topology::build_topology_from_scan;
use netweevil_persist::{read_source_snapshot_bundle, write_source_snapshot_bundle};

fn topology_of(state: &OsmSourceState) -> (TopologyBundle, Vec<i64>) {
    let (scan, topology_node_ids) = state.scan_output();
    let (topology, _, _) =
        build_topology_from_scan(Path::new("grid.osm.pbf"), "abc", scan, &mut |_| {})
            .expect("grid topology builds");
    (topology, topology_node_ids)
}

#[test]
fn pbf_scan_retains_every_object_of_the_extract() {
    let dir = TempDir::new("source-scan");
    let path = dir.join("grid.osm.pbf");
    write_pbf(&path, &osm_grid());
    let size = std::fs::metadata(&path).expect("PBF written").len();

    let scanned = OsmSourceState::scan_pbf(&path, size, &mut |_| {}).expect("PBF scans");
    assert_eq!(scanned, OsmSourceState::from_objects(osm_grid()));
    assert_eq!(
        scanned.counts(),
        ObjectCounts {
            nodes: 68,
            ways: 17,
            relations: 2,
        }
    );
    assert_eq!(scanned.ways.len(), 16);
    assert_eq!(scanned.restrictions.len(), 1);
    assert_eq!(scanned.traffic_signals.len(), 2);
}

#[test]
fn applying_a_change_file_equals_scanning_the_edited_extract() {
    let edits = osm_grid_edits();
    let mut state = OsmSourceState::from_objects(osm_grid());
    let changes = parse_osm_change(write_osc(&edits).as_bytes()).expect("change parses");

    let summary = state.apply_changes(effective_changes(changes));

    assert_eq!(
        summary,
        ChangeSummary {
            nodes: 5,
            ways: 8,
            relations: 2,
        }
    );
    let edited = OsmSourceState::from_objects(apply_edits(&osm_grid(), &edits));
    assert_eq!(state, edited);
    assert_eq!(state.scan_output().0.counts, edited.scan_output().0.counts);
    // The building became a service road over nodes that were not part of
    // the topology before.
    assert!(state.ways.contains_key(&900));
    assert!(!state.other_ways.contains(&900));
    assert!(state.restrictions.contains_key(&501));
    assert!(state.other_relations.contains(&500));
}

#[test]
fn snapshot_restores_the_state_from_topology_coordinates() {
    let edits = osm_grid_edits();
    let state = OsmSourceState::from_objects(apply_edits(&osm_grid(), &edits));
    let (topology, topology_node_ids) = topology_of(&state);
    let snapshot = state.to_snapshot(CacheBundleId::new("topology-grid"), &topology_node_ids);
    // Only nodes outside the topology carry coordinates in the snapshot.
    assert_eq!(snapshot.topology_node_ids.len(), topology.nodes.len());
    assert_eq!(
        snapshot.topology_node_ids.len() + snapshot.other_node_ids.len(),
        state.nodes.len()
    );

    let dir = TempDir::new("source-snapshot");
    let path = dir.join("source.bin");
    write_source_snapshot_bundle(&path, &snapshot).expect("snapshot writes");
    let read: OsmSourceSnapshot = read_source_snapshot_bundle(&path).expect("snapshot reads");
    assert_eq!(read, snapshot);

    let restored = OsmSourceState::from_snapshot(read, &topology).expect("state restores");
    assert_eq!(restored, state);
}

#[test]
fn snapshot_rejects_the_topology_of_another_build() {
    let state = OsmSourceState::from_objects(osm_grid());
    let (topology, topology_node_ids) = topology_of(&state);
    let snapshot = state.to_snapshot(CacheBundleId::new("topology-grid"), &topology_node_ids);
    let edited = OsmSourceState::from_objects(apply_edits(&osm_grid(), &osm_grid_edits()));
    let (other_topology, _) = topology_of(&edited);
    assert_ne!(topology.nodes.len(), other_topology.nodes.len());

    assert!(OsmSourceState::from_snapshot(snapshot, &other_topology).is_err());
}

#[test]
fn decimicro_accepts_only_exact_osm_positions() {
    assert_eq!(decimicro(65_665_018_f64 * 1e-7), Some(65_665_018));
    assert_eq!(decimicro(-1_799_999_999_f64 * 1e-7), Some(-1_799_999_999));
    assert_eq!(decimicro(6.566_501_85), None);
    assert_eq!(decimicro(f64::NAN), None);
}

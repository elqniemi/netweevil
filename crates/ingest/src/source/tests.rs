use super::*;
use crate::osm_change::{effective_changes, parse_osm_change};
use crate::pbf::ObjectList;
use crate::test_util::{
    TempDir, apply_edits, grid_node, osm_grid, osm_grid_edits, write_osc, write_pbf,
};
use crate::topology::build_topology_from_scan;
use netweevil_persist::{read_source_snapshot_bundle, write_source_snapshot_bundle};

fn topology_of(state: &OsmSourceState) -> (TopologyBundle, Vec<i64>) {
    let (scan, topology_node_ids) = state.scan_output(true);
    let (topology, _, _) = build_topology_from_scan(Path::new("grid.osm.pbf"), scan, &mut |_| {})
        .expect("grid topology builds");
    (topology, topology_node_ids)
}

#[test]
fn pbf_scan_keeps_only_routable_objects_and_what_they_refer_to() {
    let dir = TempDir::new("source-scan");
    let path = dir.join("grid.osm.pbf");
    write_pbf(&path, &osm_grid());

    let scanned = OsmSourceState::scan_pbf(&path, &mut |_| {}).expect("PBF scans");
    assert!(scanned.base.is_some());
    assert_eq!(
        scanned.clone().without_changes(),
        OsmSourceState::from_objects(osm_grid())
    );
    assert_eq!(
        scanned.counts,
        ObjectCounts {
            nodes: 72,
            ways: 18,
            relations: 4,
        }
    );
    assert_eq!(scanned.ways.len(), 16);
    assert_eq!(scanned.restrictions.len(), 2);
    assert_eq!(scanned.traffic_signals.len(), 2);
    // The boundary way and its corners stay for the admin relation; the
    // building and its nodes are not referenced by anything retained.
    assert_eq!(
        scanned.member_ways.keys().copied().collect::<Vec<_>>(),
        [901]
    );
    assert_eq!(scanned.nodes.len(), 64 + 4);
    assert!(scanned.node(1000).is_none() && scanned.node(2000).is_some());
    assert!(scanned.absent_nodes.is_empty() && scanned.absent_member_ways.is_empty());
}

#[test]
fn scan_records_referenced_objects_the_extract_lacks() {
    // Cut the extract: drop one grid node and the boundary way.
    let objects = osm_grid()
        .into_iter()
        .filter(|object| match object {
            OsmObj::Node(node) => node.id.0 != grid_node(3, 3),
            OsmObj::Way(way) => way.id.0 != 901,
            OsmObj::Relation(_) => true,
        })
        .collect::<Vec<_>>();
    let state = OsmSourceState::from_objects(objects);
    assert_eq!(state.absent_nodes, [grid_node(3, 3)]);
    assert_eq!(state.absent_member_ways, [901]);
    assert!(state.member_ways.is_empty());
    let (scan, _) = state.scan_output(true);
    assert_eq!(scan.incomplete_admin_relations, [700]);
}

/// Applies the grid edits to the grid's state, reading what the edits refer
/// to from the grid extract.
fn edited_grid_state() -> (OsmSourceState, ChangeSummary) {
    let mut state = OsmSourceState::from_objects(osm_grid());
    let changes = parse_osm_change(write_osc(&osm_grid_edits()).as_bytes()).expect("change parses");
    let summary = state.apply_changes(effective_changes(changes));
    state
        .resolve(|| Ok(ObjectList(osm_grid())))
        .expect("changes resolve");
    (state, summary)
}

#[test]
fn applying_a_change_file_equals_scanning_the_edited_extract() {
    let (state, summary) = edited_grid_state();
    assert_eq!(
        summary,
        ChangeSummary {
            nodes: 7,
            ways: 11,
            relations: 2,
        }
    );
    let edited = OsmSourceState::from_objects(apply_edits(&osm_grid(), &osm_grid_edits()));
    assert_eq!(state.clone().without_changes(), edited);
    assert_eq!(
        state.scan_output(true).0.counts,
        edited.scan_output(true).0.counts
    );
    // The building became a service road over nodes read from the extract.
    assert!(state.ways.contains_key(&900));
    assert!(state.node(1000).is_some());
    assert!(state.restrictions.contains_key(&501));
    assert!(!state.restrictions.contains_key(&500));
    // Changed objects stay whether or not anything refers to them.
    assert_eq!(state.changed_ways.get(&205), Some(&None));
    assert_eq!(state.changed_nodes.get(&grid_node(7, 7)), Some(&None));
    assert!(state.changed_nodes.contains_key(&2002));
}

#[test]
fn resolving_reads_the_extract_only_for_objects_the_state_lacks() {
    let mut state = OsmSourceState::from_objects(osm_grid());
    let moved = parse_osm_change(
        write_osc(&[(
            ChangeAction::Modify,
            crate::test_util::osm_node(grid_node(1, 1), 65_010_123, 532_010_456, &[]),
        )])
        .as_bytes(),
    )
    .expect("change parses");
    state.apply_changes(effective_changes(moved));
    state
        .resolve(|| -> Result<ObjectList> { panic!("the extract is not needed") })
        .expect("changes resolve");
    assert_eq!(state.node(grid_node(1, 1)).unwrap().lon_e7, 65_010_123);

    let error = OsmSourceState::from_objects(osm_grid())
        .resolve_after(osm_grid_edits(), || -> Result<ObjectList> {
            anyhow::bail!("extract missing")
        })
        .expect_err("new references need the extract");
    assert!(error.to_string().contains("extract missing"), "{error}");
}

#[test]
fn snapshot_restores_the_state_from_topology_coordinates() {
    let (state, _) = edited_grid_state();
    let (topology, topology_node_ids) = topology_of(&state);
    let snapshot = state.to_snapshot(
        CacheBundleId::new("topology-grid"),
        &topology_node_ids,
        &EnrichmentInputs::default(),
    );
    // Only retained nodes outside the topology carry coordinates in the
    // snapshot.
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
    let snapshot = state.to_snapshot(
        CacheBundleId::new("topology-grid"),
        &topology_node_ids,
        &EnrichmentInputs::default(),
    );
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

#[test]
fn a_repeated_node_keeps_its_last_version_and_tags() {
    let signal = grid_node(2, 2);
    let mut objects = osm_grid();
    // A later, untagged and moved copy of a signal node, after the ways.
    objects.push(crate::test_util::osm_node(
        signal,
        65_020_500,
        532_020_500,
        &[],
    ));
    let state = OsmSourceState::from_objects(objects);
    assert_eq!(state.node(signal).unwrap().lon_e7, 65_020_500);
    assert!(!state.traffic_signals.contains(&signal));
    assert_eq!(state.traffic_signals.len(), 1);
    assert!(state.nodes.windows(2).all(|pair| pair[0].id < pair[1].id));
}

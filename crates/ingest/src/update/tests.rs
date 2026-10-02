use super::*;
use crate::osm_change::ChangeAction;
use crate::test_util::{
    GRID_SIDE, OsmEdit, TempDir, apply_edits, osm_grid, osm_grid_edits, write_osc, write_pbf,
};
use crate::{DatasetImportOptions, import_dataset};
use netweevil_persist::read_edge_name_bundle;
use netweevil_profile::{ProfileDocument, load_profile};
use netweevil_query::{PreparedRoutingEngine, RouteRequest};
use osmpbfreader::{OsmObj, OsmPbfReader};
use std::sync::Arc;

fn import(paths: &WorkspacePaths, source: &Path, name: &str) -> DatasetManifest {
    import_dataset(
        paths,
        &[source.to_path_buf()],
        DatasetImportOptions {
            name: name.to_string(),
            source: source.display().to_string(),
            format: None,
            mapping: None,
        },
        |_| {},
    )
    .expect("dataset imports")
}

fn update(
    paths: &WorkspacePaths,
    dataset: &str,
    input: DatasetUpdateInput,
    reorder: ReorderPolicy,
) -> DatasetUpdateReport {
    update_dataset(
        paths,
        DatasetUpdateOptions {
            dataset_id: dataset.to_string(),
            input,
            reorder,
        },
        |_| {},
    )
    .expect("dataset updates")
}

/// A base extract, its edited version and the change file between them,
/// written into `dir`.
struct UpdateFixture {
    base: PathBuf,
    edited: PathBuf,
    change: PathBuf,
}

fn write_fixture(dir: &TempDir, base: &[OsmObj], edits: &[OsmEdit]) -> UpdateFixture {
    let fixture = UpdateFixture {
        base: dir.join("base.osm.pbf"),
        edited: dir.join("edited.osm.pbf"),
        change: dir.join("day.osc"),
    };
    write_pbf(&fixture.base, base);
    write_pbf(&fixture.edited, &apply_edits(base, edits));
    std::fs::write(&fixture.change, write_osc(edits)).expect("change file writes");
    fixture
}

fn topology(manifest: &DatasetManifest) -> TopologyBundle {
    read_topology_bundle(&manifest.topology_bundle.as_ref().unwrap().path).expect("topology reads")
}

/// Asserts that two builds have the same topology bundle apart from the
/// source label and hash, and the same edge names and topology metadata.
fn assert_same_topology(dir: &TempDir, left: &DatasetManifest, right: &DatasetManifest) {
    let encoded = |manifest: &DatasetManifest, name: &str| {
        let mut bundle = topology(manifest);
        bundle.source_path.clear();
        bundle.source_sha256.clear();
        let path = dir.join(name);
        netweevil_persist::write_topology_bundle(&path, &bundle).expect("topology writes");
        std::fs::read(path).expect("topology reads back")
    };
    assert!(encoded(left, "left.bin") == encoded(right, "right.bin"));
    let names = |manifest: &DatasetManifest| {
        read_edge_name_bundle(&manifest.edge_name_bundle.as_ref().unwrap().path)
            .expect("edge names read")
            .names
    };
    assert_eq!(names(left), names(right));
    assert_eq!(
        serde_json::to_value(&left.topology_meta).unwrap(),
        serde_json::to_value(&right.topology_meta).unwrap()
    );
}

fn car_profile() -> ProfileDocument {
    load_profile(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/profiles/car_research_v1.yml"),
    )
    .expect("car profile loads")
}

/// The accelerated and the exact engine of one dataset build.
struct Engines {
    accelerated: PreparedRoutingEngine,
    exact: PreparedRoutingEngine,
}

fn engines(manifest: &DatasetManifest, profile: &ProfileDocument) -> Engines {
    let topology_ref = manifest.topology_bundle.as_ref().unwrap();
    let acceleration_ref = manifest.acceleration_bundle.as_ref().unwrap();
    let topology = Arc::new(topology(manifest));
    let acceleration =
        Arc::new(read_acceleration_bundle(&acceleration_ref.path).expect("CCH reads"));
    let accelerated_metrics = netweevil_profile::compile_profile_metrics_with_progress(
        profile,
        &topology,
        topology_ref.bundle_id.clone(),
        Some((&acceleration, acceleration_ref.bundle_id.clone())),
        |_| {},
    )
    .expect("profile compiles against the CCH");
    let exact_metrics = netweevil_profile::compile_profile_bundle(
        profile,
        &topology,
        topology_ref.bundle_id.clone(),
    )
    .expect("profile compiles");
    Engines {
        accelerated: PreparedRoutingEngine::new(
            Arc::clone(&topology),
            Arc::new(accelerated_metrics),
            Some(acceleration),
        )
        .expect("accelerated engine builds"),
        exact: PreparedRoutingEngine::new(topology, Arc::new(exact_metrics), None)
            .expect("exact engine builds"),
    }
}

type Point = (f64, f64);

/// Distance, travel time and cost of a route, `None` when unroutable.
fn route(
    engine: &PreparedRoutingEngine,
    origin: Point,
    destination: Point,
) -> Option<(u64, f64, f64)> {
    let request: RouteRequest = serde_json::from_value(serde_json::json!({
        "route_id": "update",
        "origin": {"id": "origin", "lon": origin.0, "lat": origin.1},
        "destination": {"id": "destination", "lon": destination.0, "lat": destination.1},
    }))
    .expect("route request parses");
    engine.execute_route(&request).ok().map(|route| {
        (
            route.summary.total_distance_m,
            route.summary.total_travel_time_s,
            route.summary.total_generalized_cost,
        )
    })
}

/// Routes every pair on both engines and asserts identical results;
/// returns the number of routable pairs.
fn assert_same_routes(
    left: &PreparedRoutingEngine,
    right: &PreparedRoutingEngine,
    pairs: &[(Point, Point)],
) -> usize {
    let mut routable = 0;
    for &(origin, destination) in pairs {
        let (a, b) = (
            route(left, origin, destination),
            route(right, origin, destination),
        );
        match (a, b) {
            (None, None) => {}
            (Some(a), Some(b)) => {
                routable += 1;
                assert_eq!(a.0, b.0, "distance {origin:?} -> {destination:?}");
                assert!(
                    (a.1 - b.1).abs() <= 1e-6,
                    "time {origin:?} -> {destination:?}: {a:?} {b:?}"
                );
                assert!(
                    (a.2 - b.2).abs() <= 1e-6,
                    "cost {origin:?} -> {destination:?}: {a:?} {b:?}"
                );
            }
            _ => panic!("routability differs {origin:?} -> {destination:?}: {a:?} {b:?}"),
        }
    }
    routable
}

/// A quarter of the ordered pairs of grid nodes and the new nodes, spread
/// so every point is an origin and a destination.
fn grid_pairs() -> Vec<(Point, Point)> {
    let position = |row: i64, col: i64| (6.5 + col as f64 * 0.001, 53.2 + row as f64 * 0.001);
    let mut points = (0..GRID_SIDE)
        .flat_map(|row| (0..GRID_SIDE).map(move |col| position(row, col)))
        .collect::<Vec<_>>();
    points.extend([(6.5015, 53.2015), (6.5025, 53.2025), (6.5125, 53.2125)]);
    let mut pairs = Vec::new();
    for (from, &origin) in points.iter().enumerate() {
        for (to, &destination) in points.iter().enumerate() {
            if (to + points.len() - from) % 4 == 1 {
                pairs.push((origin, destination));
            }
        }
    }
    pairs
}

#[test]
fn incremental_update_matches_a_full_import_of_the_edited_extract() {
    let dir = TempDir::new("update-grid");
    let paths = WorkspacePaths::discover(dir.path()).expect("workspace");
    let fixture = write_fixture(&dir, &osm_grid(), &osm_grid_edits());
    let imported = import(&paths, &fixture.base, "city");
    let full = import(&paths, &fixture.edited, "city_full");

    let report = update(
        &paths,
        "city",
        DatasetUpdateInput::OsmChanges(vec![fixture.change.clone()]),
        ReorderPolicy::Never,
    );
    let updated = report.manifest.clone();

    assert_same_topology(&dir, &updated, &full);
    // The retained objects match the full import's, so the next update
    // starts from the same source state.
    let snapshot = |manifest: &DatasetManifest| -> OsmSourceSnapshot {
        let mut snapshot =
            read_source_snapshot_bundle(&manifest.source_snapshot_bundle.as_ref().unwrap().path)
                .expect("snapshot reads");
        snapshot.source_topology_bundle_id = CacheBundleId::new("");
        snapshot
    };
    assert_eq!(snapshot(&updated), snapshot(&full));

    let record = updated.last_update.as_ref().expect("update recorded");
    assert_eq!(record.kind, "osm_change");
    assert!(!record.reordered);
    assert!(record.inserted_edge_states > 0);
    assert_eq!(
        record.reused_edge_states + record.inserted_edge_states,
        updated.topology_meta.as_ref().unwrap().edge_count
    );
    assert_eq!(
        report.changed_objects,
        Some(ChangedObjects {
            nodes: 5,
            ways: 8,
            relations: 2,
        })
    );
    assert_eq!(
        updated
            .acceleration_ordering
            .as_ref()
            .unwrap()
            .patched_updates,
        1
    );
    assert_ne!(updated.source_sha256, imported.source_sha256);
    assert_ne!(updated.topology_bundle, imported.topology_bundle);
    // The previous build's bundles stay in place for running readers.
    assert!(Path::new(&imported.topology_bundle.as_ref().unwrap().path).exists());

    let profile = car_profile();
    let incremental = engines(&updated, &profile);
    let reference = engines(&full, &profile);
    let pairs = grid_pairs();
    let routable = assert_same_routes(&incremental.accelerated, &reference.exact, &pairs);
    assert!(
        routable > pairs.len() / 2,
        "{routable} of {} pairs routable",
        pairs.len()
    );
    assert_same_routes(&incremental.accelerated, &reference.accelerated, &pairs);
    assert_same_routes(&incremental.exact, &reference.exact, &pairs);
}

#[test]
fn replacement_extract_and_reordering_rebuild_the_full_import() {
    let dir = TempDir::new("update-source");
    let paths = WorkspacePaths::discover(dir.path()).expect("workspace");
    let fixture = write_fixture(&dir, &osm_grid(), &osm_grid_edits());
    import(&paths, &fixture.base, "city");
    let full = import(&paths, &fixture.edited, "city_full");

    let report = update(
        &paths,
        "city",
        DatasetUpdateInput::Source(fixture.edited.clone()),
        ReorderPolicy::Always,
    );
    let updated = report.manifest;

    assert_same_topology(&dir, &updated, &full);
    assert_eq!(updated.source_sha256, full.source_sha256);
    assert_eq!(updated.source_path, fixture.edited.display().to_string());
    // Reordering the same topology repeats the full import's hierarchy.
    let record = updated.last_update.as_ref().unwrap();
    assert!(record.reordered);
    assert_eq!(updated.acceleration_stats, full.acceleration_stats);
    let ordering = updated.acceleration_ordering.as_ref().unwrap();
    assert_eq!(ordering.patched_updates, 0);
    assert_eq!(
        ordering.shortcut_arc_count,
        full.acceleration_stats.as_ref().unwrap().shortcut_arc_count
    );
    let acceleration = |manifest: &DatasetManifest| {
        read_acceleration_bundle(&manifest.acceleration_bundle.as_ref().unwrap().path)
            .expect("CCH reads")
    };
    assert_eq!(
        acceleration(&updated).edge_order,
        acceleration(&full).edge_order
    );

    let error = update_dataset(
        &paths,
        DatasetUpdateOptions {
            dataset_id: "city".to_string(),
            input: DatasetUpdateInput::Source(fixture.edited.clone()),
            reorder: ReorderPolicy::Never,
        },
        |_| {},
    )
    .expect_err("an unchanged extract is rejected");
    assert!(error.to_string().contains("already built"), "{error}");
}

#[test]
fn consecutive_change_files_chain_and_auto_reorder_on_fill_growth() {
    let dir = TempDir::new("update-chain");
    let paths = WorkspacePaths::discover(dir.path()).expect("workspace");
    let edits = osm_grid_edits();
    let (first, second) = edits.split_at(edits.len() / 2);
    let fixture = write_fixture(&dir, &osm_grid(), &edits);
    let first_change = dir.join("first.osc");
    let second_change = dir.join("second.osc");
    std::fs::write(&first_change, write_osc(first)).unwrap();
    std::fs::write(&second_change, write_osc(second)).unwrap();
    import(&paths, &fixture.base, "city");
    let full = import(&paths, &fixture.edited, "city_full");

    update(
        &paths,
        "city",
        DatasetUpdateInput::OsmChanges(vec![first_change.clone()]),
        ReorderPolicy::Never,
    );
    // Any fill growth above zero triggers the reorder.
    let report = update(
        &paths,
        "city",
        DatasetUpdateInput::OsmChanges(vec![second_change.clone()]),
        ReorderPolicy::Auto {
            max_fill_growth: 0.0,
        },
    );
    assert_same_topology(&dir, &report.manifest, &full);
    let record = report.manifest.last_update.as_ref().unwrap();
    assert!(record.reordered);
    assert_eq!(record.inputs, vec![second_change.display().to_string()]);
    assert_eq!(report.manifest.acceleration_stats, full.acceleration_stats);

    // One call with both files reaches the same source hash as two calls.
    import(&paths, &fixture.base, "city_both");
    let both = update(
        &paths,
        "city_both",
        DatasetUpdateInput::OsmChanges(vec![first_change, second_change]),
        ReorderPolicy::Never,
    );
    assert_eq!(both.manifest.source_sha256, report.manifest.source_sha256);
}

#[test]
fn updates_need_an_osm_dataset_with_a_source_snapshot() {
    let dir = TempDir::new("update-missing");
    let paths = WorkspacePaths::discover(dir.path()).expect("workspace");
    let fixture = write_fixture(&dir, &osm_grid(), &osm_grid_edits());
    let mut manifest = import(&paths, &fixture.base, "city");
    manifest.source_snapshot_bundle = None;
    netweevil_persist::write_dataset_manifest(&paths, &manifest).unwrap();

    let error = update_dataset(
        &paths,
        DatasetUpdateOptions {
            dataset_id: "city".to_string(),
            input: DatasetUpdateInput::OsmChanges(vec![fixture.change]),
            reorder: ReorderPolicy::Never,
        },
        |_| {},
    )
    .expect_err("a dataset without a snapshot cannot be updated");
    assert!(error.to_string().contains("re-import"), "{error}");
}

#[test]
fn previous_ranks_follow_edge_identity_through_way_splits() {
    let base = osm_grid();
    let edits = osm_grid_edits();
    let build = |objects: Vec<OsmObj>| {
        let state = OsmSourceState::from_objects(objects);
        let (scan, node_ids) = state.scan_output();
        let (topology, _, _) =
            build_topology_from_scan(Path::new("grid"), "abc", scan, &mut |_| {}).unwrap();
        (topology, node_ids)
    };
    let (before, before_ids) = build(base.clone());
    let (after, after_ids) = build(apply_edits(&base, &edits));
    let previous = PreviousBuild {
        topology_node_ids: before_ids.clone(),
        edges: before
            .edge_layers
            .routing
            .iter()
            .map(|edge| (edge.from.0, edge.to.0, edge.source_way_id))
            .collect(),
        // Rank = previous edge index makes the mapping visible.
        edge_rank: (0..before.edge_count() as u32).collect(),
    };

    let ranks = previous_ranks(&previous, &after, &after_ids);

    let osm_edge = |topology: &TopologyBundle, ids: &[i64], index: usize| {
        let edge = topology.routing_edge(index);
        (
            edge.source_way_id,
            ids[edge.from.0 as usize],
            ids[edge.to.0 as usize],
        )
    };
    for (index, rank) in ranks.iter().enumerate() {
        let (way, from, to) = osm_edge(&after, &after_ids, index);
        match rank {
            Some(rank) => {
                let (previous_way, previous_from, previous_to) =
                    osm_edge(&before, &before_ids, *rank as usize);
                assert_eq!((previous_from, previous_to), (from, to));
                // Only the split-off part of row 1 changes its way.
                if previous_way != way {
                    assert_eq!((previous_way, way), (101, 301));
                }
            }
            // New edges: the new road, the service road and the moved
            // building, nothing that existed before.
            None => assert!(
                [300, 900].contains(&way),
                "edge of way {way} from {from} to {to} lost its rank"
            ),
        }
    }
    assert!(ranks.iter().flatten().count() > before.edge_count() * 3 / 4);
}

/// Groningen with a realistic synthetic day of edits: incremental update
/// against a full import of the edited extract. Needs the routing extract
/// (`NETWEEVIL_GRONINGEN_PBF`, default `datasets/groningen-260508-routing.osm.pbf`);
/// run with `cargo test --release -p netweevil-ingest groningen -- --ignored --nocapture`.
#[test]
#[ignore = "needs the Groningen routing extract and a release build"]
fn groningen_incremental_update_matches_a_full_import() {
    let source = std::env::var_os("NETWEEVIL_GRONINGEN_PBF")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../datasets/groningen-260508-routing.osm.pbf")
        });
    assert!(source.exists(), "missing {}", source.display());
    let base = OsmPbfReader::new(std::fs::File::open(&source).unwrap())
        .iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("extract reads");
    let edits = groningen_edits(&base);
    let dir = TempDir::new("update-groningen");
    let paths = WorkspacePaths::discover(dir.path()).expect("workspace");
    let edited = dir.join("edited.osm.pbf");
    let change = dir.join("day.osc");
    write_pbf(&edited, &apply_edits(&base, &edits));
    std::fs::write(&change, write_osc(&edits)).unwrap();

    let started = Instant::now();
    import(&paths, &source, "groningen");
    let import_s = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let full = import(&paths, &edited, "groningen_full");
    let full_import_s = started.elapsed().as_secs_f64();
    let report = update(
        &paths,
        "groningen",
        DatasetUpdateInput::OsmChanges(vec![change]),
        ReorderPolicy::Never,
    );
    let updated = &report.manifest;
    let record = updated.last_update.as_ref().unwrap();
    eprintln!(
        "{} edits; import {import_s:.2} s, full import of the edited extract {full_import_s:.2} s, update {:.2} s ({:?})",
        edits.len(),
        record.elapsed_s,
        report.stage_seconds
    );
    eprintln!(
        "edge states reused {}, inserted {}; shortcuts patched {} vs fresh {} (fill growth {:.4})",
        record.reused_edge_states,
        record.inserted_edge_states,
        report.patched_shortcut_arc_count,
        full.acceleration_stats.as_ref().unwrap().shortcut_arc_count,
        record.fill_growth
    );

    assert_same_topology(&dir, updated, &full);
    let profile = car_profile();
    let incremental = engines(updated, &profile);
    let reference = engines(&full, &profile);
    let pairs = groningen_pairs(&base, &edits);
    let routable = assert_same_routes(&incremental.accelerated, &reference.exact, &pairs);
    assert_same_routes(&incremental.accelerated, &reference.accelerated, &pairs);
    eprintln!(
        "{} pairs agree, {routable} of them routable",
        pairs.len()
    );
    assert!(routable > pairs.len() / 2);
}

/// Deterministic edits spread over the extract: speed and class changes,
/// closures, way splits, moved nodes, new signals, new roads over new
/// nodes, and new and deleted turn restrictions.
fn groningen_edits(base: &[OsmObj]) -> Vec<OsmEdit> {
    use ChangeAction::{Create, Delete, Modify};
    let nodes = base
        .iter()
        .filter_map(|object| object.node())
        .map(|node| (node.id.0, node))
        .collect::<std::collections::HashMap<_, _>>();
    let roads = base
        .iter()
        .filter_map(|object| object.way())
        .filter(|way| {
            way.nodes.len() >= 4
                && matches!(
                    way.tags.get("highway").map(|value| value.as_str()),
                    Some("residential" | "tertiary" | "secondary" | "unclassified")
                )
        })
        .collect::<Vec<_>>();
    let mut next_node = base
        .iter()
        .filter_map(|o| o.node())
        .map(|n| n.id.0)
        .max()
        .unwrap()
        + 1;
    let mut next_way = base
        .iter()
        .filter_map(|o| o.way())
        .map(|w| w.id.0)
        .max()
        .unwrap()
        + 1;
    let mut next_relation = base
        .iter()
        .filter_map(|o| o.relation())
        .map(|r| r.id.0)
        .max()
        .unwrap()
        + 1;
    let mut edits = Vec::new();
    for (index, way) in roads.iter().enumerate() {
        let mut way = (*way).clone();
        match index % 211 {
            0 => {
                way.tags.insert("maxspeed".into(), "15".into());
                edits.push((Modify, OsmObj::Way(way)));
            }
            1 => {
                way.tags.insert("highway".into(), "primary".into());
                edits.push((Modify, OsmObj::Way(way)));
            }
            2 => edits.push((Delete, OsmObj::Way(way))),
            3 => {
                // Split at the middle node.
                let middle = way.nodes.len() / 2;
                let mut rest = way.clone();
                rest.id = osmpbfreader::WayId(next_way);
                next_way += 1;
                rest.nodes = way.nodes[middle..].to_vec();
                way.nodes.truncate(middle + 1);
                edits.push((Modify, OsmObj::Way(way)));
                edits.push((Create, OsmObj::Way(rest)));
            }
            4 => {
                // Move the middle node by about a metre.
                let mut node = nodes[&way.nodes[way.nodes.len() / 2].0].clone();
                node.decimicro_lat += 90;
                edits.push((Modify, OsmObj::Node(node)));
            }
            5 => {
                let mut node = nodes[&way.nodes[1].0].clone();
                node.tags.insert("highway".into(), "traffic_signals".into());
                edits.push((Modify, OsmObj::Node(node)));
            }
            6 => {
                // A new road from the way's start over two new nodes to its
                // third node.
                let start = nodes[&way.nodes[0].0];
                let end = way.nodes[2];
                let mut road = Vec::new();
                for step in 1..=2 {
                    edits.push((
                        Create,
                        osm_node_at(
                            next_node,
                            start.decimicro_lon + 300 * step,
                            start.decimicro_lat + 200 * step,
                        ),
                    ));
                    road.push(next_node);
                    next_node += 1;
                }
                let mut node_ids = vec![way.nodes[0].0];
                node_ids.extend(road);
                node_ids.push(end.0);
                edits.push((
                    Create,
                    crate::test_util::osm_way(next_way, &node_ids, &[("highway", "residential")]),
                ));
                next_way += 1;
            }
            7 => {
                // A no-U-turn restriction from the way onto itself at its
                // last node.
                let via = way.nodes[way.nodes.len() - 1].0;
                edits.push((
                    Create,
                    crate::test_util::osm_turn_restriction(
                        next_relation,
                        "no_u_turn",
                        way.id.0,
                        via,
                        way.id.0,
                    ),
                ));
                next_relation += 1;
            }
            _ => {}
        }
    }
    for (index, relation) in base.iter().filter_map(|o| o.relation()).enumerate() {
        if index % 97 == 0 {
            edits.push((Delete, OsmObj::Relation(relation.clone())));
        }
    }
    edits
}

fn osm_node_at(id: i64, lon_e7: i32, lat_e7: i32) -> OsmObj {
    crate::test_util::osm_node(id, lon_e7, lat_e7, &[])
}

/// Routes between edited places and across the extract.
fn groningen_pairs(base: &[OsmObj], edits: &[OsmEdit]) -> Vec<(Point, Point)> {
    let nodes = base
        .iter()
        .filter_map(|object| object.node())
        .map(|node| (node.id.0, (node.lon(), node.lat())))
        .collect::<std::collections::HashMap<_, _>>();
    let edited = edits
        .iter()
        .filter_map(|(_, object)| match object {
            OsmObj::Way(way) => way
                .nodes
                .first()
                .and_then(|node| nodes.get(&node.0))
                .copied(),
            OsmObj::Node(node) => nodes.get(&node.id.0).copied(),
            OsmObj::Relation(_) => None,
        })
        .collect::<Vec<_>>();
    let corpus = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/perf/groningen_routes.csv"),
    )
    .expect("Groningen corpus reads");
    let mut pairs = corpus
        .lines()
        .skip(1)
        .map(|line| {
            let fields = line
                .split(',')
                .skip(1)
                .map(|v| v.parse::<f64>().unwrap())
                .collect::<Vec<_>>();
            ((fields[0], fields[1]), (fields[2], fields[3]))
        })
        .collect::<Vec<_>>();
    for (index, &origin) in edited.iter().enumerate() {
        pairs.push((origin, edited[(index * 7 + 3) % edited.len()]));
    }
    pairs
}

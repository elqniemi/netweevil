use super::{Cell, build_dataset_acceleration_bundle_with_progress, inertial_flow_split};
use crate::test_util::{edge, topology};
use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, CCH_ALGORITHM, CacheBundleId, DatasetAccelerationBundle,
    TopologyBundle,
};

fn build(topology: &TopologyBundle) -> DatasetAccelerationBundle {
    build_dataset_acceleration_bundle_with_progress(
        topology,
        CacheBundleId::new("topology-test"),
        &mut |_| {},
    )
}

fn upward(bundle: &DatasetAccelerationBundle, edge: usize) -> &[u32] {
    &bundle.upward_head
        [bundle.upward_first_out[edge] as usize..bundle.upward_first_out[edge + 1] as usize]
}

fn downward(bundle: &DatasetAccelerationBundle, edge: usize) -> &[u32] {
    &bundle.downward_head
        [bundle.downward_first_out[edge] as usize..bundle.downward_first_out[edge + 1] as usize]
}

fn has_arc(bundle: &DatasetAccelerationBundle, from: usize, to: u32) -> bool {
    let row = if bundle.edge_rank[from] < bundle.edge_rank[to as usize] {
        upward(bundle, from)
    } else {
        downward(bundle, from)
    };
    row.binary_search(&to).is_ok()
}

fn line_topology() -> TopologyBundle {
    topology(
        &[
            (0.0, 0.0, 0.0),
            (1.0, 0.0, 0.0),
            (2.0, 0.0, 0.0),
            (3.0, 0.0, 0.0),
        ],
        &[edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 2, 3, 12)],
    )
}

/// Square grid with both travel directions between neighbouring nodes.
fn grid_topology(side: u32) -> TopologyBundle {
    let mut edges = Vec::new();
    let mut connect = |a: u32, b: u32| {
        for (from, to) in [(a, b), (b, a)] {
            let id = edges.len() as u32;
            edges.push(edge(id, from, to, 1000 + i64::from(id)));
        }
    };
    for row in 0..side {
        for col in 0..side {
            let node = row * side + col;
            if col + 1 < side {
                connect(node, node + 1);
            }
            if row + 1 < side {
                connect(node, node + side);
            }
        }
    }
    let nodes = (0..side * side)
        .map(|node| {
            (
                f64::from(node % side) * 0.001,
                f64::from(node / side) * 0.001,
                0.0,
            )
        })
        .collect::<Vec<_>>();
    topology(&nodes, &edges)
}

/// Grid cell of `side * side` nodes with unit spacing.
fn grid_cell(side: usize) -> Cell {
    let node_count = side * side;
    let mut first_out = vec![0];
    let mut heads = Vec::new();
    for node in 0..node_count {
        let (row, col) = (node / side, node % side);
        if row > 0 {
            heads.push((node - side) as u32);
        }
        if col > 0 {
            heads.push((node - 1) as u32);
        }
        if col + 1 < side {
            heads.push((node + 1) as u32);
        }
        if row + 1 < side {
            heads.push((node + side) as u32);
        }
        first_out.push(heads.len() as u32);
    }
    Cell {
        ids: (0..node_count as u32).collect(),
        first_out,
        heads,
        x: (0..node_count).map(|node| (node % side) as f32).collect(),
        y: (0..node_count).map(|node| (node / side) as f32).collect(),
    }
}

#[test]
fn builds_complete_cch_bundle_for_line_topology() {
    let bundle = build(&line_topology());

    assert_eq!(bundle.schema_version, ACCELERATION_BUNDLE_SCHEMA_VERSION);
    assert_eq!(bundle.algorithm, CCH_ALGORITHM);
    assert_eq!(bundle.stats.base_arc_count, 2);
    assert_eq!(
        bundle.stats.total_arc_count,
        bundle.stats.base_arc_count + bundle.stats.shortcut_arc_count
    );
    assert_eq!(bundle.edge_order.len(), 3);
    assert_eq!(bundle.edge_rank.len(), 3);
    assert_eq!(bundle.upward_first_out.len(), 4);
    assert_eq!(bundle.downward_first_out.len(), 4);
    assert_eq!(
        bundle.upward_head.len() + bundle.downward_head.len(),
        bundle.stats.total_arc_count as usize
    );
}

#[test]
fn edge_order_is_a_valid_elimination_order() {
    let topology = grid_topology(12);
    let edge_count = topology.edge_count();
    let bundle = build(&topology);

    // The order is a permutation of all edge states and the rank table
    // is exactly its inverse, so contraction visits every state once.
    let mut sorted = bundle.edge_order.clone();
    sorted.sort_unstable();
    assert!(sorted.into_iter().eq(0..edge_count as u32));
    for (rank, &edge_index) in bundle.edge_order.iter().enumerate() {
        assert_eq!(bundle.edge_rank[edge_index as usize], rank as u32);
    }
    // Arcs respect the order in both directions, and each row is sorted
    // by head so queries can binary-search it.
    for edge_index in 0..edge_count {
        let rank = bundle.edge_rank[edge_index];
        let up = upward(&bundle, edge_index);
        let down = downward(&bundle, edge_index);
        assert!(
            up.iter()
                .all(|&head| rank < bundle.edge_rank[head as usize])
        );
        assert!(
            down.iter()
                .all(|&head| rank > bundle.edge_rank[head as usize])
        );
        assert!(up.is_sorted_by(|a, b| a < b));
        assert!(down.is_sorted_by(|a, b| a < b));
    }
}

#[test]
fn inertial_flow_separates_a_grid() {
    let side = 24_usize;
    let cell = grid_cell(side);
    let split = inertial_flow_split(&cell).expect("grid admits an inertial flow split");

    // A square grid is cut by a single grid line.
    assert!(
        split.separator.len() <= side,
        "separator of {} nodes is larger than a grid line",
        split.separator.len()
    );
    assert!(!split.separator.is_empty());
    assert_eq!(
        split.side_a.len() + split.side_b.len() + split.separator.len(),
        cell.len()
    );
    // Both sides keep at least the flow source or sink set.
    assert!(split.side_a.len() * 4 >= cell.len());
    assert!(split.side_b.len() * 4 >= cell.len());

    let mut side_of = vec![0_u8; cell.len()];
    for &node in &split.side_a {
        side_of[node as usize] = 1;
    }
    for &node in &split.side_b {
        side_of[node as usize] = 2;
    }
    for node in 0..cell.len() {
        for &neighbour in cell.neighbours(node) {
            let (left, right) = (side_of[node], side_of[neighbour as usize]);
            assert!(
                left == 0 || right == 0 || left == right,
                "separator leaves an arc between the two sides"
            );
        }
    }
}

#[test]
fn edge_ordering_preserves_every_transition_including_uturns() {
    let topology = grid_topology(6);
    let bundle = build(&topology);
    let transitions = &topology.edge_based_topology;
    let mut uturns = 0;
    for edge in 0..topology.edge_count() {
        let from = topology.routing_edge(edge);
        let first_out = &transitions.edge_transition_first_out;
        for &next in &transitions.edge_transition_edges
            [first_out[edge] as usize..first_out[edge + 1] as usize]
        {
            let to = topology.routing_edge(next as usize);
            if from.from == to.to && from.to == to.from {
                uturns += 1;
            }
            assert!(has_arc(&bundle, edge, next));
        }
    }
    assert!(uturns > 0);
}

#[test]
fn contraction_is_complete_on_a_grid() {
    let topology = grid_topology(4);
    let edge_count = topology.edge_count();
    let bundle = build(&topology);

    // Reverse index of downward arcs: incoming higher-ranked tails per state.
    let mut incoming_from_higher: Vec<Vec<u32>> = vec![Vec::new(); edge_count];
    for tail in 0..edge_count {
        for &head in downward(&bundle, tail) {
            incoming_from_higher[head as usize].push(tail as u32);
        }
    }

    // Lower-triangle completeness: for every state v, every incoming arc
    // from a higher-ranked x and outgoing arc to a higher-ranked y must
    // be closed by an arc between x and y. This is the invariant that
    // makes CCH queries exact with no follow-up search.
    for (v, incoming) in incoming_from_higher.iter().enumerate() {
        for &x in incoming {
            for &y in upward(&bundle, v) {
                assert!(
                    x == y || has_arc(&bundle, x as usize, y),
                    "missing closure arc {x} -> {y} for contracted state {v}"
                );
            }
        }
    }
}

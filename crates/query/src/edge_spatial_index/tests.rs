use crate::tests::fixtures::{edge, topology, uniform_metrics};

/// The tree query returns exactly the edges a brute-force bbox test finds.
#[test]
fn edges_in_bbox_matches_brute_force() {
    let side = 6_u32;
    let nodes = (0..side * side)
        .map(|index| {
            (
                6.0 + f64::from(index % side) * 0.001,
                53.0 + f64::from(index / side) * 0.001,
            )
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    for index in 0..side * side {
        for neighbour in [index + 1, index + side] {
            let same_row = neighbour == index + side || neighbour % side != 0;
            if neighbour < side * side && same_row {
                for (from, to) in [(index, neighbour), (neighbour, index)] {
                    edges.push(edge(edges.len() as u32, from, to, i64::from(index), 70));
                }
            }
        }
    }
    let topology = topology("grid", &nodes, &edges);
    let metrics = uniform_metrics(topology.edge_count(), 5.0);
    let graph = crate::build_routing_graph(&topology, &metrics).unwrap();

    for bbox in [
        [6.0, 53.0, 6.0005, 53.0005],
        [6.0012, 53.0012, 6.0038, 53.0021],
        [5.0, 52.0, 7.0, 54.0],
        [6.0101, 53.0, 6.02, 53.01],
    ] {
        let expected = (0..topology.edge_count() as u32)
            .filter(|&index| {
                let edge = topology.routing_edge(index as usize);
                let (from, to) = (
                    &topology.nodes[edge.from.0 as usize],
                    &topology.nodes[edge.to.0 as usize],
                );
                from.lon.min(to.lon) <= bbox[2]
                    && from.lon.max(to.lon) >= bbox[0]
                    && from.lat.min(to.lat) <= bbox[3]
                    && from.lat.max(to.lat) >= bbox[1]
            })
            .collect::<Vec<_>>();
        assert_eq!(
            graph.edge_spatial_index.edges_in_bbox(&topology, bbox),
            expected,
            "bbox {bbox:?}"
        );
    }
}

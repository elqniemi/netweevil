use super::{EdgeSpatialIndex, routable_edge_set};
use crate::tests::fixtures::{edge, topology, uniform_metrics};

/// The tree query returns exactly the edges a brute-force bbox test finds.
#[test]
fn edges_in_bbox_matches_brute_force() {
    let topology = grid();
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

/// A 6 x 6 grid of 70 m two-way segments.
fn grid() -> netweevil_core::TopologyBundle {
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
    topology("grid", &nodes, &edges)
}

/// A tree over every edge restricted to a subset answers like a tree built
/// over the subset: the same bbox edges, and the same candidates once each
/// is tested against the query radius as snapping and avoid resolution do.
#[test]
fn restricted_tree_answers_like_a_tree_of_the_subset() {
    let topology = grid();
    let all = vec![1.0; topology.edge_count()];
    let subset = (0..topology.edge_count())
        .map(|edge| if edge % 3 == 0 { f64::INFINITY } else { 1.0 })
        .collect::<Vec<_>>();
    let full = EdgeSpatialIndex::build(&topology, &all);
    let restricted = full.restricted_to(routable_edge_set(&subset).into());
    let dedicated = EdgeSpatialIndex::build(&topology, &subset);

    for bbox in [
        [6.0, 53.0, 6.0005, 53.0005],
        [6.0012, 53.0012, 6.0038, 53.0021],
        [5.0, 52.0, 7.0, 54.0],
    ] {
        assert_eq!(
            restricted.edges_in_bbox(&topology, bbox),
            dedicated.edges_in_bbox(&topology, bbox)
        );
    }
    let within = |index: &EdgeSpatialIndex, lon: f64, lat: f64, radius_m: f64| {
        index
            .candidates(lon, lat, radius_m)
            .into_iter()
            .filter(|&edge| {
                let edge = topology.routing_edge(edge as usize);
                let from = &topology.nodes[edge.from.0 as usize];
                let to = &topology.nodes[edge.to.0 as usize];
                crate::avoid::point_segment_distance_m(lon, lat, from, to) <= radius_m
            })
            .collect::<Vec<_>>()
    };
    for (lon, lat) in [(6.0, 53.0), (6.0023, 53.0031), (6.0049, 53.0012)] {
        for radius_m in [5.0, 40.0, 120.0, 500.0] {
            let expected = within(&dedicated, lon, lat, radius_m);
            assert_eq!(within(&restricted, lon, lat, radius_m), expected);
            assert!(
                expected
                    .iter()
                    .all(|&edge| subset[edge as usize].is_finite())
            );
        }
    }
}

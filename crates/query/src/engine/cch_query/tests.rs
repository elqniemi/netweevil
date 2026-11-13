use super::*;
use crate::tests::fixtures::*;

#[test]
fn query_on_an_oversized_scratch_dirties_only_the_states_it_labels() {
    let mut random = 23_u64;
    let mut next = || {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        random >> 32
    };
    let nodes = (0..8)
        .map(|index| (6.0 + f64::from(index) * 0.001, 53.0))
        .collect::<Vec<_>>();
    let large_state_count = 1 << 18;
    let mut scratch = PairwiseCchScratch::default();
    for _ in 0..3 {
        let mut edges = Vec::new();
        let mut costs = Vec::new();
        for edge_index in 0..24 {
            let from = (next() % 8) as u32;
            let to = (from + 1 + (next() % 7) as u32) % 8;
            edges.push(edge(edge_index, from, to, 10, 100));
            costs.push((next() % 30) as f64);
        }
        let topology = topology("test", &nodes, &edges);
        let (bundle, metrics) = build_test_cch(&topology, &metrics(&costs));
        let graph = crate::build_routing_graph_from_shared(
            &topology,
            std::sync::Arc::new(metrics),
            Some(bundle),
        )
        .unwrap();
        let acceleration = graph.acceleration.as_ref().unwrap();
        for _ in 0..12 {
            let origins = [(next() as usize % 24, (next() % 30) as f64)];
            let destinations = [(next() as usize % 24, -((next() % 30) as f64))];

            scratch.prepare(large_state_count);
            assert_eq!(scratch.labels.len(), large_state_count);
            let path = pairwise_query(&mut scratch, acceleration, &origins, &destinations, None);
            assert!(!scratch.touched.is_empty());
            assert!(scratch.touched.len() <= 24);
            assert_eq!(scratch.labels.count_nonzero(), scratch.touched.len());

            let pooled =
                accelerated_route_query_seeded(&topology, &graph, &origins, &destinations, None)
                    .unwrap();
            let (_, _, recorded_cost) =
                recorded_hierarchy_search(&graph, &origins, &destinations, usize::MAX).unwrap();
            let forward = crate::build_forward_cch_space(&graph, &origins);
            let backward = crate::build_backward_cch_space(&graph, &destinations);
            let joined = crate::join_cch_spaces(&forward, &backward).map(|(cost, _)| cost);
            let cost = path.as_ref().map(|path| path.total_generalized_cost);
            assert_eq!(cost.map(f64::to_bits), joined.map(f64::to_bits));
            assert_eq!(cost.map(f64::to_bits), recorded_cost.map(f64::to_bits));
            assert_eq!(
                path.map(|path| path.edge_indexes),
                pooled.map(|path| path.edge_indexes)
            );
        }
    }
    scratch.prepare(large_state_count);
    assert_eq!(scratch.labels.count_nonzero(), 0);
}

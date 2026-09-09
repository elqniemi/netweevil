//! A prepared edge index for snapping, including the interiors of long edges.

use netweevil_core::{TopologyBundle, geo::EARTH_RADIUS_M};

const LEAF_SIZE: usize = 16;
const METERS_PER_DEGREE: f64 = EARTH_RADIUS_M * std::f64::consts::PI / 180.0;

struct Node {
    // Longitude minimum, latitude minimum, longitude maximum, latitude maximum.
    bounds: [f64; 4],
    longitude_scale: f64,
    // A branch has count == 0 and first identifies its left child. Its right
    // child follows immediately. Leaves refer to a range in edge_ids.
    first: u32,
    count: u32,
}

#[derive(Default)]
pub(crate) struct EdgeSpatialIndex {
    nodes: Vec<Node>,
    edge_ids: Vec<u32>,
}

impl EdgeSpatialIndex {
    pub(crate) fn build(topology: &TopologyBundle, edge_costs: &[f64]) -> Self {
        let edge_ids = edge_costs
            .iter()
            .enumerate()
            .filter(|(_, cost)| cost.is_finite())
            .map(|(edge, _)| edge as u32)
            .collect::<Vec<_>>();
        if edge_ids.is_empty() {
            return Self::default();
        }
        let mut index = Self {
            nodes: vec![Node {
                bounds: [0.0; 4],
                longitude_scale: 0.0,
                first: 0,
                count: 0,
            }],
            edge_ids,
        };
        index.build_node(topology, 0, 0, index.edge_ids.len());
        index
    }

    fn build_node(&mut self, topology: &TopologyBundle, slot: usize, start: usize, end: usize) {
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for &edge_id in &self.edge_ids[start..end] {
            let edge = topology.routing_edge(edge_id as usize);
            for node in [
                &topology.nodes[edge.from.0 as usize],
                &topology.nodes[edge.to.0 as usize],
            ] {
                bounds[0] = bounds[0].min(node.lon);
                bounds[1] = bounds[1].min(node.lat);
                bounds[2] = bounds[2].max(node.lon);
                bounds[3] = bounds[3].max(node.lat);
            }
        }
        // This is no larger than any member edge's projection scale. Thus
        // distance to the box is a lower bound for the snapper's local metric,
        // including long edges spanning different latitudes.
        let longitude_scale = bounds[1]
            .abs()
            .max(bounds[3].abs())
            .to_radians()
            .cos()
            .max(0.0)
            * METERS_PER_DEGREE;
        self.nodes[slot] = Node {
            bounds,
            longitude_scale,
            first: start as u32,
            count: (end - start) as u32,
        };
        if end - start <= LEAF_SIZE {
            return;
        }
        let longitude_axis = (bounds[2] - bounds[0]) * longitude_scale
            >= (bounds[3] - bounds[1]) * METERS_PER_DEGREE;
        let midpoint = start + (end - start) / 2;
        self.edge_ids[start..end].select_nth_unstable_by(midpoint - start, |&left, &right| {
            let center = |edge_id| {
                let edge = topology.routing_edge(edge_id as usize);
                let from = &topology.nodes[edge.from.0 as usize];
                let to = &topology.nodes[edge.to.0 as usize];
                if longitude_axis {
                    from.lon + to.lon
                } else {
                    from.lat + to.lat
                }
            };
            center(left)
                .total_cmp(&center(right))
                .then_with(|| left.cmp(&right))
        });
        let first_child = self.nodes.len();
        for _ in 0..2 {
            self.nodes.push(Node {
                bounds: [0.0; 4],
                longitude_scale: 0.0,
                first: 0,
                count: 0,
            });
        }
        self.nodes[slot].first = first_child as u32;
        self.nodes[slot].count = 0;
        self.build_node(topology, first_child, start, midpoint);
        self.build_node(topology, first_child + 1, midpoint, end);
    }

    pub(crate) fn candidates(&self, lon: f64, lat: f64, radius_m: f64) -> Vec<u32> {
        let mut edges = Vec::new();
        if self.nodes.is_empty() {
            return edges;
        }
        // The balanced tree over u32 edge IDs is at most 29 levels deep.
        let mut stack = [0_u32; 64];
        let mut length = 1;
        let radius_squared = radius_m * radius_m;
        while length != 0 {
            length -= 1;
            let node = &self.nodes[stack[length] as usize];
            let dx = (lon - lon.clamp(node.bounds[0], node.bounds[2])) * node.longitude_scale;
            let dy = (lat - lat.clamp(node.bounds[1], node.bounds[3])) * METERS_PER_DEGREE;
            if dx * dx + dy * dy > radius_squared {
                continue;
            }
            if node.count != 0 {
                edges.extend_from_slice(
                    &self.edge_ids[node.first as usize..(node.first + node.count) as usize],
                );
            } else {
                stack[length] = node.first;
                stack[length + 1] = node.first + 1;
                length += 2;
            }
        }
        // Candidate order must not change equal-distance snapping tie breaks.
        edges.sort_unstable();
        edges
    }
}

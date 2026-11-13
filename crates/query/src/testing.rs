//! Small street networks for tests in other crates (feature
//! `test-fixtures`): the fixtures of this crate's own tests.

#[cfg(not(test))]
#[allow(dead_code, unused_imports)]
#[path = "tests/fixtures.rs"]
mod fixtures;
#[cfg(test)]
use crate::tests::fixtures;

pub use netweevil_core::{DirectedEdge, TopologyBundle};

use netweevil_core::CompiledProfileBundle;

/// A residential car edge of `length_m` from node `from` to node `to`.
pub fn edge(id: u32, from: u32, to: u32, way_id: i64, length_m: u32) -> DirectedEdge {
    fixtures::edge(id, from, to, way_id, length_m)
}

/// A topology over `nodes` (`(lon, lat)`, ids in order) and `edges`, with a
/// spatial index, the edge-based graph and weak components.
pub fn topology(nodes: &[(f64, f64)], edges: &[DirectedEdge]) -> TopologyBundle {
    fixtures::topology("test", nodes, edges)
}

/// Metrics with the given travel time (and cost) per edge, in edge order.
pub fn metrics(costs: &[f64]) -> CompiledProfileBundle {
    fixtures::metrics(costs)
}

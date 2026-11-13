//! Network density per cell.

use std::collections::BTreeMap;

use h3o::{CellIndex, Resolution};
use netweevil_core::TopologyBundle;
use serde::Serialize;

use crate::cell_at;
use crate::io::serialize_cell;

/// Nodes and directed edges of a topology in one cell. An edge counts in
/// the cell of its midpoint.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct NetworkCellStat {
    #[serde(serialize_with = "serialize_cell")]
    pub cell: CellIndex,
    pub node_count: u32,
    pub edge_count: u32,
    /// Summed length of the directed edges in metres.
    pub edge_length_m: f64,
}

/// Midpoint of an edge between its end nodes as `(lon, lat)`.
pub fn edge_midpoint(topology: &TopologyBundle, edge_index: usize) -> (f64, f64) {
    let edge = topology.routing_edge(edge_index);
    let from = &topology.nodes[edge.from.0 as usize];
    let to = &topology.nodes[edge.to.0 as usize];
    ((from.lon() + to.lon()) / 2.0, (from.lat() + to.lat()) / 2.0)
}

/// Counts nodes and directed edges per cell, ordered by cell.
pub fn summarize_network(
    topology: &TopologyBundle,
    resolution: Resolution,
) -> Vec<NetworkCellStat> {
    let mut cells = BTreeMap::<CellIndex, NetworkCellStat>::new();
    fn stat(
        cells: &mut BTreeMap<CellIndex, NetworkCellStat>,
        cell: CellIndex,
    ) -> &mut NetworkCellStat {
        cells.entry(cell).or_insert(NetworkCellStat {
            cell,
            node_count: 0,
            edge_count: 0,
            edge_length_m: 0.0,
        })
    }
    for node in &topology.nodes {
        if let Some(cell) = cell_at(node.lon(), node.lat(), resolution) {
            stat(&mut cells, cell).node_count += 1;
        }
    }
    for edge_index in 0..topology.edge_count() {
        let (lon, lat) = edge_midpoint(topology, edge_index);
        if let Some(cell) = cell_at(lon, lat, resolution) {
            let entry = stat(&mut cells, cell);
            entry.edge_count += 1;
            entry.edge_length_m += f64::from(topology.routing_edge(edge_index).length_m);
        }
    }
    cells.into_values().collect()
}

//! Nested-dissection ordering of the edge states by inertial flow.

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use anyhow::Result;
use netweevil_core::TopologyBundle;
use rayon::prelude::*;

use super::flow::{FlowGraph, flow_split};
use crate::topology::checked_u32;

/// Node count below which a cell is ordered directly by minimum degree.
pub(super) const LEAF_NODES: usize = 32;
/// Fraction of a cell placed in the flow source set and in the sink set.
const FLOW_SIDE_FRACTION: f64 = 0.25;
/// Cell node roles for one inertial-flow direction.
pub(super) const ROLE_INNER: u8 = 0;
pub(super) const ROLE_SOURCE: u8 = 1;
pub(super) const ROLE_SINK: u8 = 2;
/// Projection directions tried per cell; the smallest separator wins.
const FLOW_DIRECTIONS: [(f32, f32); 4] = [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, -1.0)];

/// Undirected working graph for one nested-dissection cell.
///
/// `ids` maps a local index back to a topology node id, adjacency rows hold
/// sorted local ids, and `x`/`y` are equirectangular projections of the node
/// coordinates so that projecting onto a direction is metric.
#[derive(Clone)]
pub(super) struct Cell {
    pub(super) ids: Vec<u32>,
    pub(super) first_out: Vec<u32>,
    pub(super) heads: Vec<u32>,
    pub(super) x: Vec<f32>,
    pub(super) y: Vec<f32>,
}

impl Cell {
    pub(super) fn len(&self) -> usize {
        self.ids.len()
    }

    pub(super) fn neighbours(&self, node: usize) -> &[u32] {
        &self.heads[self.first_out[node] as usize..self.first_out[node + 1] as usize]
    }

    /// Builds the subgraph induced by `members`, a sorted list of local ids.
    pub(super) fn induced(&self, members: &[u32]) -> Cell {
        let mut mapping = vec![u32::MAX; self.len()];
        for (local, &member) in members.iter().enumerate() {
            mapping[member as usize] = local as u32;
        }
        let mut ids = Vec::with_capacity(members.len());
        let mut x = Vec::with_capacity(members.len());
        let mut y = Vec::with_capacity(members.len());
        let mut first_out = Vec::with_capacity(members.len() + 1);
        let mut heads = Vec::new();
        first_out.push(0);
        for &member in members {
            let member = member as usize;
            ids.push(self.ids[member]);
            x.push(self.x[member]);
            y.push(self.y[member]);
            for &neighbour in self.neighbours(member) {
                let mapped = mapping[neighbour as usize];
                if mapped != u32::MAX {
                    heads.push(mapped);
                }
            }
            first_out.push(heads.len() as u32);
        }
        Cell {
            ids,
            first_out,
            heads,
            x,
            y,
        }
    }
}

/// A cell partitioned into two sides that share no arc, plus the vertex
/// separator removed to keep them apart.
pub(super) struct Split {
    pub(super) side_a: Vec<u32>,
    pub(super) side_b: Vec<u32>,
    pub(super) separator: Vec<u32>,
}

/// The undirected transition graph is used only to choose elimination ranks.
/// Contraction retains every directed transition, including legal u-turns.
pub(super) fn build_edge_cell(topology: &TopologyBundle) -> Result<Cell> {
    let edge_count = topology.edge_count();
    let transitions = &topology.edge_based_topology;
    let mut pairs = Vec::with_capacity(transitions.edge_transition_edges.len() * 2);
    if transitions.edge_transition_first_out.len() == edge_count + 1 {
        for edge in 0..edge_count {
            let start = transitions.edge_transition_first_out[edge] as usize;
            let end = transitions.edge_transition_first_out[edge + 1] as usize;
            for &next in &transitions.edge_transition_edges[start..end] {
                if next as usize != edge && (next as usize) < edge_count {
                    pairs.push((edge as u32, next));
                    pairs.push((next, edge as u32));
                }
            }
        }
    }
    pairs.par_sort_unstable();
    pairs.dedup();
    let (first_out, heads) = arcs_to_csr(&pairs, edge_count, "undirected edge transitions")?;
    drop(pairs);
    let mean_lat = if topology.nodes.is_empty() {
        0.0
    } else {
        topology.nodes.iter().map(|node| node.lat()).sum::<f64>() / topology.nodes.len() as f64
    };
    let longitude_scale = mean_lat.to_radians().cos().abs().max(0.05);
    let mut x = Vec::with_capacity(edge_count);
    let mut y = Vec::with_capacity(edge_count);
    for index in 0..edge_count {
        let edge = topology.routing_edge(index);
        let from = &topology.nodes[edge.from.0 as usize];
        let to = &topology.nodes[edge.to.0 as usize];
        x.push(((from.lon() + to.lon()) * 0.5 * longitude_scale) as f32);
        y.push(((from.lat() + to.lat()) * 0.5) as f32);
    }
    Ok(Cell {
        ids: (0..edge_count as u32).collect(),
        first_out,
        heads,
        x,
        y,
    })
}

/// Splits a cell into two size-balanced groups of whole connected
/// components. Returns `None` when the cell is connected.
fn split_components(cell: &Cell) -> Option<(Vec<u32>, Vec<u32>)> {
    let node_count = cell.len();
    let mut component = vec![u32::MAX; node_count];
    let mut sizes = Vec::<u32>::new();
    let mut stack = Vec::<u32>::new();
    for root in 0..node_count {
        if component[root] != u32::MAX {
            continue;
        }
        let label = sizes.len() as u32;
        let mut size = 0_u32;
        component[root] = label;
        stack.push(root as u32);
        while let Some(node) = stack.pop() {
            size += 1;
            for &neighbour in cell.neighbours(node as usize) {
                if component[neighbour as usize] == u32::MAX {
                    component[neighbour as usize] = label;
                    stack.push(neighbour);
                }
            }
        }
        sizes.push(size);
    }
    if sizes.len() < 2 {
        return None;
    }

    // Largest component first into the currently lighter group.
    let mut labels = (0..sizes.len() as u32).collect::<Vec<_>>();
    labels.sort_unstable_by_key(|&label| (std::cmp::Reverse(sizes[label as usize]), label));
    let mut group = vec![0_u8; sizes.len()];
    let mut load = [0_u64; 2];
    for &label in &labels {
        let target = usize::from(load[1] < load[0]);
        group[label as usize] = target as u8;
        load[target] += u64::from(sizes[label as usize]);
    }

    let mut left = Vec::new();
    let mut right = Vec::new();
    for (node, &label) in component.iter().enumerate() {
        if group[label as usize] == 0 {
            left.push(node as u32);
        } else {
            right.push(node as u32);
        }
    }
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some((left, right))
}

/// The best separator found so far among the projection directions of one
/// cell, shared by directions running in parallel.
///
/// Holds `(size << 8) | direction`, so the winner is the smallest separator
/// and, among equal sizes, the first direction, whatever order the
/// directions finish in.
pub(super) struct BestCut(AtomicU64);

impl BestCut {
    pub(super) fn new() -> Self {
        Self(AtomicU64::new(u64::MAX))
    }

    fn key(size: u32, direction: usize) -> u64 {
        (u64::from(size) << 8) | direction as u64
    }

    fn offer(&self, size: u32, direction: usize) {
        self.0
            .fetch_min(Self::key(size, direction), AtomicOrdering::Relaxed);
    }
}

/// Lets one direction's max flow stop as soon as it cannot beat the best
/// separator of the other directions.
#[derive(Clone, Copy)]
pub(super) struct CutBound<'a> {
    pub(super) best: &'a BestCut,
    pub(super) direction: usize,
}

impl CutBound<'_> {
    /// Whether a cut of at least `flow` nodes can still win.
    pub(super) fn admits(self, flow: u32) -> bool {
        BestCut::key(flow, self.direction) < self.best.0.load(AtomicOrdering::Relaxed)
    }
}

/// Projects the cell onto `FLOW_DIRECTIONS[direction]` and marks the
/// `side` lowest nodes as sources and the `side` highest as sinks, ties
/// broken by topology id.
pub(super) fn direction_roles(cell: &Cell, direction: usize, side: usize) -> (Vec<f32>, Vec<u8>) {
    let node_count = cell.len();
    let (dx, dy) = FLOW_DIRECTIONS[direction];
    let projection = (0..node_count)
        .map(|node| dx * cell.x[node] + dy * cell.y[node])
        .collect::<Vec<_>>();
    let mut order = (0..node_count as u32).collect::<Vec<_>>();
    order.sort_unstable_by(|&left, &right| {
        projection[left as usize]
            .total_cmp(&projection[right as usize])
            .then_with(|| cell.ids[left as usize].cmp(&cell.ids[right as usize]))
    });
    let mut role = vec![ROLE_INNER; node_count];
    for &node in &order[..side] {
        role[node as usize] = ROLE_SOURCE;
    }
    for &node in &order[node_count - side..] {
        role[node as usize] = ROLE_SINK;
    }
    (projection, role)
}

/// Node count from which the projection directions of a cell run in
/// parallel.
const PARALLEL_DIRECTION_NODES: usize = 4096;

/// Chooses the smallest inertial-flow separator over the projection
/// directions, ties going to the earlier direction. Each side keeps at least
/// [`FLOW_SIDE_FRACTION`] of the cell because source and sink nodes have
/// infinite vertex capacity and therefore never enter the separator.
pub(super) fn inertial_flow_split(cell: &Cell) -> Result<Option<Split>> {
    let node_count = cell.len();
    let side = (node_count as f64 * FLOW_SIDE_FRACTION) as usize;
    if side == 0 || 2 * side >= node_count {
        return Ok(None);
    }

    let graph = FlowGraph::new(cell)?;
    let best = BestCut::new();
    let try_direction = |direction: usize| {
        let (projection, role) = direction_roles(cell, direction, side);
        let bound = CutBound {
            best: &best,
            direction,
        };
        let split = flow_split(&graph, &role, &projection, bound)?;
        best.offer(split.separator.len() as u32, direction);
        Some((direction, split))
    };
    let splits = if node_count >= PARALLEL_DIRECTION_NODES {
        (0..FLOW_DIRECTIONS.len())
            .into_par_iter()
            .filter_map(try_direction)
            .collect::<Vec<_>>()
    } else {
        (0..FLOW_DIRECTIONS.len())
            .filter_map(try_direction)
            .collect::<Vec<_>>()
    };
    Ok(splits
        .into_iter()
        .min_by_key(|(direction, split)| BestCut::key(split.separator.len() as u32, *direction))
        .map(|(_, split)| split))
}

/// Splits a cell at the median of its widest coordinate axis and keeps the
/// boundary of the lower half as the separator.
fn coordinate_split(cell: &Cell) -> Split {
    let node_count = cell.len();
    let extent = |values: &[f32]| {
        values
            .iter()
            .copied()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |bounds, value| {
                (bounds.0.min(value), bounds.1.max(value))
            })
    };
    let (min_x, max_x) = extent(&cell.x);
    let (min_y, max_y) = extent(&cell.y);
    let axis = if (max_x - min_x) >= (max_y - min_y) {
        &cell.x
    } else {
        &cell.y
    };

    let mut order = (0..node_count as u32).collect::<Vec<_>>();
    order.sort_unstable_by(|&left, &right| {
        axis[left as usize]
            .total_cmp(&axis[right as usize])
            .then_with(|| cell.ids[left as usize].cmp(&cell.ids[right as usize]))
    });
    let mut lower = vec![false; node_count];
    for &node in &order[..node_count / 2] {
        lower[node as usize] = true;
    }

    let mut side_a = Vec::new();
    let mut side_b = Vec::new();
    let mut separator = Vec::new();
    for (node, &in_lower) in lower.iter().enumerate() {
        if !in_lower {
            side_b.push(node as u32);
        } else if cell
            .neighbours(node)
            .iter()
            .any(|&neighbour| !lower[neighbour as usize])
        {
            separator.push(node as u32);
        } else {
            side_a.push(node as u32);
        }
    }
    Split {
        side_a,
        side_b,
        separator,
    }
}

/// Orders a small cell by repeatedly eliminating a node of minimum degree in
/// the fill-in graph.
fn minimum_degree_order(cell: &Cell) -> Vec<u32> {
    fn insert_sorted(row: &mut Vec<u32>, value: u32) {
        if let Err(position) = row.binary_search(&value) {
            row.insert(position, value);
        }
    }

    let node_count = cell.len();
    let mut adjacency = (0..node_count)
        .map(|node| cell.neighbours(node).to_vec())
        .collect::<Vec<_>>();
    let mut queue = std::collections::BinaryHeap::with_capacity(node_count);
    for (node, row) in adjacency.iter().enumerate() {
        queue.push(std::cmp::Reverse((row.len() as u32, node as u32)));
    }
    let mut eliminated = vec![false; node_count];
    let mut order = Vec::with_capacity(node_count);

    while let Some(std::cmp::Reverse((degree, node))) = queue.pop() {
        let node = node as usize;
        if eliminated[node] || adjacency[node].len() as u32 != degree {
            continue;
        }
        eliminated[node] = true;
        order.push(cell.ids[node]);
        let neighbours = std::mem::take(&mut adjacency[node]);
        for (position, &left) in neighbours.iter().enumerate() {
            let row = &mut adjacency[left as usize];
            if let Ok(slot) = row.binary_search(&(node as u32)) {
                row.remove(slot);
            }
            for &right in &neighbours[position + 1..] {
                insert_sorted(&mut adjacency[left as usize], right);
                insert_sorted(&mut adjacency[right as usize], left);
            }
        }
        for &neighbour in &neighbours {
            queue.push(std::cmp::Reverse((
                adjacency[neighbour as usize].len() as u32,
                neighbour,
            )));
        }
    }
    order
}

/// Produces a nested-dissection elimination order for one cell: both sides
/// first, then the separator that keeps them apart.
pub(super) fn dissect(cell: Cell, leaf_nodes: usize) -> Result<Vec<u32>> {
    let node_count = cell.len();
    if node_count == 0 {
        return Ok(Vec::new());
    }
    if node_count <= leaf_nodes {
        return Ok(minimum_degree_order(&cell));
    }

    if let Some((left, right)) = split_components(&cell) {
        let left_cell = cell.induced(&left);
        let right_cell = cell.induced(&right);
        drop(cell);
        let (left_order, right_order) = join_if_large(
            node_count,
            || dissect(left_cell, leaf_nodes),
            || dissect(right_cell, leaf_nodes),
        );
        let mut order = left_order?;
        order.append(&mut right_order?);
        return Ok(order);
    }

    let split = match inertial_flow_split(&cell)? {
        Some(split) => split,
        None => coordinate_split(&cell),
    };
    let side_a = cell.induced(&split.side_a);
    let side_b = cell.induced(&split.side_b);
    let separator = cell.induced(&split.separator);
    drop(cell);
    let ((a_order, b_order), separator_order) = join_if_large(
        node_count,
        || {
            join_if_large(
                node_count,
                || dissect(side_a, leaf_nodes),
                || dissect(side_b, leaf_nodes),
            )
        },
        || dissect(separator, leaf_nodes),
    );
    let mut order = a_order?;
    order.append(&mut b_order?);
    order.append(&mut separator_order?);
    Ok(order)
}

/// Node count from which the parts of a dissected cell are ordered in
/// parallel.
const PARALLEL_DISSECT_NODES: usize = 2048;

/// Runs `left` and `right` in parallel for a cell of at least
/// [`PARALLEL_DISSECT_NODES`] nodes, one after the other otherwise.
fn join_if_large<A: Send, B: Send>(
    node_count: usize,
    left: impl FnOnce() -> A + Send,
    right: impl FnOnce() -> B + Send,
) -> (A, B) {
    if node_count >= PARALLEL_DISSECT_NODES {
        rayon::join(left, right)
    } else {
        (left(), right())
    }
}

/// CSR offsets and heads of arcs sorted by tail. `what` names the arcs in
/// the error for more arcs than 32-bit offsets address.
fn arcs_to_csr(
    sorted_arcs: &[(u32, u32)],
    edge_count: usize,
    what: &str,
) -> Result<(Vec<u32>, Vec<u32>)> {
    checked_u32(sorted_arcs.len(), what)?;
    let mut first_out = vec![0_u32; edge_count + 1];
    let mut heads = vec![0_u32; sorted_arcs.len()];
    for (slot, &(tail, head)) in sorted_arcs.iter().enumerate() {
        first_out[tail as usize + 1] += 1;
        heads[slot] = head;
    }
    for edge_index in 0..edge_count {
        first_out[edge_index + 1] += first_out[edge_index];
    }
    Ok((first_out, heads))
}

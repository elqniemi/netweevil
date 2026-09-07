use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, AccelerationBundleStats, CCH_ALGORITHM, CacheBundleId,
    DatasetAccelerationBundle, TopologyBundle,
};
use rayon::prelude::*;

use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress};

#[cfg(test)]
pub(crate) fn build_dataset_acceleration_bundle(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
) -> DatasetAccelerationBundle {
    build_dataset_acceleration_bundle_with_progress(
        topology,
        source_topology_bundle_id,
        &mut |_| {},
    )
}

/// Builds the metric-independent CCH topology for a dataset.
///
/// Edge states are ordered by nested dissection (separators last), then
/// contracted in that order via the elimination game: contracting state `v`
/// inserts a shortcut `x -> y` for every pair of arcs `x -> v` and `v -> y`
/// whose other endpoints outrank `v`. The insertion is complete (no budgets),
/// which is what makes hierarchy queries exact without any follow-up search
/// on the base graph.
pub(crate) fn build_dataset_acceleration_bundle_with_progress(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> DatasetAccelerationBundle {
    let edge_count = topology.edge_count();

    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(0.0),
        format!("Ordering {edge_count} edge states by nested dissection"),
    );

    let edge_order = dissect(build_edge_cell(topology), LEAF_NODES);

    build_with_order(topology, source_topology_bundle_id, edge_order, progress)
}

fn build_with_order(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    edge_order: Vec<u32>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> DatasetAccelerationBundle {
    let edge_count = topology.edge_count();
    let transition_topology = &topology.edge_based_topology;
    let has_transitions = transition_topology.edge_transition_first_out.len() == edge_count + 1;
    let mut edge_rank = vec![0_u32; edge_count];
    for (rank, &edge_index) in edge_order.iter().enumerate() {
        edge_rank[edge_index as usize] = rank as u32;
    }

    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(0.0),
        format!("Building CCH 0% ({edge_count} edge states)"),
    );

    // Pending adjacency: every arc is owned by its lower-ranked endpoint and
    // consumed when that state is contracted. Each pending list is a sorted
    // vector of `(other_endpoint << 1) | outgoing` keys, so duplicate arcs
    // (multi-edges, shortcut pairs proposed by several middles) dedup with a
    // binary search per insertion instead of a global hash set over every
    // arc, and arcs are emitted straight into the upward/downward stores
    // instead of a third arc table.
    let mut pending: Vec<Vec<u64>> = vec![Vec::new(); edge_count];
    let mut upward = Vec::<(u32, u32)>::new();
    let mut downward = Vec::<(u32, u32)>::new();
    let mut base_arc_count = 0_u64;
    let mut shortcut_arc_count = 0_u64;

    fn insert_pending(pending: &mut [Vec<u64>], owner: usize, other: u32, outgoing: bool) -> bool {
        let key = ((other as u64) << 1) | outgoing as u64;
        let list = &mut pending[owner];
        match list.binary_search(&key) {
            Ok(_) => false,
            Err(position) => {
                list.insert(position, key);
                true
            }
        }
    }

    if has_transitions {
        for edge_index in 0..edge_count {
            let start = transition_topology.edge_transition_first_out[edge_index] as usize;
            let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
            for &next_edge in &transition_topology.edge_transition_edges[start..end] {
                if next_edge as usize == edge_index || next_edge as usize >= edge_count {
                    continue;
                }
                let (owner, other, outgoing) =
                    if edge_rank[edge_index] < edge_rank[next_edge as usize] {
                        (edge_index, next_edge, true)
                    } else {
                        (next_edge as usize, edge_index as u32, false)
                    };
                if insert_pending(&mut pending, owner, other, outgoing) {
                    base_arc_count += 1;
                }
            }
        }
    }

    let mut reporter = PercentReporter::starting_at_zero();
    let mut incoming = Vec::<u32>::new();
    let mut outgoing = Vec::<u32>::new();
    for (order_index, &contracted_edge) in edge_order.iter().enumerate() {
        let contracted_edge = contracted_edge as usize;
        let owned = std::mem::take(&mut pending[contracted_edge]);
        incoming.clear();
        outgoing.clear();
        for &key in &owned {
            let other = (key >> 1) as u32;
            if key & 1 == 1 {
                outgoing.push(other);
            } else {
                incoming.push(other);
            }
        }
        drop(owned);

        // The contracted state is the lower endpoint of everything it owns:
        // `contracted -> y` is an upward arc, `x -> contracted` a downward
        // arc. Emitting here visits every arc exactly once.
        for &head in &outgoing {
            upward.push((contracted_edge as u32, head));
        }
        for &tail in &incoming {
            downward.push((tail, contracted_edge as u32));
        }

        for &tail in &incoming {
            for &head in &outgoing {
                if tail == head {
                    continue;
                }
                let (owner, other, is_outgoing) =
                    if edge_rank[tail as usize] < edge_rank[head as usize] {
                        (tail as usize, head, true)
                    } else {
                        (head as usize, tail, false)
                    };
                if insert_pending(&mut pending, owner, other, is_outgoing) {
                    shortcut_arc_count += 1;
                }
            }
        }
        reporter.emit_if_needed(
            (order_index + 1) as u64,
            edge_order.len() as u64,
            DatasetImportStage::BuildAcceleration,
            progress,
            |percent| {
                format!(
                    "Building CCH {percent:.0}% ({}/{}) with {} arcs ({} base, {} shortcuts)",
                    order_index + 1,
                    edge_order.len(),
                    base_arc_count + shortcut_arc_count,
                    base_arc_count,
                    shortcut_arc_count
                )
            },
        );
    }
    drop(pending);

    let total_arc_count = base_arc_count + shortcut_arc_count;
    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(100.0),
        format!(
            "Building CCH 100% ({} arcs: {} base, {} shortcuts)",
            total_arc_count, base_arc_count, shortcut_arc_count
        ),
    );

    let stats = AccelerationBundleStats {
        base_arc_count,
        shortcut_arc_count,
        total_arc_count,
    };

    // Rows sorted by head id enable binary-search arc lookup during
    // customization and unpacking.
    upward.par_sort_unstable();
    downward.par_sort_unstable();

    let (upward_first_out, upward_head) = arcs_to_csr(&upward, edge_count);
    let (downward_first_out, downward_head) = arcs_to_csr(&downward, edge_count);

    DatasetAccelerationBundle {
        schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id,
        algorithm: CCH_ALGORITHM.to_string(),
        stats,
        edge_order,
        edge_rank,
        upward_first_out,
        upward_head,
        downward_first_out,
        downward_head,
    }
}

fn arcs_to_csr(sorted_arcs: &[(u32, u32)], edge_count: usize) -> (Vec<u32>, Vec<u32>) {
    let mut first_out = vec![0_u32; edge_count + 1];
    let mut heads = vec![0_u32; sorted_arcs.len()];
    for (slot, &(tail, head)) in sorted_arcs.iter().enumerate() {
        first_out[tail as usize + 1] += 1;
        heads[slot] = head;
    }
    for edge_index in 0..edge_count {
        first_out[edge_index + 1] += first_out[edge_index];
    }
    (first_out, heads)
}

/// Node count below which a cell is ordered directly by minimum degree.
const LEAF_NODES: usize = 32;
/// Fraction of a cell placed in the flow source set and in the sink set.
const FLOW_SIDE_FRACTION: f64 = 0.25;
/// Capacity marking arcs that must never appear in a minimum cut.
const INFINITE_CAPACITY: u8 = u8::MAX;
/// Flow node id of the contracted source set.
const FLOW_SOURCE: usize = 0;
/// Flow node id of the contracted sink set.
const FLOW_SINK: usize = 1;
/// Cell node roles for one inertial-flow direction.
const ROLE_INNER: u8 = 0;
const ROLE_SOURCE: u8 = 1;
const ROLE_SINK: u8 = 2;
/// Projection directions tried per cell; the smallest separator wins.
const FLOW_DIRECTIONS: [(f32, f32); 4] = [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, -1.0)];

/// Undirected working graph for one nested-dissection cell.
///
/// `ids` maps a local index back to a topology node id, adjacency rows hold
/// sorted local ids, and `x`/`y` are equirectangular projections of the node
/// coordinates so that projecting onto a direction is metric.
#[derive(Clone)]
struct Cell {
    ids: Vec<u32>,
    first_out: Vec<u32>,
    heads: Vec<u32>,
    x: Vec<f32>,
    y: Vec<f32>,
}

impl Cell {
    fn len(&self) -> usize {
        self.ids.len()
    }

    fn neighbours(&self, node: usize) -> &[u32] {
        &self.heads[self.first_out[node] as usize..self.first_out[node + 1] as usize]
    }

    /// Builds the subgraph induced by `members`, a sorted list of local ids.
    fn induced(&self, members: &[u32]) -> Cell {
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
struct Split {
    side_a: Vec<u32>,
    side_b: Vec<u32>,
    separator: Vec<u32>,
}

/// The undirected transition graph is used only to choose elimination ranks.
/// Contraction retains every directed transition, including legal u-turns.
fn build_edge_cell(topology: &TopologyBundle) -> Cell {
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
    let (first_out, heads) = arcs_to_csr(&pairs, edge_count);
    drop(pairs);
    let mean_lat = if topology.nodes.is_empty() {
        0.0
    } else {
        topology.nodes.iter().map(|node| node.lat).sum::<f64>() / topology.nodes.len() as f64
    };
    let longitude_scale = mean_lat.to_radians().cos().abs().max(0.05);
    let mut x = Vec::with_capacity(edge_count);
    let mut y = Vec::with_capacity(edge_count);
    for index in 0..edge_count {
        let edge = topology.routing_edge(index);
        let from = &topology.nodes[edge.from.0 as usize];
        let to = &topology.nodes[edge.to.0 as usize];
        x.push(((from.lon + to.lon) * 0.5 * longitude_scale) as f32);
        y.push(((from.lat + to.lat) * 0.5) as f32);
    }
    Cell {
        ids: (0..edge_count as u32).collect(),
        first_out,
        heads,
        x,
        y,
    }
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

/// Enumerates the arc pairs of the vertex-capacity flow network for one
/// source/sink assignment, calling `visit(tail, head, capacity)` once per
/// pair of mutually reverse arcs.
///
/// Inner nodes are split into an entry node `2 + 2 * local` and an exit node
/// `3 + 2 * local` joined by a unit arc, so every finite minimum cut is a
/// vertex separator. Cell arcs run from exits to entries with infinite
/// capacity. The source and sink sets are contracted into [`FLOW_SOURCE`] and
/// [`FLOW_SINK`]. Returns `false` when a source node touches a sink node, in
/// which case no vertex separator exists for this assignment.
fn visit_flow_arcs(cell: &Cell, role: &[u8], mut visit: impl FnMut(usize, usize, u8)) -> bool {
    for node in 0..cell.len() {
        if role[node] == ROLE_INNER {
            visit(2 + 2 * node, 3 + 2 * node, 1);
        }
        for &neighbour in cell.neighbours(node) {
            let neighbour = neighbour as usize;
            if neighbour <= node {
                continue;
            }
            match (role[node], role[neighbour]) {
                (ROLE_SOURCE, ROLE_SINK) | (ROLE_SINK, ROLE_SOURCE) => return false,
                (ROLE_INNER, ROLE_INNER) => {
                    visit(3 + 2 * node, 2 + 2 * neighbour, INFINITE_CAPACITY);
                    visit(3 + 2 * neighbour, 2 + 2 * node, INFINITE_CAPACITY);
                }
                (ROLE_SOURCE, ROLE_INNER) => {
                    visit(FLOW_SOURCE, 2 + 2 * neighbour, INFINITE_CAPACITY)
                }
                (ROLE_INNER, ROLE_SOURCE) => visit(FLOW_SOURCE, 2 + 2 * node, INFINITE_CAPACITY),
                (ROLE_SINK, ROLE_INNER) => visit(3 + 2 * neighbour, FLOW_SINK, INFINITE_CAPACITY),
                (ROLE_INNER, ROLE_SINK) => visit(3 + 2 * node, FLOW_SINK, INFINITE_CAPACITY),
                _ => {}
            }
        }
    }
    true
}

/// Residual network in CSR form; arc `arc` is undone by arc `rev[arc]`.
struct FlowNetwork {
    first_out: Vec<u32>,
    head: Vec<u32>,
    cap: Vec<u8>,
    rev: Vec<u32>,
}

fn build_flow_network(cell: &Cell, role: &[u8]) -> Option<FlowNetwork> {
    let flow_nodes = 2 + 2 * cell.len();
    let mut first_out = vec![0_u32; flow_nodes + 1];
    let mut arc_count = 0_usize;
    let feasible = visit_flow_arcs(cell, role, |tail, target, _| {
        first_out[tail + 1] += 1;
        first_out[target + 1] += 1;
        arc_count += 2;
    });
    if !feasible {
        return None;
    }
    for node in 0..flow_nodes {
        first_out[node + 1] += first_out[node];
    }

    let mut cursor = first_out[..flow_nodes].to_vec();
    let mut head = vec![0_u32; arc_count];
    let mut cap = vec![0_u8; arc_count];
    let mut rev = vec![0_u32; arc_count];
    visit_flow_arcs(cell, role, |tail, target, capacity| {
        let forward = cursor[tail] as usize;
        cursor[tail] += 1;
        let backward = cursor[target] as usize;
        cursor[target] += 1;
        head[forward] = target as u32;
        cap[forward] = capacity;
        rev[forward] = backward as u32;
        head[backward] = tail as u32;
        cap[backward] = 0;
        rev[backward] = forward as u32;
    });

    Some(FlowNetwork {
        first_out,
        head,
        cap,
        rev,
    })
}

/// Runs Dinic's algorithm and returns the residual BFS levels of the final
/// phase, in which the non-negative entries are exactly the source side of
/// the minimum cut.
///
/// Returns `None` once the flow reaches `limit`: the minimum cut is at least
/// the flow pushed so far, so the cut can no longer beat `limit`.
fn max_flow(network: &mut FlowNetwork, limit: u32) -> Option<Vec<i32>> {
    if limit == 0 {
        return None;
    }
    let flow_nodes = network.first_out.len() - 1;
    let mut level = vec![-1_i32; flow_nodes];
    let mut queue = Vec::<u32>::with_capacity(flow_nodes);
    let mut iter = vec![0_u32; flow_nodes];
    let mut path_arcs = Vec::<u32>::new();
    let mut path_tails = Vec::<u32>::new();
    let mut flow = 0_u32;

    loop {
        level.iter_mut().for_each(|entry| *entry = -1);
        queue.clear();
        level[FLOW_SOURCE] = 0;
        queue.push(FLOW_SOURCE as u32);
        let mut read = 0;
        while read < queue.len() {
            let node = queue[read] as usize;
            read += 1;
            for arc in network.first_out[node] as usize..network.first_out[node + 1] as usize {
                if network.cap[arc] == 0 {
                    continue;
                }
                let target = network.head[arc] as usize;
                if level[target] < 0 {
                    level[target] = level[node] + 1;
                    queue.push(target as u32);
                }
            }
        }
        if level[FLOW_SINK] < 0 {
            return Some(level);
        }

        iter.copy_from_slice(&network.first_out[..flow_nodes]);
        path_arcs.clear();
        path_tails.clear();
        let mut node = FLOW_SOURCE;
        loop {
            if node == FLOW_SINK {
                // Every source-to-sink path leaves an entry node through an
                // arc of residual capacity one, so a unit push always
                // saturates at least one arc of the path.
                for &arc in &path_arcs {
                    let arc = arc as usize;
                    if network.cap[arc] != INFINITE_CAPACITY {
                        network.cap[arc] -= 1;
                    }
                    let back = network.rev[arc] as usize;
                    if network.cap[back] != INFINITE_CAPACITY {
                        network.cap[back] += 1;
                    }
                }
                flow += 1;
                if flow >= limit {
                    return None;
                }
                let saturated = path_arcs
                    .iter()
                    .position(|&arc| network.cap[arc as usize] == 0)
                    .unwrap_or(0);
                node = path_tails[saturated] as usize;
                path_arcs.truncate(saturated);
                path_tails.truncate(saturated);
                continue;
            }

            let mut advanced = false;
            while iter[node] < network.first_out[node + 1] {
                let arc = iter[node] as usize;
                let target = network.head[arc] as usize;
                if network.cap[arc] > 0 && level[target] == level[node] + 1 {
                    path_arcs.push(arc as u32);
                    path_tails.push(node as u32);
                    node = target;
                    advanced = true;
                    break;
                }
                iter[node] += 1;
            }
            if advanced {
                continue;
            }
            level[node] = -1;
            if path_arcs.pop().is_some() {
                let tail = path_tails.pop().expect("arc and tail stacks stay aligned");
                iter[tail as usize] += 1;
                node = tail as usize;
            } else {
                break;
            }
        }
    }
}

/// Splits a cell along one source/sink assignment with a minimum vertex cut.
fn flow_split(cell: &Cell, role: &[u8], limit: u32) -> Option<Split> {
    let mut network = build_flow_network(cell, role)?;
    let level = max_flow(&mut network, limit)?;
    drop(network);

    let mut side_a = Vec::new();
    let mut side_b = Vec::new();
    let mut separator = Vec::new();
    for (node, &node_role) in role.iter().enumerate() {
        match node_role {
            ROLE_SOURCE => side_a.push(node as u32),
            ROLE_SINK => side_b.push(node as u32),
            _ if level[3 + 2 * node] >= 0 => side_a.push(node as u32),
            _ if level[2 + 2 * node] >= 0 => separator.push(node as u32),
            _ => side_b.push(node as u32),
        }
    }
    Some(Split {
        side_a,
        side_b,
        separator,
    })
}

/// Chooses the smallest inertial-flow separator over the projection
/// directions. Each side keeps at least [`FLOW_SIDE_FRACTION`] of the cell
/// because source and sink nodes have infinite vertex capacity and therefore
/// never enter the separator.
fn inertial_flow_split(cell: &Cell) -> Option<Split> {
    let node_count = cell.len();
    let side = (node_count as f64 * FLOW_SIDE_FRACTION) as usize;
    if side == 0 || 2 * side >= node_count {
        return None;
    }

    let mut order = (0..node_count as u32).collect::<Vec<_>>();
    let mut best: Option<Split> = None;
    let mut best_size = u32::MAX;
    for &(dx, dy) in FLOW_DIRECTIONS.iter() {
        let projection = (0..node_count)
            .map(|node| dx * cell.x[node] + dy * cell.y[node])
            .collect::<Vec<_>>();
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

        let Some(split) = flow_split(cell, &role, best_size) else {
            continue;
        };
        let size = split.separator.len() as u32;
        if size < best_size {
            best_size = size;
            best = Some(split);
        }
        if best_size == 0 {
            break;
        }
    }
    best
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
fn dissect(cell: Cell, leaf_nodes: usize) -> Vec<u32> {
    let node_count = cell.len();
    if node_count == 0 {
        return Vec::new();
    }
    if node_count <= leaf_nodes {
        return minimum_degree_order(&cell);
    }

    if let Some((left, right)) = split_components(&cell) {
        let left_cell = cell.induced(&left);
        let right_cell = cell.induced(&right);
        drop(cell);
        let (mut left_order, mut right_order) = rayon::join(
            || dissect(left_cell, leaf_nodes),
            || dissect(right_cell, leaf_nodes),
        );
        left_order.append(&mut right_order);
        return left_order;
    }

    let split = inertial_flow_split(&cell).unwrap_or_else(|| coordinate_split(&cell));
    let side_a = cell.induced(&split.side_a);
    let side_b = cell.induced(&split.side_b);
    let separator = cell.induced(&split.separator);
    drop(cell);
    let ((mut a_order, mut b_order), mut separator_order) = rayon::join(
        || {
            rayon::join(
                || dissect(side_a, leaf_nodes),
                || dissect(side_b, leaf_nodes),
            )
        },
        || dissect(separator, leaf_nodes),
    );
    a_order.append(&mut b_order);
    a_order.append(&mut separator_order);
    a_order
}

#[cfg(test)]
mod tests {
    use super::{Cell, build_dataset_acceleration_bundle, inertial_flow_split};
    use crate::test_util::edge;
    use crate::topology::build_edge_based_topology;
    use netweevil_core::{
        ACCELERATION_BUNDLE_SCHEMA_VERSION, CCH_ALGORITHM, CacheBundleId, NodeId, TopologyBundle,
        TopologyNode,
    };

    #[test]
    #[ignore = "requires NETWEEVIL_ORDERING_TOPOLOGY pointing to a local topology bundle"]
    fn measure_edge_ordering() {
        let path = std::env::var("NETWEEVIL_ORDERING_TOPOLOGY").expect("topology path");
        let mut topology = netweevil_persist::read_topology_bundle(path).expect("read topology");
        let leaves =
            std::env::var("NETWEEVIL_ORDERING_LEAVES").unwrap_or("16,32,64,128,256".into());
        let transitions = &topology.edge_based_topology;
        let mut uturns = 0usize;
        for edge in 0..topology.edge_count() {
            let from = topology.routing_edge(edge);
            for &next in &transitions.edge_transition_edges[transitions.edge_transition_first_out
                [edge] as usize
                ..transitions.edge_transition_first_out[edge + 1] as usize]
            {
                let to = topology.routing_edge(next as usize);
                if from.from == to.to && from.to == to.from {
                    uturns += 1;
                }
            }
        }
        eprintln!(
            "edge_states={} transitions={} uturns={uturns}",
            topology.edge_count(),
            transitions.edge_transition_edges.len()
        );
        // Diagnostic only: this altered graph must never be used for routing.
        if std::env::var("NETWEEVIL_ORDERING_OMIT_UTURNS").as_deref() == Ok("1") {
            let mut first_out = vec![0u32];
            let mut heads = Vec::new();
            for edge in 0..topology.edge_count() {
                let from = topology.routing_edge(edge);
                for &next in &transitions.edge_transition_edges[transitions
                    .edge_transition_first_out[edge]
                    as usize
                    ..transitions.edge_transition_first_out[edge + 1] as usize]
                {
                    let to = topology.routing_edge(next as usize);
                    if from.from != to.to || from.to != to.from {
                        heads.push(next);
                    }
                }
                first_out.push(heads.len() as u32);
            }
            topology.edge_based_topology.edge_transition_first_out = first_out;
            topology.edge_based_topology.edge_transition_edges = heads;
            eprintln!("diagnostic graph omits u-turns; routing semantics are not preserved");
        }
        if let Ok(path) = std::env::var("NETWEEVIL_ORDERING_FIXED_ACCELERATION") {
            let acceleration =
                netweevil_persist::read_acceleration_bundle(path).expect("read acceleration");
            let baseline_shortcuts = acceleration.stats.shortcut_arc_count;
            let order = acceleration.edge_order.clone();
            drop(acceleration);
            let start = std::time::Instant::now();
            let bundle = super::build_with_order(
                &topology,
                CacheBundleId::new("uturn-diagnostic"),
                order,
                &mut |_| {},
            );
            eprintln!(
                "fixed_order baseline_shortcuts={baseline_shortcuts} base={} shortcuts={} total={} contraction_s={:.3}",
                bundle.stats.base_arc_count,
                bundle.stats.shortcut_arc_count,
                bundle.stats.total_arc_count,
                start.elapsed().as_secs_f64()
            );
            return;
        }
        let leaves = leaves
            .split(',')
            .map(|leaf| leaf.parse::<usize>().expect("leaf size").max(1))
            .collect::<Vec<_>>();
        let start = std::time::Instant::now();
        let cells = experiment_cells(
            super::build_edge_cell(&topology),
            *leaves.iter().max().expect("leaf sizes"),
        );
        let partition_s = start.elapsed().as_secs_f64();
        eprintln!("partition_s={partition_s:.3} cells={}", cells.len());
        for leaf in leaves {
            let start = std::time::Instant::now();
            use rayon::prelude::*;
            let order = cells
                .par_iter()
                .flat_map_iter(|cell| super::dissect(cell.clone(), leaf))
                .collect();
            let ordering_s = partition_s + start.elapsed().as_secs_f64();
            eprintln!("leaf={leaf} ordering_s={ordering_s:.3}; contracting");
            let bundle = super::build_with_order(
                &topology,
                CacheBundleId::new("ordering-experiment"),
                order,
                &mut |_| {},
            );
            eprintln!(
                "leaf={leaf} ordering_s={ordering_s:.3} total_s={:.3} base={} shortcuts={} total={}",
                partition_s + start.elapsed().as_secs_f64(),
                bundle.stats.base_arc_count,
                bundle.stats.shortcut_arc_count,
                bundle.stats.total_arc_count
            );
        }
    }

    // Share the expensive large-cell flow cuts across the leaf-size sweep.
    fn experiment_cells(cell: Cell, cutoff: usize) -> Vec<Cell> {
        if cell.len() <= cutoff {
            return vec![cell];
        }
        if let Some((left, right)) = super::split_components(&cell) {
            let left = cell.induced(&left);
            let right = cell.induced(&right);
            drop(cell);
            let (mut a, mut b) = rayon::join(
                || experiment_cells(left, cutoff),
                || experiment_cells(right, cutoff),
            );
            a.append(&mut b);
            return a;
        }
        let split =
            super::inertial_flow_split(&cell).unwrap_or_else(|| super::coordinate_split(&cell));
        let left = cell.induced(&split.side_a);
        let right = cell.induced(&split.side_b);
        let separator = cell.induced(&split.separator);
        drop(cell);
        let ((mut a, mut b), mut separator) = rayon::join(
            || {
                rayon::join(
                    || experiment_cells(left, cutoff),
                    || experiment_cells(right, cutoff),
                )
            },
            || experiment_cells(separator, cutoff),
        );
        a.append(&mut b);
        a.append(&mut separator);
        a
    }

    fn line_topology() -> TopologyBundle {
        let edges = vec![edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 2, 3, 12)];
        TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    lon: 0.0,
                    lat: 0.0,
                    z: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    lon: 1.0,
                    lat: 0.0,
                    z: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    lon: 2.0,
                    lat: 0.0,
                    z: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    lon: 3.0,
                    lat: 0.0,
                    z: 0.0,
                },
            ],
            edge_layers: netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: build_edge_based_topology(4, &edges),
            spatial_index: None,
            node_component_ids: vec![0, 0, 0, 0],
            edge_component_ids: vec![0, 0, 0, 0],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
        }
    }

    /// Square grid with both travel directions between neighbouring nodes.
    fn grid_topology(side: u32) -> TopologyBundle {
        let mut edges = Vec::new();
        let mut edge_id = 0_u32;
        let mut connect = |a: u32, b: u32, edges: &mut Vec<netweevil_core::DirectedEdge>| {
            edges.push(edge(edge_id, a, b, 1000 + edge_id as i64));
            edge_id += 1;
            edges.push(edge(edge_id, b, a, 1000 + edge_id as i64));
            edge_id += 1;
        };
        for row in 0..side {
            for col in 0..side {
                let node = row * side + col;
                if col + 1 < side {
                    connect(node, node + 1, &mut edges);
                }
                if row + 1 < side {
                    connect(node, node + side, &mut edges);
                }
            }
        }
        let nodes = (0..side * side)
            .map(|node| TopologyNode {
                node_id: NodeId(node),
                lon: (node % side) as f64 * 0.001,
                lat: (node / side) as f64 * 0.001,
                z: 0.0,
            })
            .collect::<Vec<_>>();
        let node_count = nodes.len();
        let edge_count = edges.len();
        TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes,
            edge_layers: netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: build_edge_based_topology(node_count, &edges),
            spatial_index: None,
            node_component_ids: vec![0; node_count],
            edge_component_ids: vec![0; edge_count],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
        }
    }

    /// Grid cell of `side * side` nodes with unit spacing.
    fn grid_cell(side: usize) -> Cell {
        let node_count = side * side;
        let mut first_out = Vec::with_capacity(node_count + 1);
        let mut heads = Vec::new();
        first_out.push(0);
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
        let topology = line_topology();
        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));

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
        // Every rank pairing must be strictly monotone per direction.
        for edge_index in 0..3_usize {
            for slot in bundle.upward_first_out[edge_index] as usize
                ..bundle.upward_first_out[edge_index + 1] as usize
            {
                let head = bundle.upward_head[slot] as usize;
                assert!(bundle.edge_rank[edge_index] < bundle.edge_rank[head]);
            }
            for slot in bundle.downward_first_out[edge_index] as usize
                ..bundle.downward_first_out[edge_index + 1] as usize
            {
                let head = bundle.downward_head[slot] as usize;
                assert!(bundle.edge_rank[edge_index] > bundle.edge_rank[head]);
            }
        }
    }

    #[test]
    fn cch_rows_are_sorted_by_head_for_binary_search() {
        let topology = line_topology();
        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));
        for edge_index in 0..3_usize {
            let row = &bundle.upward_head[bundle.upward_first_out[edge_index] as usize
                ..bundle.upward_first_out[edge_index + 1] as usize];
            assert!(row.windows(2).all(|pair| pair[0] < pair[1]));
            let row = &bundle.downward_head[bundle.downward_first_out[edge_index] as usize
                ..bundle.downward_first_out[edge_index + 1] as usize];
            assert!(row.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn edge_order_is_a_valid_elimination_order() {
        let topology = grid_topology(12);
        let edge_count = topology.edge_count();
        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));

        // The order is a permutation of all edge states and the rank table
        // is exactly its inverse, so contraction visits every state once.
        assert_eq!(bundle.edge_order.len(), edge_count);
        let mut seen = vec![false; edge_count];
        for &edge_index in &bundle.edge_order {
            assert!(!seen[edge_index as usize]);
            seen[edge_index as usize] = true;
        }
        assert!(seen.into_iter().all(|hit| hit));
        for (rank, &edge_index) in bundle.edge_order.iter().enumerate() {
            assert_eq!(bundle.edge_rank[edge_index as usize], rank as u32);
        }
        // Arcs are consistent with the order in both directions.
        for edge_index in 0..edge_count {
            for slot in bundle.upward_first_out[edge_index] as usize
                ..bundle.upward_first_out[edge_index + 1] as usize
            {
                let head = bundle.upward_head[slot] as usize;
                assert!(bundle.edge_rank[edge_index] < bundle.edge_rank[head]);
            }
            for slot in bundle.downward_first_out[edge_index] as usize
                ..bundle.downward_first_out[edge_index + 1] as usize
            {
                let head = bundle.downward_head[slot] as usize;
                assert!(bundle.edge_rank[edge_index] > bundle.edge_rank[head]);
            }
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
        let bundle = build_dataset_acceleration_bundle(&topology, CacheBundleId::new("grid"));
        let transitions = &topology.edge_based_topology;
        let mut uturns = 0;
        for edge in 0..topology.edge_count() {
            let from = topology.routing_edge(edge);
            for &next in &transitions.edge_transition_edges[transitions.edge_transition_first_out
                [edge] as usize
                ..transitions.edge_transition_first_out[edge + 1] as usize]
            {
                let to = topology.routing_edge(next as usize);
                if from.from == to.to && from.to == to.from {
                    uturns += 1;
                }
                let (offsets, heads) = if bundle.edge_rank[edge] < bundle.edge_rank[next as usize] {
                    (&bundle.upward_first_out, &bundle.upward_head)
                } else {
                    (&bundle.downward_first_out, &bundle.downward_head)
                };
                assert!(
                    heads[offsets[edge] as usize..offsets[edge + 1] as usize]
                        .binary_search(&next)
                        .is_ok()
                );
            }
        }
        assert!(uturns > 0);
    }

    #[test]
    fn contraction_is_complete_on_a_grid() {
        let topology = grid_topology(4);
        let edge_count = topology.edge_count();
        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));

        // Reverse index of downward arcs: incoming higher-ranked tails per state.
        let mut incoming_from_higher: Vec<Vec<u32>> = vec![Vec::new(); edge_count];
        for tail in 0..edge_count {
            for slot in bundle.downward_first_out[tail] as usize
                ..bundle.downward_first_out[tail + 1] as usize
            {
                incoming_from_higher[bundle.downward_head[slot] as usize].push(tail as u32);
            }
        }

        let has_arc = |from: usize, to: u32| -> bool {
            let (first_out, heads) = if bundle.edge_rank[from] < bundle.edge_rank[to as usize] {
                (&bundle.upward_first_out, &bundle.upward_head)
            } else {
                (&bundle.downward_first_out, &bundle.downward_head)
            };
            heads[first_out[from] as usize..first_out[from + 1] as usize]
                .binary_search(&to)
                .is_ok()
        };

        // Lower-triangle completeness: for every state v, every incoming arc
        // from a higher-ranked x and outgoing arc to a higher-ranked y must
        // be closed by an arc between x and y. This is the invariant that
        // makes CCH queries exact with no follow-up search.
        for (v, incoming) in incoming_from_higher.iter().enumerate() {
            let outgoing = &bundle.upward_head
                [bundle.upward_first_out[v] as usize..bundle.upward_first_out[v + 1] as usize];
            for &x in incoming {
                for &y in outgoing {
                    if x == y {
                        continue;
                    }
                    assert!(
                        has_arc(x as usize, y),
                        "missing closure arc {x} -> {y} for contracted state {v}"
                    );
                }
            }
        }
    }
}

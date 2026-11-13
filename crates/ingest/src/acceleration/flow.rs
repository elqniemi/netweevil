//! Minimum vertex cuts for inertial-flow separators.

use anyhow::Result;

use super::dissection::{Cell, CutBound, ROLE_INNER, ROLE_SINK, ROLE_SOURCE, Split};
use crate::topology::checked_u32;

/// Vertex-capacity flow network of one cell, shared by all projection
/// directions and read straight from the cell adjacency.
///
/// Every cell node is split into an entry flow node `2 * local` and an exit
/// flow node `2 * local + 1` joined by an internal unit arc, so every finite
/// minimum cut is a vertex separator. Every cell arc `v -> w` runs from the
/// exit of `v` to the entry of `w` with infinite capacity. Each of these
/// arcs and its residual reverse form one link: cell arc `k` is link `k`, and
/// the internal arc of node `v` is link `heads.len() + v`.
pub(super) struct FlowGraph<'a> {
    cell: &'a Cell,
    /// Index of the opposite cell arc `w -> v` of every cell arc `v -> w`.
    rev: Vec<u32>,
}

fn entry(node: usize) -> usize {
    2 * node
}

fn exit(node: usize) -> usize {
    2 * node + 1
}

fn is_exit(flow_node: usize) -> bool {
    flow_node % 2 == 1
}

impl<'a> FlowGraph<'a> {
    pub(super) fn new(cell: &'a Cell) -> Result<Self> {
        checked_u32(cell.heads.len() + cell.len(), "flow network links")?;
        // Rows are sorted and the adjacency is symmetric, so the lower
        // neighbours of `w` are met in ascending order while scanning the
        // nodes in ascending order.
        let mut rev = vec![0_u32; cell.heads.len()];
        let mut cursor = cell.first_out[..cell.len()].to_vec();
        for node in 0..cell.len() {
            for arc in cell.first_out[node] as usize..cell.first_out[node + 1] as usize {
                let neighbour = cell.heads[arc] as usize;
                if neighbour > node {
                    let back = cursor[neighbour] as usize;
                    cursor[neighbour] += 1;
                    debug_assert_eq!(cell.heads[back] as usize, node);
                    rev[arc] = back as u32;
                    rev[back] = arc as u32;
                }
            }
        }
        Ok(Self { cell, rev })
    }

    fn internal_link(&self, node: usize) -> u32 {
        (self.cell.heads.len() + node) as u32
    }
}

impl FlowGraph<'_> {
    fn link_count(&self, flow_node: usize) -> usize {
        let node = flow_node / 2;
        (self.cell.first_out[node + 1] - self.cell.first_out[node]) as usize + 1
    }

    /// The `index`-th flow node adjacent to `flow_node` and the link to it:
    /// first the other half of the same cell node, then one per cell arc.
    fn link_at(&self, flow_node: usize, index: usize) -> (usize, u32) {
        let node = flow_node / 2;
        if index == 0 {
            return (flow_node ^ 1, self.internal_link(node));
        }
        let arc = self.cell.first_out[node] as usize + index - 1;
        let neighbour = self.cell.heads[arc] as usize;
        if is_exit(flow_node) {
            (entry(neighbour), arc as u32)
        } else {
            (exit(neighbour), self.rev[arc])
        }
    }
}

/// Residual state of one direction's max flow over a shared [`FlowGraph`].
///
/// The flow nodes of source and sink cell nodes act as one contracted
/// terminal each. Every arc carries at most one unit: an internal arc
/// because of its capacity, a cell arc because its tail exit or its head
/// entry passes at most one unit on.
struct FlowState<'a> {
    graph: &'a FlowGraph<'a>,
    role: &'a [u8],
    /// Flow through the internal arc of every cell node.
    through: Vec<bool>,
    /// Flow on every cell arc.
    flow: Vec<bool>,
}

impl<'a> FlowState<'a> {
    /// Returns `None` when a source node touches a sink node, in which case
    /// no vertex separator exists for this assignment.
    fn new(graph: &'a FlowGraph<'a>, role: &'a [u8]) -> Option<Self> {
        let cell = graph.cell;
        for node in 0..cell.len() {
            if role[node] == ROLE_SOURCE
                && cell
                    .neighbours(node)
                    .iter()
                    .any(|&neighbour| role[neighbour as usize] == ROLE_SINK)
            {
                return None;
            }
        }
        Some(Self {
            graph,
            role,
            through: vec![false; cell.len()],
            flow: vec![false; cell.heads.len()],
        })
    }

    /// Whether `link` has residual capacity leaving `from`.
    fn residual(&self, link: u32, from: usize) -> bool {
        let link = link as usize;
        let cell_arcs = self.flow.len();
        if link >= cell_arcs {
            // Entry to exit while unused, exit to entry while used.
            self.through[link - cell_arcs] == (is_exit(from))
        } else {
            // Exit to entry is unbounded; entry to exit undoes flow.
            is_exit(from) || self.flow[link]
        }
    }

    /// Pushes one unit over `link` leaving `from`.
    fn push(&mut self, link: u32, from: usize) {
        debug_assert!(self.residual(link, from), "augmenting arcs keep capacity");
        let link = link as usize;
        let cell_arcs = self.flow.len();
        if link >= cell_arcs {
            self.through[link - cell_arcs] = !is_exit(from);
        } else {
            debug_assert!(
                self.flow[link] != (is_exit(from)),
                "cell arcs carry at most one unit"
            );
            self.flow[link] = is_exit(from);
        }
    }
}

/// One flow node on the depth-first search stack, the link the search took
/// to reach it and the position of its next untried link in
/// [`NeighbourOrder`].
struct Frame {
    flow_node: u32,
    link: u32,
    next: u32,
}

/// A set of flow nodes as one bit each.
struct FlowNodeSet {
    words: Vec<u64>,
}

impl FlowNodeSet {
    fn new(flow_nodes: usize) -> Self {
        Self {
            words: vec![0; flow_nodes.div_ceil(64)],
        }
    }

    fn clear(&mut self) {
        self.words.fill(0);
    }

    fn contains(&self, flow_node: usize) -> bool {
        self.words[flow_node / 64] & (1 << (flow_node % 64)) != 0
    }

    /// Adds `flow_node` and returns whether it was absent.
    fn insert(&mut self, flow_node: usize) -> bool {
        let word = &mut self.words[flow_node / 64];
        let bit = 1 << (flow_node % 64);
        let absent = *word & bit == 0;
        *word |= bit;
        absent
    }
}

/// The cell arcs of every row sorted by the neighbour's projection,
/// farthest first, as positions within the row. Rows too long for 16-bit
/// positions keep their own order.
struct NeighbourOrder {
    position: Vec<u16>,
}

impl NeighbourOrder {
    fn new(cell: &Cell, projection: &[f32]) -> Self {
        let mut position = vec![0_u16; cell.heads.len()];
        for node in 0..cell.len() {
            let row = cell.neighbours(node);
            let Ok(degree) = u16::try_from(row.len()) else {
                continue;
            };
            let slots =
                &mut position[cell.first_out[node] as usize..cell.first_out[node + 1] as usize];
            for (slot, index) in slots.iter_mut().zip(0..degree) {
                *slot = index;
            }
            slots.sort_by(|&left, &right| {
                projection[row[right as usize] as usize]
                    .total_cmp(&projection[row[left as usize] as usize])
            });
        }
        Self { position }
    }

    /// The link index [`FlowGraph::link_at`] takes for the `rank`-th link to
    /// try from `flow_node`: the internal link first, then the cell arcs.
    fn link_index(&self, cell: &Cell, flow_node: usize, rank: usize) -> usize {
        if rank == 0 {
            return 0;
        }
        let node = flow_node / 2;
        let degree = (cell.first_out[node + 1] - cell.first_out[node]) as usize;
        if degree > usize::from(u16::MAX) {
            return rank;
        }
        usize::from(self.position[cell.first_out[node] as usize + rank - 1]) + 1
    }
}

/// Computes a maximum flow from the source set to the sink set and returns,
/// per flow node, whether the source set reaches it in the final residual
/// network: the source side of the minimum cut closest to the source, which
/// every maximum flow leaves the same.
///
/// Augmenting paths come from depth-first searches that try the neighbours
/// farthest along `projection` (towards the sink) first. Within a phase,
/// visited flow nodes stay visited, so one sweep finds many paths; the first
/// phase that finds none has visited exactly the residual reach.
///
/// Returns `None` once `bound` rejects the flow pushed so far: the minimum
/// cut is at least that flow, so it can no longer win.
fn max_flow(
    state: &mut FlowState<'_>,
    projection: &[f32],
    bound: CutBound<'_>,
) -> Option<FlowNodeSet> {
    if !bound.admits(0) {
        return None;
    }
    let graph = state.graph;
    let cell = graph.cell;
    let role = state.role;
    // Source exits that touch inner nodes, nearest to the sink first.
    let mut sources = (0..cell.len())
        .filter(|&node| {
            role[node] == ROLE_SOURCE
                && cell
                    .neighbours(node)
                    .iter()
                    .any(|&neighbour| role[neighbour as usize] == ROLE_INNER)
        })
        .map(exit)
        .collect::<Vec<_>>();
    sources.sort_by(|&left, &right| projection[right / 2].total_cmp(&projection[left / 2]));
    let order = NeighbourOrder::new(cell, projection);

    let mut visited = FlowNodeSet::new(2 * cell.len());
    let mut stack = Vec::<Frame>::new();
    let mut flow = 0_u32;
    loop {
        visited.clear();
        let phase_flow = flow;
        for &source in &sources {
            stack.clear();
            stack.push(Frame {
                flow_node: source as u32,
                link: 0,
                next: 0,
            });
            while let Some(top) = stack.last_mut() {
                let flow_node = top.flow_node as usize;
                if top.next as usize == graph.link_count(flow_node) {
                    stack.pop();
                    continue;
                }
                let (neighbour, link) = graph.link_at(
                    flow_node,
                    order.link_index(cell, flow_node, top.next as usize),
                );
                top.next += 1;
                if !state.residual(link, flow_node) {
                    continue;
                }
                match role[neighbour / 2] {
                    ROLE_SOURCE => continue,
                    ROLE_SINK => {
                        state.push(link, flow_node);
                        for frame in 1..stack.len() {
                            state.push(stack[frame].link, stack[frame - 1].flow_node as usize);
                        }
                        flow += 1;
                        if !bound.admits(flow) {
                            return None;
                        }
                        // Resume below the first arc the push saturated.
                        let saturated = (1..stack.len()).find(|&frame| {
                            !state.residual(stack[frame].link, stack[frame - 1].flow_node as usize)
                        });
                        if let Some(frame) = saturated {
                            stack.truncate(frame);
                        }
                        continue;
                    }
                    _ => {}
                }
                if !visited.insert(neighbour) {
                    continue;
                }
                stack.push(Frame {
                    flow_node: neighbour as u32,
                    link,
                    next: 0,
                });
            }
        }
        if flow == phase_flow {
            return Some(visited);
        }
    }
}

/// Splits a cell along one source/sink assignment with a minimum vertex cut.
pub(super) fn flow_split(
    graph: &FlowGraph<'_>,
    role: &[u8],
    projection: &[f32],
    bound: CutBound<'_>,
) -> Option<Split> {
    let mut state = FlowState::new(graph, role)?;
    let in_source = max_flow(&mut state, projection, bound)?;
    drop(state);

    let mut side_a = Vec::new();
    let mut side_b = Vec::new();
    let mut separator = Vec::new();
    for (node, &node_role) in role.iter().enumerate() {
        match node_role {
            ROLE_SOURCE => side_a.push(node as u32),
            ROLE_SINK => side_b.push(node as u32),
            _ if in_source.contains(exit(node)) => side_a.push(node as u32),
            _ if in_source.contains(entry(node)) => separator.push(node as u32),
            _ => side_b.push(node as u32),
        }
    }
    Some(Split {
        side_a,
        side_b,
        separator,
    })
}

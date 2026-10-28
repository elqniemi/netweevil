//! Sparse per-edge labels of a service-area expansion.
//!
//! Expansions used to allocate three dense `f64` arrays over every edge of
//! the dataset per origin, which dominated small isochrones and multiplied
//! memory by the number of origins. Labels now live only for reached edges;
//! a thread-local dense position index makes updates O(1) during the search
//! and is reset through the reached list afterwards.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Index;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use super::ServiceAreaEdgeComponentRange;

const NO_POSITION: u32 = u32::MAX;

thread_local! {
    static LABEL_POSITIONS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// Per-edge values for reached edges, aligned with `reached_edges`; other
/// edges read as `default`.
#[derive(Debug, Clone)]
pub(crate) struct SparseEdgeValues {
    positions: Arc<FxHashMap<u32, u32>>,
    values: Vec<f64>,
    default: f64,
}

impl SparseEdgeValues {
    pub(crate) fn get(&self, edge_index: usize) -> Option<&f64> {
        Some(&self[edge_index])
    }

    /// Value of the `position`-th reached edge.
    pub(crate) fn at(&self, position: usize) -> f64 {
        self.values[position]
    }
}

impl Index<usize> for SparseEdgeValues {
    type Output = f64;

    fn index(&self, edge_index: usize) -> &f64 {
        u32::try_from(edge_index)
            .ok()
            .and_then(|edge| self.positions.get(&edge))
            .map_or(&self.default, |&position| &self.values[position as usize])
    }
}

/// Collects the best label per reached edge during one expansion.
pub(crate) struct EdgeLabelBuilder {
    positions: Vec<u32>,
    pub(crate) reached_edges: Vec<u32>,
    before: Vec<f64>,
    end: Vec<f64>,
    start_fraction: Vec<f64>,
    pub(crate) component_ranges: HashMap<usize, ServiceAreaEdgeComponentRange>,
}

impl EdgeLabelBuilder {
    pub(crate) fn new(edge_count: usize) -> Self {
        let mut positions = LABEL_POSITIONS.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        if positions.len() < edge_count {
            positions.resize(edge_count, NO_POSITION);
        }
        Self {
            positions,
            reached_edges: Vec::new(),
            before: Vec::new(),
            end: Vec::new(),
            start_fraction: Vec::new(),
            component_ranges: HashMap::new(),
        }
    }

    /// Keeps the label with the smallest end cost, as the dense arrays did.
    pub(crate) fn update(
        &mut self,
        edge_index: usize,
        before_cost: f64,
        end_cost: f64,
        start_fraction: f64,
        before_components: Vec<f64>,
        end_components: Vec<f64>,
    ) {
        match self.positions[edge_index] {
            NO_POSITION => {
                if end_cost + f64::EPSILON >= f64::INFINITY {
                    return;
                }
                self.positions[edge_index] = self.reached_edges.len() as u32;
                self.reached_edges.push(edge_index as u32);
                self.before.push(before_cost);
                self.end.push(end_cost);
                self.start_fraction.push(start_fraction);
            }
            position => {
                let position = position as usize;
                if end_cost + f64::EPSILON >= self.end[position] {
                    return;
                }
                self.before[position] = before_cost;
                self.end[position] = end_cost;
                self.start_fraction[position] = start_fraction;
            }
        }
        if !before_components.is_empty() || !end_components.is_empty() {
            self.component_ranges.insert(
                edge_index,
                ServiceAreaEdgeComponentRange {
                    before: before_components,
                    end: end_components,
                },
            );
        }
    }

    /// Drops every label, keeping allocations, before a restarted expansion.
    pub(crate) fn clear(&mut self) {
        for &edge in &self.reached_edges {
            self.positions[edge as usize] = NO_POSITION;
        }
        self.reached_edges.clear();
        self.before.clear();
        self.end.clear();
        self.start_fraction.clear();
        self.component_ranges.clear();
    }

    pub(crate) fn finish(
        mut self,
    ) -> (
        Vec<u32>,
        SparseEdgeValues,
        SparseEdgeValues,
        SparseEdgeValues,
        HashMap<usize, ServiceAreaEdgeComponentRange>,
    ) {
        let mut index =
            FxHashMap::with_capacity_and_hasher(self.reached_edges.len(), Default::default());
        for (position, &edge) in self.reached_edges.iter().enumerate() {
            self.positions[edge as usize] = NO_POSITION;
            index.insert(edge, position as u32);
        }
        // Report reached edges in ascending edge order: interval builders
        // then emit already-sorted runs and their normalizing sort is linear.
        let mut order = self
            .reached_edges
            .iter()
            .enumerate()
            .map(|(position, &edge)| (u64::from(edge) << 32) | position as u64)
            .collect::<Vec<_>>();
        order.sort_unstable();
        let permute = |values: &[f64]| {
            order
                .iter()
                .map(|&packed| values[(packed & u64::from(u32::MAX)) as usize])
                .collect::<Vec<_>>()
        };
        self.before = permute(&self.before);
        self.end = permute(&self.end);
        self.start_fraction = permute(&self.start_fraction);
        index.clear();
        for (position, &packed) in order.iter().enumerate() {
            let edge = (packed >> 32) as u32;
            self.reached_edges[position] = edge;
            index.insert(edge, position as u32);
        }
        let reached_edges = std::mem::take(&mut self.reached_edges);
        let index = Arc::new(index);
        let values = |values: Vec<f64>, default: f64| SparseEdgeValues {
            positions: index.clone(),
            values,
            default,
        };
        (
            reached_edges,
            values(std::mem::take(&mut self.before), f64::INFINITY),
            values(std::mem::take(&mut self.end), f64::INFINITY),
            values(std::mem::take(&mut self.start_fraction), 0.0),
            std::mem::take(&mut self.component_ranges),
        )
    }
}

impl Drop for EdgeLabelBuilder {
    fn drop(&mut self) {
        if self.positions.is_empty() {
            return;
        }
        for &edge in &self.reached_edges {
            self.positions[edge as usize] = NO_POSITION;
        }
        let positions = std::mem::take(&mut self.positions);
        LABEL_POSITIONS.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.len() < positions.len() {
                *cell = positions;
            }
        });
    }
}

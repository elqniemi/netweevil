//! A compact 4-ary min-queue for label-setting searches whose entries have
//! `score == cost`.

/// Pops the lowest cost first; among equal costs, the highest edge index and
/// then the highest automaton state. That is exactly the pop order of
/// `BinaryHeap<State>` when `score == cost`, so swapping this queue in keeps
/// every search's tie-breaking. Searches only push strict improvements, so
/// entries are distinct and the pop sequence is fully determined by the order.
#[derive(Default)]
pub(crate) struct StateHeap {
    entries: Vec<StateHeapEntry>,
}

#[derive(Clone, Copy)]
struct StateHeapEntry {
    key: u64,
    edge: u32,
    automaton_state: u32,
}

impl StateHeapEntry {
    fn new(cost: f64, edge: usize, automaton_state: usize) -> Self {
        // Order-preserving map of `f64::total_cmp` onto unsigned integers.
        let bits = cost.to_bits();
        let key = if bits >> 63 == 1 {
            !bits
        } else {
            bits | (1 << 63)
        };
        Self {
            key,
            edge: u32::try_from(edge).expect("edge index fits in u32"),
            automaton_state: u32::try_from(automaton_state).expect("automaton state fits in u32"),
        }
    }

    fn cost(self) -> f64 {
        f64::from_bits(if self.key >> 63 == 1 {
            self.key & !(1 << 63)
        } else {
            !self.key
        })
    }

    fn precedes(self, other: Self) -> bool {
        (self.key, other.edge, other.automaton_state) < (other.key, self.edge, self.automaton_state)
    }
}

impl StateHeap {
    const ARITY: usize = 4;

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Cost of the next entry to pop, or infinity when empty.
    pub(crate) fn peek_cost(&self) -> f64 {
        self.entries
            .first()
            .map_or(f64::INFINITY, |entry| entry.cost())
    }

    pub(crate) fn push(&mut self, edge: usize, automaton_state: usize, cost: f64) {
        let entry = StateHeapEntry::new(cost, edge, automaton_state);
        let mut position = self.entries.len();
        self.entries.push(entry);
        while position > 0 {
            let parent = (position - 1) / Self::ARITY;
            if !entry.precedes(self.entries[parent]) {
                break;
            }
            self.entries[position] = self.entries[parent];
            position = parent;
        }
        self.entries[position] = entry;
    }

    /// Removes the next entry as `(edge, automaton_state, cost)`.
    pub(crate) fn pop(&mut self) -> Option<(usize, usize, f64)> {
        let top = *self.entries.first()?;
        let last = self.entries.pop().expect("non-empty heap");
        let len = self.entries.len();
        if len > 0 {
            let mut position = 0;
            loop {
                let first_child = position * Self::ARITY + 1;
                if first_child >= len {
                    break;
                }
                let mut best = first_child;
                for child in first_child + 1..(first_child + Self::ARITY).min(len) {
                    if self.entries[child].precedes(self.entries[best]) {
                        best = child;
                    }
                }
                if !self.entries[best].precedes(last) {
                    break;
                }
                self.entries[position] = self.entries[best];
                position = best;
            }
            self.entries[position] = last;
        }
        Some((top.edge as usize, top.automaton_state as usize, top.cost()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::State;
    use std::collections::BinaryHeap;

    /// The 4-ary heap must pop in exactly the order of `BinaryHeap<State>`,
    /// so searches keep their tie-breaking.
    #[test]
    fn pops_in_binary_heap_state_order() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut heap = StateHeap::default();
        let mut reference = BinaryHeap::new();
        for round in 0..4_000 {
            if next() % 3 != 0 {
                // Few distinct values force ties, including -0.0 and +0.0.
                let cost = match next() % 6 {
                    0 => -0.0,
                    1 => -((next() % 5) as f64) * 0.25,
                    _ => (next() % 9) as f64 * 0.5,
                };
                let edge = (next() % 40) as usize;
                let automaton_state = (next() % 3) as usize;
                if reference.iter().any(|item: &State| {
                    item.edge_index == edge
                        && item.automaton_state == automaton_state
                        && item.cost.to_bits() == cost.to_bits()
                }) {
                    continue;
                }
                heap.push(edge, automaton_state, cost);
                reference.push(State {
                    edge_index: edge,
                    automaton_state,
                    cost,
                    score: cost,
                });
            } else {
                let expected = reference
                    .pop()
                    .map(|item| (item.edge_index, item.automaton_state, item.cost.to_bits()));
                let actual = heap
                    .pop()
                    .map(|(edge, automaton_state, cost)| (edge, automaton_state, cost.to_bits()));
                assert_eq!(actual, expected, "round {round}");
            }
            assert_eq!(
                heap.peek_cost().to_bits(),
                reference
                    .peek()
                    .map_or(f64::INFINITY, |item| item.cost)
                    .to_bits()
            );
        }
    }
}

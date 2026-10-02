//! Monotone radix heap keyed by non-negative `f64` costs.
//!
//! Non-negative IEEE-754 doubles order like their bit patterns, so the heap
//! buckets keys by the highest bit in which they differ from the last
//! extracted minimum. Dijkstra-style searches with non-negative arc costs
//! never insert below that minimum, which makes push O(1) and pop amortized
//! O(log C) without comparisons on a binary tree.

/// Items carry a `u64` payload; ties pop in unspecified order.
#[derive(Default)]
pub(crate) struct RadixHeap {
    last: u64,
    len: usize,
    buckets: Vec<Vec<(u64, u64)>>,
}

impl RadixHeap {
    pub(crate) fn clear(&mut self) {
        self.last = 0;
        self.len = 0;
        for bucket in &mut self.buckets {
            bucket.clear();
        }
    }

    fn bucket_index(last: u64, key: u64) -> usize {
        (u64::BITS - (key ^ last).leading_zeros()) as usize
    }

    /// Inserts `payload` at `cost`. Costs below the last popped minimum are
    /// clamped to it, which only happens with negative arc costs.
    pub(crate) fn push(&mut self, cost: f64, payload: u64) {
        debug_assert!(cost >= 0.0 && cost.is_finite());
        if self.buckets.is_empty() {
            self.buckets.resize_with(u64::BITS as usize + 1, Vec::new);
        }
        let key = cost.max(0.0).to_bits().max(self.last);
        self.buckets[Self::bucket_index(self.last, key)].push((key, payload));
        self.len += 1;
    }

    pub(crate) fn pop(&mut self) -> Option<(f64, u64)> {
        if self.len == 0 {
            return None;
        }
        if self.buckets[0].is_empty() {
            let index = self
                .buckets
                .iter()
                .position(|bucket| !bucket.is_empty())
                .expect("non-empty heap has a bucket");
            let bucket = std::mem::take(&mut self.buckets[index]);
            self.last = bucket.iter().map(|&(key, _)| key).min().expect("non-empty");
            for &(key, payload) in &bucket {
                self.buckets[Self::bucket_index(self.last, key)].push((key, payload));
            }
            let mut bucket = bucket;
            bucket.clear();
            if self.buckets[index].is_empty() {
                self.buckets[index] = bucket;
            }
        }
        self.len -= 1;
        let (key, payload) = self.buckets[0]
            .pop()
            .expect("bucket zero holds the minimum");
        Some((f64::from_bits(key), payload))
    }
}

#[cfg(test)]
mod tests {
    use super::RadixHeap;

    #[test]
    fn pops_in_nondecreasing_cost_order_with_interleaved_pushes() {
        let mut random = 3_u64;
        let mut next = || {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            random >> 40
        };
        let mut heap = RadixHeap::default();
        let mut reference = Vec::new();
        let mut last = 0.0;
        for round in 0..2_000 {
            if round % 3 != 2 {
                let cost = last + (next() % 1000) as f64 / 7.0;
                heap.push(cost, round);
                reference.push(cost);
            } else if let Some((cost, _)) = heap.pop() {
                reference.sort_by(f64::total_cmp);
                assert_eq!(cost, reference.remove(0));
                assert!(cost >= last);
                last = cost;
            }
        }
        while let Some((cost, _)) = heap.pop() {
            reference.sort_by(f64::total_cmp);
            assert_eq!(cost, reference.remove(0));
        }
        assert!(reference.is_empty());
    }
}

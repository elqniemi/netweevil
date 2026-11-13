//! Visiting order of a multi-stop route's stops (`multistop.rs`) from a cost matrix: exact
//! (Held-Karp dynamic programming) up to `EXACT_MAX` stops, nearest neighbour improved by
//! 2-opt and Or-opt moves above. The start is fixed, and so is the end when there is one;
//! costs may be asymmetric (stairs, one-way edges).

/// Most stops ordered exactly: Held-Karp takes O(2^n n^2) time and O(2^n n) memory, about
/// 1.3 ms at 12 stops, 15 ms at 15 and 32 ms at 16 (release build). The
/// heuristic takes 40 µs at 12 stops and 2 ms at 50.
pub const EXACT_MAX: usize = 12;
/// Rounds of local search at most (each round tries every move once).
const ROUNDS: usize = 64;

/// Costs between the points of a trip, row-major `k` x `k`: 0 is the start, 1..=n the stops
/// and n + 1 the end (when it is fixed). INFINITY: no way.
pub struct Matrix {
    pub k: usize,
    pub c: Vec<f32>,
}

impl Matrix {
    #[inline]
    pub fn at(&self, i: usize, j: usize) -> f32 {
        self.c[i * self.k + j]
    }
}

/// Cost of visiting the stops in `order` (0-based stop indices) from the start, then the end
/// when `end`.
pub fn path_cost(m: &Matrix, order: &[usize], end: bool) -> f32 {
    let mut at = 0;
    let mut sum = 0.0;
    for &s in order {
        sum += m.at(at, s + 1);
        at = s + 1;
    }
    if end {
        sum += m.at(at, order.len() + 1);
    }
    sum
}

/// The order that costs least, and how it was found (`exact` / `heuristic`).
pub fn best_order(m: &Matrix, n: usize, end: bool) -> (Vec<usize>, &'static str) {
    if n <= EXACT_MAX
        && let Some(o) = held_karp(m, n, end)
    {
        return (o, "exact");
    }
    // No order reaches every stop: the heuristic says which one fails.
    (heuristic(m, n, end), "heuristic")
}

/// Exact order (None: no order is finite). `dp[S][j]`: least cost from the start through the
/// stop set S, ending at stop j in S.
pub fn held_karp(m: &Matrix, n: usize, end: bool) -> Option<Vec<usize>> {
    if n == 0 {
        return Some(vec![]);
    }
    let full = (1usize << n) - 1;
    let mut dp = vec![f32::INFINITY; (full + 1) * n];
    let mut from = vec![u8::MAX; (full + 1) * n];
    for j in 0..n {
        dp[(1 << j) * n + j] = m.at(0, j + 1);
    }
    for s in 1..=full {
        for j in 0..n {
            let d = dp[s * n + j];
            if s & (1 << j) == 0 || !d.is_finite() {
                continue;
            }
            for k in 0..n {
                if s & (1 << k) != 0 {
                    continue;
                }
                let t = s | (1 << k);
                let c = d + m.at(j + 1, k + 1);
                if c < dp[t * n + k] {
                    dp[t * n + k] = c;
                    from[t * n + k] = j as u8;
                }
            }
        }
    }
    let close = |j: usize| dp[full * n + j] + if end { m.at(j + 1, n + 1) } else { 0.0 };
    let last = (0..n).min_by(|&a, &b| close(a).total_cmp(&close(b)))?;
    if !close(last).is_finite() {
        return None;
    }
    let mut order = vec![last];
    let (mut s, mut j) = (full, last);
    while from[s * n + j] != u8::MAX {
        let p = from[s * n + j] as usize;
        s &= !(1 << j);
        j = p;
        order.push(j);
    }
    order.reverse();
    Some(order)
}

/// Nearest neighbour from the start, then 2-opt (reverse a run) and Or-opt (move a run of
/// 1 to 3 stops) until no move helps. Each move is costed on the whole path, so asymmetric
/// costs are right.
pub fn heuristic(m: &Matrix, n: usize, end: bool) -> Vec<usize> {
    let mut order = Vec::with_capacity(n);
    let mut used = vec![false; n];
    let mut at = 0;
    for _ in 0..n {
        let next = (0..n)
            .filter(|&s| !used[s])
            .min_by(|&a, &b| m.at(at, a + 1).total_cmp(&m.at(at, b + 1)))
            .unwrap();
        used[next] = true;
        order.push(next);
        at = next + 1;
    }
    let mut cost = path_cost(m, &order, end);
    let mut cand = order.clone();
    let better = |cand: &[usize], order: &mut Vec<usize>, cost: &mut f32| {
        let c = path_cost(m, cand, end);
        // Strictly better by more than rounding, so the search always ends.
        if c < *cost - 1e-4 * cost.abs().max(1.0) {
            *cost = c;
            order.copy_from_slice(cand);
            true
        } else {
            false
        }
    };
    for _ in 0..ROUNDS {
        let mut moved = false;
        for i in 0..n {
            for j in i + 1..n {
                cand.copy_from_slice(&order);
                cand[i..=j].reverse();
                moved |= better(&cand, &mut order, &mut cost);
            }
        }
        for len in 1..=3.min(n) {
            for i in 0..=n - len {
                for p in 0..=n - len {
                    if p == i {
                        continue;
                    }
                    cand.copy_from_slice(&order);
                    let run: Vec<usize> = cand.drain(i..i + len).collect();
                    cand.splice(p..p, run);
                    moved |= better(&cand, &mut order, &mut cost);
                }
            }
        }
        if !moved {
            break;
        }
    }
    order
}

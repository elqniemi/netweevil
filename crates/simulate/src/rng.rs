//! Deterministic, dependency-free RNG used everywhere in the simulation so
//! that a scenario with the same seed always reproduces the same run.

/// SplitMix64, used to seed and derive independent substreams.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// Xoshiro256** main generator.
#[derive(Debug, Clone)]
pub struct SimRng {
    s: [u64; 4],
    /// Cached second normal sample from Box-Muller.
    spare_normal: Option<f64>,
}

impl SimRng {
    pub fn seeded(seed: u64) -> Self {
        let mut mix = SplitMix64::new(seed);
        Self {
            s: [
                mix.next_u64(),
                mix.next_u64(),
                mix.next_u64(),
                mix.next_u64(),
            ],
            spare_normal: None,
        }
    }

    /// Derive an independent deterministic substream (e.g. per agent).
    pub fn derive(seed: u64, stream: u64) -> Self {
        let mut mix = SplitMix64::new(seed ^ stream.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let _ = mix.next_u64();
        Self::seeded(mix.next_u64())
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [low, high).
    pub fn uniform(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.next_f64()
    }

    /// Uniform integer in [0, bound).
    pub fn next_index(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        self.next_f64() < probability
    }

    /// Standard normal via Box-Muller.
    pub fn normal(&mut self, mean: f64, std_dev: f64) -> f64 {
        if let Some(z) = self.spare_normal.take() {
            return mean + std_dev * z;
        }
        let mut u1 = self.next_f64();
        if u1 < 1e-12 {
            u1 = 1e-12;
        }
        let u2 = self.next_f64();
        let radius = (-2.0 * u1.ln()).sqrt();
        let theta = 2.0 * std::f64::consts::PI * u2;
        self.spare_normal = Some(radius * theta.sin());
        mean + std_dev * radius * theta.cos()
    }

    /// Exponential inter-arrival sample with the given rate (events/s).
    pub fn exponential(&mut self, rate_per_s: f64) -> f64 {
        if rate_per_s <= 0.0 {
            return f64::INFINITY;
        }
        let mut u = self.next_f64();
        if u < 1e-12 {
            u = 1e-12;
        }
        -u.ln() / rate_per_s
    }

    /// Weighted index choice; weights must be non-negative.
    pub fn weighted_index(&mut self, weights: &[f64]) -> usize {
        let total: f64 = weights.iter().filter(|w| w.is_finite() && **w > 0.0).sum();
        if total <= 0.0 || weights.is_empty() {
            return 0;
        }
        let mut target = self.next_f64() * total;
        for (index, weight) in weights.iter().enumerate() {
            if *weight > 0.0 && weight.is_finite() {
                target -= *weight;
                if target <= 0.0 {
                    return index;
                }
            }
        }
        weights.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = SimRng::seeded(42);
        let mut b = SimRng::seeded(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn derived_streams_differ() {
        let mut a = SimRng::derive(42, 1);
        let mut b = SimRng::derive(42, 2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn normal_is_roughly_centered() {
        let mut rng = SimRng::seeded(7);
        let mean: f64 = (0..10_000).map(|_| rng.normal(5.0, 2.0)).sum::<f64>() / 10_000.0;
        assert!((mean - 5.0).abs() < 0.1, "mean drifted: {mean}");
    }
}

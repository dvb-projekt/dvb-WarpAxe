//! A small, seedable PRNG so a simulation is a pure function of its seed.
//!
//! PCG32 (Melissa O'Neill's `pcg32_random_r`): 64-bit state, 32-bit
//! output, good enough statistics for timing and nonce bits, and — the
//! point — no dependency and no global state. Two `SimChain`s built from
//! the same seed and fed the same bytes produce the same bytes.

/// PCG32 generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    /// Seed with a state and a stream selector.
    pub fn new(seed: u64, stream: u64) -> Self {
        let mut r = Pcg32 {
            state: 0,
            inc: (stream << 1) | 1,
        };
        r.next_u32();
        r.state = r.state.wrapping_add(seed);
        r.next_u32();
        r
    }

    /// Next 32 random bits.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in `0..n` (`n > 0`), via multiply-high; slight bias below
    /// 2⁻³² is irrelevant here.
    pub fn below(&mut self, n: u32) -> u32 {
        ((u64::from(self.next_u32()) * u64::from(n)) >> 32) as u32
    }

    /// Uniform in `(0, 1]` as f64 (never exactly zero, so `ln` is safe).
    pub fn unit(&mut self) -> f64 {
        (f64::from(self.next_u32()) + 1.0) / 4_294_967_297.0
    }

    /// Exponentially distributed interval with the given rate per
    /// second, in microseconds. Inter-arrival time of a Poisson process.
    pub fn exp_interval_us(&mut self, rate_per_s: f64) -> u64 {
        if rate_per_s <= 0.0 {
            return u64::MAX;
        }
        let t = -self.unit().ln() / rate_per_s;
        (t * 1e6).min(u64::MAX as f64) as u64
    }

    /// True with probability `per_mille / 1000`.
    pub fn chance_per_mille(&mut self, per_mille: u16) -> bool {
        self.below(1000) < u32::from(per_mille)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Pcg32::new(42, 7);
        let mut b = Pcg32::new(42, 7);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn different_streams_differ() {
        let mut a = Pcg32::new(42, 1);
        let mut b = Pcg32::new(42, 2);
        assert_ne!(
            (0..8).map(|_| a.next_u32()).collect::<Vec<_>>(),
            (0..8).map(|_| b.next_u32()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn below_stays_below() {
        let mut r = Pcg32::new(1, 1);
        for _ in 0..10_000 {
            assert!(r.below(7) < 7);
        }
        assert_eq!(r.below(1), 0);
    }

    /// Mean of an exponential with rate λ is 1/λ. 20 000 samples at
    /// λ = 10/s: mean 100 000 µs, standard error ~700 µs.
    #[test]
    fn exp_interval_has_the_right_mean() {
        let mut r = Pcg32::new(3, 3);
        let n = 20_000;
        let sum: u64 = (0..n).map(|_| r.exp_interval_us(10.0)).sum();
        let mean = sum as f64 / f64::from(n);
        assert!((mean - 100_000.0).abs() < 3_000.0, "mean {mean}");
        assert_eq!(r.exp_interval_us(0.0), u64::MAX);
    }
}

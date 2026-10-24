//! What a nonce is worth in hashes, for the hashrate estimate.
//!
//! The chips report every nonce whose hash meets their ticket mask, the
//! largest power of two at or below the requested difficulty, so each
//! nonce stands for that many times 2^32 hashes. The firmware moves the
//! mask with the pool difficulty: down to it once VarDiff has settled,
//! back to the chip's default as soon as the pool changes it again. After
//! a raise, nonces the chips found under the old, lower mask are still on
//! the wire for a moment; counted at the new mask, each read 256 times
//! too much on a BM1370 going from 1 to 256. Seen 2026-10-24 on a Bitaxe
//! Gamma behind a pool that starts at difficulty 0.001 and steps it up
//! four times in the first six minutes: 637 such nonces, a displayed
//! 3.2 TH/s against 1.42 TH/s from the chip's own counter.
//!
//! So for a short while after a raise, a nonce below the new mask counts
//! at the old one. Outside that window a nonce below the mask is a
//! hardware error and still counts at the mask, as before (and as the
//! reference counts every nonce).

/// Hashes per difficulty-1 nonce.
const HASHES_PER_DIFF1: f64 = 4_294_967_296.0;
/// How long after a raise nonces found under the old mask may still
/// arrive.
pub const RAISE_GRACE_US: u64 = 2_000_000;

/// The ticket mask a requested difficulty gives: the largest power of
/// two at or below it.
pub fn ticket_mask(difficulty: u32) -> u32 {
    let d = difficulty.max(1);
    1u32 << (31 - d.leading_zeros())
}

/// Tracks the mask and the one before a raise.
#[derive(Debug, Clone, Copy)]
pub struct NonceWorth {
    mask: u32,
    /// The mask before the last raise, and when it was raised.
    raised_from: Option<(u32, u64)>,
}

impl NonceWorth {
    /// Starting at this requested difficulty.
    pub fn new(difficulty: u32) -> Self {
        NonceWorth {
            mask: ticket_mask(difficulty),
            raised_from: None,
        }
    }

    /// The chips now run at this requested difficulty.
    pub fn set(&mut self, difficulty: u32, now_us: u64) {
        let mask = ticket_mask(difficulty);
        if mask > self.mask {
            self.raised_from = Some((self.mask, now_us));
        } else if mask < self.mask {
            // Nonces found under the higher mask meet the lower one too;
            // counting them low only undercounts for a moment.
            self.raised_from = None;
        }
        self.mask = mask;
    }

    /// The current ticket mask.
    pub fn mask(&self) -> u32 {
        self.mask
    }

    /// The hashes a nonce of this difficulty stands for.
    pub fn hashes(&self, diff: f64, now_us: u64) -> f64 {
        let mask = match self.raised_from {
            Some((old, at))
                if diff < f64::from(self.mask) && now_us.saturating_sub(at) < RAISE_GRACE_US =>
            {
                old
            }
            _ => self.mask,
        };
        f64::from(mask) * HASHES_PER_DIFF1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000;

    #[test]
    fn mask_is_the_power_of_two_at_or_below() {
        assert_eq!(ticket_mask(0), 1);
        assert_eq!(ticket_mask(1), 1);
        assert_eq!(ticket_mask(16), 16);
        assert_eq!(ticket_mask(17), 16);
        assert_eq!(ticket_mask(256), 256);
        assert_eq!(ticket_mask(1000), 512);
    }

    #[test]
    fn steady_mask_counts_every_nonce_at_the_mask() {
        let w = NonceWorth::new(256);
        assert_eq!(w.hashes(300.0, 0), 256.0 * HASHES_PER_DIFF1);
        // A hardware error below the mask still counts as before.
        assert_eq!(w.hashes(3.0, 0), 256.0 * HASHES_PER_DIFF1);
    }

    #[test]
    fn nonces_from_before_a_raise_count_at_the_old_mask() {
        let mut w = NonceWorth::new(256);
        w.set(1, 81 * S);
        assert_eq!(w.hashes(5.0, 82 * S), HASHES_PER_DIFF1);
        w.set(256, 82 * S);
        // Still on the wire from the mask-1 time.
        assert_eq!(w.hashes(5.0, 82 * S + 300_000), HASHES_PER_DIFF1);
        // A nonce that meets the new mask counts at it.
        assert_eq!(w.hashes(400.0, 82 * S + 300_000), 256.0 * HASHES_PER_DIFF1);
        // After the grace, below the mask is an error again.
        assert_eq!(w.hashes(5.0, 85 * S), 256.0 * HASHES_PER_DIFF1);
    }

    #[test]
    fn a_lower_mask_ends_the_grace() {
        let mut w = NonceWorth::new(1);
        w.set(256, 10 * S);
        w.set(16, 10 * S + 100_000);
        assert_eq!(w.mask(), 16);
        assert_eq!(w.hashes(5.0, 10 * S + 200_000), 16.0 * HASHES_PER_DIFF1);
    }

    /// The Bitaxe Gamma case: a pool starting at 0.001 and stepping it
    /// up; the mask goes 256 → 1 → 256 each time. With the old count
    /// every leftover low nonce read 256 times too much.
    #[test]
    fn a_vardiff_ramp_no_longer_inflates_the_count() {
        let mut w = NonceWorth::new(256);
        let mut old_way = 0.0;
        let mut new_way = 0.0;
        for round in 0..4u64 {
            let t = round * 60 * S;
            w.set(1, t);
            w.set(256, t + 20 * S);
            // 160 nonces of difficulty 1–255 found under mask 1 arrive
            // within half a second of the raise.
            for i in 0..160u64 {
                let at = t + 20 * S + i * 3_000;
                old_way += 256.0 * HASHES_PER_DIFF1;
                new_way += w.hashes(1.0 + (i % 255) as f64, at);
            }
        }
        assert_eq!(new_way, 640.0 * HASHES_PER_DIFF1);
        assert_eq!(old_way, 640.0 * 256.0 * HASHES_PER_DIFF1);
    }
}

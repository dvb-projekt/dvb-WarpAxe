//! Which notify and which block the chips' work belongs to.
//!
//! The pool thread publishes work with two counters, the notify number
//! and the block-change (clean) number; the miner loop reads the
//! counters at its top and fetches the work later in the same round.
//! Between the two reads a new block may arrive. Retiring on the
//! counter change at the loop top AND on the clean flag of the fetched
//! work retired the freshly submitted job a second time (NerdQAxe++,
//! 2026-09-27: all chips "silent" for 13 s after some block changes,
//! their nonces counted as stale until the next refresh job). The gate
//! keeps both views in step: a block is retired once, whichever read
//! saw it first.

/// The miner's view of the work feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkGate {
    /// Notify number of the work last submitted to the chips.
    seen_notify: u64,
    /// Block-change number the chips' work belongs to.
    seen_clean: u64,
}

impl WorkGate {
    /// A gate for a chain that starts at the pool's current block.
    pub fn new(clean_jobs: u64) -> WorkGate {
        WorkGate {
            seen_notify: 0,
            seen_clean: clean_jobs,
        }
    }

    /// Loop top, with the pool's current block-change number: `true`
    /// when the work in flight is for an older block and must be retired.
    pub fn block_changed(&mut self, clean_jobs: u64) -> bool {
        if clean_jobs == self.seen_clean {
            return false;
        }
        self.seen_clean = clean_jobs;
        true
    }

    /// Work with notify number `seq` was fetched: is it newer than what
    /// the chips have?
    pub fn is_fresh(&self, seq: u64) -> bool {
        seq != self.seen_notify
    }

    /// Work with notify number `seq` for block number `clean_seq` is
    /// about to be submitted: `true` when the chips still hold work for
    /// an older block, which must be retired first. Records both numbers.
    pub fn submitting(&mut self, seq: u64, clean_seq: u64) -> bool {
        let retire = clean_seq != self.seen_clean;
        self.seen_notify = seq;
        self.seen_clean = clean_seq;
        retire
    }

    /// Notify number of the work last submitted.
    pub fn seen_notify(&self) -> u64 {
        self.seen_notify
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_change_seen_at_the_loop_top_is_retired_once() {
        let mut g = WorkGate::new(5);
        assert!(!g.block_changed(5));
        assert!(g.block_changed(6), "new block at the loop top");
        // The fetched work is for that block: no second retire.
        assert!(g.is_fresh(41));
        assert!(!g.submitting(41, 6));
        assert!(!g.block_changed(6));
    }

    #[test]
    fn a_block_change_between_loop_top_and_fetch_is_retired_once() {
        let mut g = WorkGate::new(5);
        assert!(!g.block_changed(5), "loop top still sees the old block");
        // The fetch returns work for block 6 (the pool thread published
        // it after the loop top).
        assert!(g.submitting(42, 6), "retire before submitting");
        // Next loop top sees 6 as well: the fresh work must survive.
        assert!(!g.block_changed(6), "the double retire of 2026-09-27");
        assert!(!g.is_fresh(42));
    }

    #[test]
    fn refresh_jobs_never_retire_and_stale_feeds_are_not_fresh() {
        let mut g = WorkGate::new(3);
        assert!(!g.submitting(10, 3));
        assert!(!g.submitting(11, 3), "refresh on the same block");
        assert!(!g.is_fresh(11));
        assert!(g.is_fresh(12));
        assert_eq!(g.seen_notify(), 11);
        // Counter bumped before the feed was swapped: the old feed comes
        // back with its old number and is not fresh.
        assert!(!g.is_fresh(11));
    }

    #[test]
    fn two_block_changes_in_a_row_retire_twice() {
        let mut g = WorkGate::new(0);
        assert!(g.submitting(1, 1));
        assert!(g.block_changed(2));
        assert!(!g.submitting(2, 2));
        assert!(g.submitting(3, 3));
    }
}

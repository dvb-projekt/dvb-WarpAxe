//! A pool that keeps the connection and stops sending new jobs.
//!
//! Seen 2026-11-20 on four Bitaxe Gammas at a ckpool-based BCH pool: the
//! pool sent `mining.ping` every ten seconds and no `mining.notify` for
//! over an hour. ckpool does that while its node gives it no new block
//! template, "to keep [the miners] connected while bitcoind recovers".
//! The miner kept hashing the last job; the read timeout never ran out,
//! because the pings are data.
//!
//! Pools send a fresh job at least every minute or so (ckpool every 30 s,
//! with a new block at once), so ten minutes without one means the pool
//! is stuck. The session then ends and the miner reconnects. If the next
//! session also brings no second job within five minutes, the miner
//! switches to the other pool (the fallback, or back to the main pool).
//! A second job in a session clears the strike.

/// No new job for this long ends a session that has had work (seconds).
pub const LIMIT_S: u64 = 600;
/// The limit for the session right after such an end (seconds).
pub const RETRY_LIMIT_S: u64 = 300;

/// What to do after a session ended for lack of new jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Reconnect to the same pool, with [`RETRY_LIMIT_S`].
    Reconnect,
    /// Switch to the other pool.
    SwitchPool,
}

/// Strikes against the pool in use.
#[derive(Debug, Default, Clone)]
pub struct Watch {
    strikes: u8,
}

impl Watch {
    /// No strikes.
    pub const fn new() -> Self {
        Self { strikes: 0 }
    }

    /// The limit for the next session (seconds).
    pub fn limit_s(&self) -> u64 {
        if self.strikes == 0 {
            LIMIT_S
        } else {
            RETRY_LIMIT_S
        }
    }

    /// A session ended for lack of new jobs. `other_pool` says whether
    /// there is another pool to switch to.
    pub fn stale(&mut self, other_pool: bool) -> Verdict {
        if self.strikes >= 1 && other_pool {
            self.strikes = 0;
            Verdict::SwitchPool
        } else {
            self.strikes = 1;
            Verdict::Reconnect
        }
    }

    /// The pool sent a second job in a session: it is producing work.
    pub fn fresh(&mut self) {
        self.strikes = 0;
    }

    /// Settings changed or the pool was switched for another reason.
    pub fn reset(&mut self) {
        self.strikes = 0;
    }
}

/// Whether a session that last saw a job `since_job_s` seconds ago has
/// gone stale under `limit_s`. Only sessions that had work count: until
/// the first job the setup timeout applies.
pub fn is_stale(had_work: bool, since_job_s: u64, limit_s: u64) -> bool {
    had_work && since_job_s >= limit_s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stale_end_reconnects_second_switches() {
        let mut w = Watch::default();
        assert_eq!(w.limit_s(), LIMIT_S);
        assert_eq!(w.stale(true), Verdict::Reconnect);
        assert_eq!(w.limit_s(), RETRY_LIMIT_S);
        assert_eq!(w.stale(true), Verdict::SwitchPool);
        // The other pool starts with the full limit.
        assert_eq!(w.limit_s(), LIMIT_S);
    }

    #[test]
    fn without_another_pool_it_keeps_reconnecting() {
        let mut w = Watch::default();
        assert_eq!(w.stale(false), Verdict::Reconnect);
        assert_eq!(w.stale(false), Verdict::Reconnect);
        assert_eq!(w.limit_s(), RETRY_LIMIT_S);
    }

    #[test]
    fn a_second_job_clears_the_strike() {
        let mut w = Watch::default();
        w.stale(true);
        w.fresh();
        assert_eq!(w.limit_s(), LIMIT_S);
        assert_eq!(w.stale(true), Verdict::Reconnect);
    }

    #[test]
    fn stale_only_after_work() {
        assert!(!is_stale(false, 10_000, LIMIT_S));
        assert!(!is_stale(true, LIMIT_S - 1, LIMIT_S));
        assert!(is_stale(true, LIMIT_S, LIMIT_S));
    }
}

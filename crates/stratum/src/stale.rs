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
//! session brings no fresh job within five minutes, the miner switches to
//! the other pool (the fallback, or back to the main pool). A job that
//! comes at least [`FRESH_AFTER_S`] after the session's first one is
//! fresh work and clears the strike; the jobs a pool hands out right after
//! the connect do not count, since a stuck pool sends those too.
//!
//! Sv2 sessions use the same limits: Sv2 pools also refresh the job every
//! 30 s to two minutes without a new block (Braiins and Blitzpool,
//! measured over two hours with a 77 min block gap on 2026-11-20), and a
//! stuck one keeps answering shares instead of pinging.
//!
//! A main pool left this way stays marked: when the probe brings the miner
//! back from the fallback, it gets only the short limit, and the firmware
//! waits longer before the next probe. Fresh work from it clears the mark.

/// No new job for this long ends a session that has had work (seconds).
pub const LIMIT_S: u64 = 600;
/// The limit after such an end, and for a main pool marked stuck (seconds).
pub const RETRY_LIMIT_S: u64 = 300;
/// A job this long after the session's first one is fresh work (seconds).
pub const FRESH_AFTER_S: u64 = 60;
/// A session with no job at all this long after the connect ends, even
/// while the pool keeps talking (pings), and counts as a failed attempt
/// (seconds).
pub const FIRST_JOB_S: u64 = 60;

/// What to do after a session ended for lack of new jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Reconnect to the same pool, with [`RETRY_LIMIT_S`].
    Reconnect,
    /// Switch to the other pool.
    SwitchPool,
}

/// Strikes against the pool in use, and whether the main pool was left
/// for sending no new jobs.
#[derive(Debug, Default, Clone)]
pub struct Watch {
    strikes: u8,
    main_stuck: bool,
}

impl Watch {
    /// No strikes, no mark.
    pub const fn new() -> Self {
        Self {
            strikes: 0,
            main_stuck: false,
        }
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
    /// there is another pool to switch to, `on_fallback` which pool the
    /// session was on.
    pub fn stale(&mut self, other_pool: bool, on_fallback: bool) -> Verdict {
        if self.strikes >= 1 && other_pool {
            self.strikes = 0;
            if !on_fallback {
                self.main_stuck = true;
            }
            Verdict::SwitchPool
        } else {
            self.strikes = 1;
            Verdict::Reconnect
        }
    }

    /// The pool sent fresh work. On the main pool this clears the mark.
    pub fn fresh(&mut self, on_fallback: bool) {
        self.strikes = 0;
        if !on_fallback {
            self.main_stuck = false;
        }
    }

    /// Whether the main pool was left for sending no new jobs and has not
    /// sent fresh work since.
    pub fn main_stuck(&self) -> bool {
        self.main_stuck
    }

    /// The probe brought the miner back to the main pool: a marked one
    /// gets the short limit straight away.
    pub fn back_to_main(&mut self) {
        self.strikes = u8::from(self.main_stuck);
    }

    /// The pool was switched for another reason (failures, an outage):
    /// the other pool starts with the full limit. The mark stays.
    pub fn switched(&mut self) {
        self.strikes = 0;
    }

    /// Settings changed: everything starts over.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// Whether a session that last saw a job `since_job_s` seconds ago has
/// gone stale under `limit_s`. Only sessions that had work count; see
/// [`no_first_job`] for the others.
pub fn is_stale(had_work: bool, since_job_s: u64, limit_s: u64) -> bool {
    had_work && since_job_s >= limit_s
}

/// Whether a session without any job `since_connect_s` seconds after the
/// connect should end.
pub fn no_first_job(had_work: bool, since_connect_s: u64) -> bool {
    !had_work && since_connect_s >= FIRST_JOB_S
}

/// Whether a job `since_first_s` seconds after the session's first one is
/// fresh work.
pub fn is_fresh(since_first_s: u64) -> bool {
    since_first_s >= FRESH_AFTER_S
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stale_end_reconnects_second_switches() {
        let mut w = Watch::default();
        assert_eq!(w.limit_s(), LIMIT_S);
        assert_eq!(w.stale(true, false), Verdict::Reconnect);
        assert_eq!(w.limit_s(), RETRY_LIMIT_S);
        assert_eq!(w.stale(true, false), Verdict::SwitchPool);
        // The other pool starts with the full limit; the main is marked.
        assert_eq!(w.limit_s(), LIMIT_S);
        assert!(w.main_stuck());
    }

    #[test]
    fn without_another_pool_it_keeps_reconnecting() {
        let mut w = Watch::default();
        assert_eq!(w.stale(false, false), Verdict::Reconnect);
        assert_eq!(w.stale(false, false), Verdict::Reconnect);
        assert_eq!(w.limit_s(), RETRY_LIMIT_S);
        assert!(!w.main_stuck());
    }

    #[test]
    fn fresh_work_clears_the_strike_and_on_the_main_the_mark() {
        let mut w = Watch::default();
        w.stale(true, false);
        w.stale(true, false);
        assert!(w.main_stuck());
        w.fresh(true);
        assert!(
            w.main_stuck(),
            "fresh work on the fallback says nothing about the main"
        );
        w.fresh(false);
        assert!(!w.main_stuck());
        assert_eq!(w.limit_s(), LIMIT_S);
    }

    #[test]
    fn a_marked_main_gets_the_short_limit_after_the_probe() {
        let mut w = Watch::default();
        w.stale(true, false);
        w.stale(true, false);
        w.back_to_main();
        assert_eq!(w.limit_s(), RETRY_LIMIT_S);
        // Still stuck: straight back to the fallback after one stale end.
        assert_eq!(w.stale(true, false), Verdict::SwitchPool);
        let mut healthy = Watch::default();
        healthy.back_to_main();
        assert_eq!(healthy.limit_s(), LIMIT_S);
    }

    #[test]
    fn a_switch_for_other_reasons_gives_the_full_limit() {
        let mut w = Watch::default();
        w.stale(true, false);
        w.switched();
        assert_eq!(w.limit_s(), LIMIT_S);
        w.reset();
        assert!(!w.main_stuck());
    }

    #[test]
    fn stale_only_after_work() {
        assert!(!is_stale(false, 10_000, LIMIT_S));
        assert!(!is_stale(true, LIMIT_S - 1, LIMIT_S));
        assert!(is_stale(true, LIMIT_S, LIMIT_S));
    }

    #[test]
    fn a_session_that_never_gets_a_job_ends() {
        assert!(!no_first_job(false, FIRST_JOB_S - 1));
        assert!(no_first_job(false, FIRST_JOB_S));
        assert!(!no_first_job(true, 10_000));
    }

    #[test]
    fn jobs_right_after_the_connect_are_not_fresh() {
        assert!(!is_fresh(0));
        assert!(!is_fresh(FRESH_AFTER_S - 1));
        assert!(is_fresh(FRESH_AFTER_S));
    }
}

//! No pool answers: when the chips go off, and when they come back.
//!
//! A miner without a pool hashes for nothing at full power. The firmware
//! switches the chips off once the main pool and, where one is set, the
//! fallback pool have both stopped answering: at least [`MIN_FAILS`]
//! failed attempts on each and [`MIN_OUTAGE_S`] since the first failure,
//! so a Wi-Fi hiccup or a pool restart does not switch anything. While
//! the pools are out, both are tried in turn every [`PROBE_S`]; the
//! first job from either brings the chips back at the clock and voltage
//! they had. AxeOS (v2.15.4) and ForgeOS (v1.8) do the same and report
//! it as `poolsUnavailable`; so does this.
//!
//! The rule only: the firmware's connect loop reports failed attempts
//! and jobs, time (uptime, s) is passed in.

/// Failed attempts in a row on a pool before it counts as not answering.
pub const MIN_FAILS: u32 = 2;
/// Seconds from the first failed attempt before the chips go off.
pub const MIN_OUTAGE_S: u64 = 60;
/// While the pools are out: seconds between two rounds of attempts.
pub const PROBE_S: u64 = 30;

/// The pools' state changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Neither pool answers: chips off.
    Unavailable,
    /// A pool sent work again after `off_s` seconds: chips back on.
    Available {
        /// How long the pools were out, s.
        off_s: u64,
    },
}

/// While the pools are out, after a failed attempt: whether the next one
/// goes to the fallback pool, and how long to wait before it, s. Both
/// pools in turn, the main pool first, then a pause of [`PROBE_S`]; one
/// pool alone every [`PROBE_S`].
pub const fn next_probe(was_fallback: bool, has_fallback: bool) -> (bool, u64) {
    if has_fallback && !was_fallback {
        (true, 0)
    } else {
        (false, PROBE_S)
    }
}

/// The connect loop's memory of the outage.
#[derive(Debug, Clone, Copy, Default)]
pub struct PoolWatch {
    main_fails: u32,
    fallback_fails: u32,
    /// The first failed attempt since a pool last sent work, uptime s.
    failing_since: Option<u64>,
    /// Since when the pools count as out, uptime s.
    out_since: Option<u64>,
}

impl PoolWatch {
    /// Nothing failed yet, as after a boot.
    pub const fn new() -> Self {
        Self {
            main_fails: 0,
            fallback_fails: 0,
            failing_since: None,
            out_since: None,
        }
    }

    /// Neither pool answers; the chips are (to be) off.
    pub const fn unavailable(&self) -> bool {
        self.out_since.is_some()
    }

    /// An attempt on a pool failed: no connection, or a session that
    /// ended before it had run a minute. `has_fallback` says a fallback
    /// pool is set and switched on.
    pub fn failed(&mut self, now_s: u64, fallback: bool, has_fallback: bool) -> Option<Change> {
        if fallback {
            self.fallback_fails = self.fallback_fails.saturating_add(1);
        } else {
            self.main_fails = self.main_fails.saturating_add(1);
        }
        self.failing_since.get_or_insert(now_s);
        self.tick(now_s, has_fallback)
    }

    /// Time passed without an attempt ending (a backoff, a long connect
    /// timeout): the minute may be over now.
    pub fn tick(&mut self, now_s: u64, has_fallback: bool) -> Option<Change> {
        if self.out_since.is_some() {
            return None;
        }
        let since = self.failing_since?;
        let both =
            self.main_fails >= MIN_FAILS && (!has_fallback || self.fallback_fails >= MIN_FAILS);
        if both && now_s.saturating_sub(since) >= MIN_OUTAGE_S {
            self.out_since = Some(now_s);
            return Some(Change::Unavailable);
        }
        None
    }

    /// A pool sent work: it answers.
    pub fn answered(&mut self, now_s: u64) -> Option<Change> {
        self.main_fails = 0;
        self.fallback_fails = 0;
        self.failing_since = None;
        self.out_since.take().map(|t| Change::Available {
            off_s: now_s.saturating_sub(t),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The connect loop's attempts before an outage is called: failures
    /// 3 s, 6 s, 12 s … apart on the main pool, the fallback after three.
    fn fail_main(w: &mut PoolWatch, at: &[u64], has_fallback: bool) -> Vec<(u64, Change)> {
        at.iter()
            .filter_map(|&t| w.failed(t, false, has_fallback).map(|c| (t, c)))
            .collect()
    }

    #[test]
    fn one_pool_out_for_a_minute_with_several_failures_switches_off() {
        let mut w = PoolWatch::new();
        assert!(fail_main(&mut w, &[100, 103, 109, 121, 145], false).is_empty());
        assert!(!w.unavailable());
        // In the next backoff the minute is over.
        assert_eq!(w.tick(159, false), None);
        assert_eq!(w.tick(160, false), Some(Change::Unavailable));
        assert!(w.unavailable());
        // Said once.
        assert_eq!(w.tick(161, false), None);
        assert_eq!(w.failed(190, false, false), None);
    }

    #[test]
    fn with_a_fallback_both_have_to_fail() {
        let mut w = PoolWatch::new();
        assert!(fail_main(&mut w, &[0, 3, 9], true).is_empty());
        // The main pool alone, however long: the fallback has not been
        // tried twice yet.
        assert_eq!(w.tick(500, true), None);
        assert_eq!(w.failed(501, true, true), None);
        assert_eq!(w.failed(525, true, true), Some(Change::Unavailable));
    }

    #[test]
    fn a_short_outage_switches_nothing() {
        let mut w = PoolWatch::new();
        // Wi-Fi gone for half a minute: three failures, then work again.
        assert!(fail_main(&mut w, &[0, 3, 9, 21], false).is_empty());
        assert_eq!(w.answered(40), None);
        assert!(!w.unavailable());
        // The count starts over: the next failures are a new outage.
        assert!(fail_main(&mut w, &[1000, 1003], false).is_empty());
        assert_eq!(w.tick(1059, false), None);
        assert_eq!(w.tick(1060, false), Some(Change::Unavailable));
    }

    #[test]
    fn one_failure_is_not_an_outage_however_long_it_takes() {
        let mut w = PoolWatch::new();
        assert_eq!(w.failed(0, false, false), None);
        assert_eq!(w.tick(10_000, false), None);
    }

    #[test]
    fn the_first_job_brings_the_chips_back() {
        let mut w = PoolWatch::new();
        fail_main(&mut w, &[0, 3, 9, 21, 45], false);
        assert_eq!(w.tick(60, false), Some(Change::Unavailable));
        // Probes every 30 s fail on.
        for t in [90, 120, 150] {
            assert_eq!(w.failed(t, false, false), None);
        }
        assert_eq!(w.answered(185), Some(Change::Available { off_s: 125 }));
        assert!(!w.unavailable());
        assert_eq!(w.answered(186), None);
    }

    #[test]
    fn while_out_both_pools_are_tried_in_turn_every_half_minute() {
        // Main failed: the fallback at once; fallback failed: the main
        // pool after the pause.
        assert_eq!(next_probe(false, true), (true, 0));
        assert_eq!(next_probe(true, true), (false, PROBE_S));
        // One pool: every half minute.
        assert_eq!(next_probe(false, false), (false, PROBE_S));
        // The fallback was switched off in the settings meanwhile.
        assert_eq!(next_probe(true, false), (false, PROBE_S));
    }

    #[test]
    fn a_fallback_taken_out_of_the_config_no_longer_has_to_fail() {
        let mut w = PoolWatch::new();
        fail_main(&mut w, &[0, 3, 9], true);
        assert_eq!(w.tick(100, true), None);
        assert_eq!(w.tick(100, false), Some(Change::Unavailable));
    }
}

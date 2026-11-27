//! One chip silent while the others hash: when to start the chain over.
//!
//! The watchdog in [`crate::Chain`] flags a chip silent; the firmware
//! already restarts the chain when *every* chip is silent for a minute.
//! A single chip that drops out mid-run (seen on a NerdOctaxe after 4.9
//! days at a steady clock) stayed dark until the next boot, and a reboot
//! brought it back. This decides when that restart happens for one chip:
//!
//! - after [`SILENT_S`] of silence with work going out,
//! - at most once per [`SPACING_S`], over all chips and shared with the
//!   domain-latch reset ([`LoneSilence::note_restart`]),
//! - and not again for a chip whose last [`MAX_TRIES`] restarts each left
//!   it silent again before it had run [`SPACING_S`]: that chip is given
//!   up until the next boot.
//!
//! A board whose nonce-to-chip attribution has not been seen on a device
//! (`Profile::silent_chip_restart` false: the NerdQAxe+ until it has
//! run; the Naja Duo was one until its nonces had been counted on the
//! bench, 2026-11-27) gets the policy in report-only mode
//! ([`LoneSilence::report_only`]):
//! the same ten minutes of silence yield a [`Verdict::Report`] instead of
//! a restart, once per ten minutes for as long as the chip stays silent,
//! and nothing is counted against the chip. A wrong attribution would
//! otherwise call a hashing chip silent and restart a healthy chain.
//!
//! Time is uptime in seconds, passed in, so the policy runs under
//! `cargo test`.

use crate::MAX_CHIPS;

/// Silence of one chip before the chain is started over, s.
pub const SILENT_S: u64 = 600;
/// Least time between two such restarts, and the time a chip has to run
/// after one for it to count as a success, s.
pub const SPACING_S: u64 = 3600;
/// Restarts in a row that did not keep a chip running before it is
/// given up.
pub const MAX_TRIES: u8 = 3;

/// What to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Start the chain over for this chip.
    Restart {
        /// The silent chip, as the watchdog counts them (from 0).
        chip: u8,
    },
    /// Stop trying for this chip until the next boot.
    GiveUp {
        /// The chip given up (from 0).
        chip: u8,
    },
    /// Report-only mode: this chip has been silent for [`SILENT_S`]
    /// (again); nothing is restarted. Repeated every [`SILENT_S`] while
    /// the silence lasts.
    Report {
        /// The silent chip (from 0).
        chip: u8,
    },
}

/// The policy's memory; lives across chain restarts, not across boots.
#[derive(Debug, Clone)]
pub struct LoneSilence {
    silent_since: [Option<u64>; MAX_CHIPS],
    /// Restarts for the chip that did not (yet) keep it running.
    tries: [u8; MAX_CHIPS],
    /// Running again since, with work going out, after a restart for it.
    back_since: [Option<u64>; MAX_CHIPS],
    given_up: [bool; MAX_CHIPS],
    last_restart: Option<u64>,
    /// Chips on the chain at the last look: a chain that came back with
    /// another count numbers its chips differently.
    chips: usize,
    /// Clock (MHz) and core voltage (mV) the counts belong to.
    point: Option<(u32, u16)>,
    /// Each chip's clock offset (MHz) its counts belong to.
    offsets: [i16; MAX_CHIPS],
    /// Report silences, never restart for them.
    report_only: bool,
}

impl Default for LoneSilence {
    fn default() -> Self {
        Self::new()
    }
}

impl LoneSilence {
    /// Nothing seen yet, as after a boot.
    pub const fn new() -> Self {
        Self {
            silent_since: [None; MAX_CHIPS],
            tries: [0; MAX_CHIPS],
            back_since: [None; MAX_CHIPS],
            given_up: [false; MAX_CHIPS],
            last_restart: None,
            chips: 0,
            point: None,
            offsets: [0; MAX_CHIPS],
            report_only: false,
        }
    }

    /// The policy that only reports ([`Verdict::Report`]): for a board
    /// whose per-chip nonce attribution is not confirmed yet.
    pub const fn report_only() -> Self {
        Self {
            report_only: true,
            ..Self::new()
        }
    }

    /// Whether this policy only reports.
    pub const fn is_report_only(&self) -> bool {
        self.report_only
    }

    /// Chips given up until the next boot.
    pub fn given_up(&self) -> impl Iterator<Item = u8> + '_ {
        (0..MAX_CHIPS as u8).filter(|&i| self.given_up[usize::from(i)])
    }

    /// The clock and core voltage set now. A new point (a tuner moving
    /// on, a change by hand) is a fresh chance for every chip: what went
    /// silent at the old point says nothing about this one.
    pub fn set_point(&mut self, mhz: u32, mv: u16) {
        if self.point.is_some_and(|p| p != (mhz, mv)) {
            self.tries = [0; MAX_CHIPS];
            self.back_since = [None; MAX_CHIPS];
            self.given_up = [false; MAX_CHIPS];
        }
        self.point = Some((mhz, mv));
    }

    /// Each chip's own clock offset now, MHz (chain order; a chip the
    /// list does not reach has none). A chip whose offset changed is at
    /// a new point of its own and gets its fresh chance; the others
    /// keep their counts.
    pub fn set_chip_offsets(&mut self, offsets_mhz: &[i16]) {
        for i in 0..MAX_CHIPS {
            let offset = offsets_mhz.get(i).copied().unwrap_or(0);
            if self.offsets[i] != offset {
                self.offsets[i] = offset;
                self.tries[i] = 0;
                self.back_since[i] = None;
                self.given_up[i] = false;
            }
        }
    }

    /// Another intervention on the chain happened now (the domain-latch
    /// reset): the hour between two restarts counts from it, so the
    /// firmware intervenes at most once an hour in all.
    pub fn note_restart(&mut self, now_s: u64) {
        self.last_restart = Some(now_s);
    }

    /// One look at the chain. `silent` holds the watchdog's flag per chip
    /// on the chain; `working` says work is going out right now (pool up,
    /// chain past its bring-up). Without work a chip shows nothing either
    /// way, so both clocks stop: the silence and the hour of running
    /// after a restart. The all-chips case is not this policy's: it
    /// returns nothing then.
    pub fn observe(&mut self, now_s: u64, working: bool, silent: &[bool]) -> Option<Verdict> {
        let n = silent.len().min(MAX_CHIPS);
        if n != self.chips {
            self.silent_since = [None; MAX_CHIPS];
            self.back_since = [None; MAX_CHIPS];
            self.chips = n;
        }
        let all = n > 0 && silent[..n].iter().all(|&s| s);
        for (i, &s) in silent[..n].iter().enumerate() {
            if !working || all {
                self.silent_since[i] = None;
                self.back_since[i] = None;
            } else if s {
                self.silent_since[i].get_or_insert(now_s);
                self.back_since[i] = None;
            } else {
                self.silent_since[i] = None;
                if self.tries[i] > 0 {
                    let since = *self.back_since[i].get_or_insert(now_s);
                    if now_s.saturating_sub(since) >= SPACING_S {
                        self.tries[i] = 0;
                        self.back_since[i] = None;
                    }
                }
            }
        }
        let spaced = self
            .last_restart
            .is_none_or(|t| now_s.saturating_sub(t) >= SPACING_S);
        for i in 0..n {
            let Some(since) = self.silent_since[i] else {
                continue;
            };
            if self.given_up[i] || now_s.saturating_sub(since) < SILENT_S {
                continue;
            }
            if self.report_only {
                // Said once per ten minutes of silence; nothing else moves.
                self.silent_since[i] = Some(now_s);
                return Some(Verdict::Report { chip: i as u8 });
            }
            if self.tries[i] >= MAX_TRIES {
                self.given_up[i] = true;
                self.silent_since[i] = None;
                return Some(Verdict::GiveUp { chip: i as u8 });
            }
            if !spaced {
                continue;
            }
            self.tries[i] += 1;
            self.last_restart = Some(now_s);
            // The restart clears every flag; nothing carries over.
            self.silent_since = [None; MAX_CHIPS];
            return Some(Verdict::Restart { chip: i as u8 });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EIGHT_OK: [bool; 8] = [false; 8];

    fn one(chip: usize) -> [bool; 8] {
        let mut s = EIGHT_OK;
        s[chip] = true;
        s
    }

    /// Feed `flags` once a second from `from` to `to` (inclusive).
    fn run(p: &mut LoneSilence, from: u64, to: u64, flags: &[bool]) -> Vec<(u64, Verdict)> {
        (from..=to)
            .filter_map(|t| p.observe(t, true, flags).map(|v| (t, v)))
            .collect()
    }

    #[test]
    fn a_chip_silent_for_ten_minutes_restarts_the_chain_once() {
        let mut p = LoneSilence::new();
        let v = run(&mut p, 0, 900, &one(4));
        assert_eq!(v, vec![(SILENT_S, Verdict::Restart { chip: 4 })]);
    }

    #[test]
    fn a_short_silence_does_nothing() {
        let mut p = LoneSilence::new();
        assert!(run(&mut p, 0, 599, &one(2)).is_empty());
        assert!(run(&mut p, 600, 700, &EIGHT_OK).is_empty());
        // The clock starts over after the chip came back.
        assert!(run(&mut p, 701, 1300, &one(2)).is_empty());
        assert_eq!(
            run(&mut p, 1301, 1301, &one(2)),
            vec![(1301, Verdict::Restart { chip: 2 })]
        );
    }

    #[test]
    fn all_chips_silent_is_left_to_the_chain_restart() {
        let mut p = LoneSilence::new();
        assert!(run(&mut p, 0, 2000, &[true; 8]).is_empty());
    }

    #[test]
    fn no_work_going_out_stops_the_clock() {
        let mut p = LoneSilence::new();
        for t in 0..=2000 {
            assert_eq!(p.observe(t, false, &one(1)), None);
        }
        assert_eq!(
            run(&mut p, 2001, 2601, &one(1)).last().map(|v| v.0),
            Some(2601)
        );
    }

    #[test]
    fn restarts_are_an_hour_apart_over_all_chips() {
        let mut p = LoneSilence::new();
        assert_eq!(
            run(&mut p, 0, 600, &one(4)),
            vec![(600, Verdict::Restart { chip: 4 })]
        );
        // Another chip drops out right after: it waits for the hour.
        let v = run(&mut p, 601, 5000, &one(6));
        assert_eq!(
            v.first(),
            Some(&(600 + SPACING_S, Verdict::Restart { chip: 6 }))
        );
    }

    #[test]
    fn a_domain_latch_reset_counts_against_the_hour() {
        let mut p = LoneSilence::new();
        assert!(run(&mut p, 0, 500, &one(4)).is_empty());
        p.note_restart(500);
        let v = run(&mut p, 501, 500 + SPACING_S + 10, &one(4));
        assert_eq!(v, vec![(500 + SPACING_S, Verdict::Restart { chip: 4 })]);
    }

    #[test]
    fn a_chip_that_runs_an_hour_after_its_restart_starts_fresh() {
        let mut p = LoneSilence::new();
        let mut t = 0;
        for _ in 0..10 {
            let v = run(&mut p, t, t + 600, &one(4));
            assert_eq!(v, vec![(t + 600, Verdict::Restart { chip: 4 })]);
            // Back, and running for the full hour.
            assert!(run(&mut p, t + 601, t + 601 + SPACING_S, &EIGHT_OK).is_empty());
            t += 602 + SPACING_S;
        }
        assert_eq!(p.given_up().count(), 0);
    }

    #[test]
    fn three_restarts_that_do_not_hold_give_the_chip_up() {
        let mut p = LoneSilence::new();
        // Back for a few minutes after each restart, then silent again.
        let mut t = 0;
        let mut restarts = 0;
        let mut gave_up_at = None;
        while t < 6 * SPACING_S {
            let flags = if p.last_restart.is_some_and(|r| t - r < 300) {
                EIGHT_OK
            } else {
                one(5)
            };
            match p.observe(t, true, &flags) {
                Some(Verdict::Restart { chip: 5 }) => restarts += 1,
                Some(Verdict::GiveUp { chip: 5 }) => gave_up_at = Some(t),
                None => {}
                other => panic!("unexpected {other:?}"),
            }
            t += 1;
        }
        assert_eq!(restarts, MAX_TRIES);
        assert!(gave_up_at.is_some());
        assert_eq!(p.given_up().collect::<Vec<_>>(), vec![5]);
        // Given up: no restart for it any more, but another chip still gets one.
        let v = run(&mut p, t, t + SPACING_S, &{
            let mut s = one(5);
            s[1] = true;
            s
        });
        assert_eq!(
            v.iter()
                .filter(|(_, v)| *v == Verdict::Restart { chip: 1 })
                .count(),
            1
        );
        assert!(v.iter().all(|(_, v)| !matches!(
            v,
            Verdict::Restart { chip: 5 } | Verdict::GiveUp { chip: 5 }
        )));
    }

    #[test]
    fn more_flags_than_chips_are_ignored() {
        let mut p = LoneSilence::new();
        let mut flags = [false; MAX_CHIPS + 4];
        flags[MAX_CHIPS + 1] = true;
        assert!(run(&mut p, 0, 700, &flags).is_empty());
        flags[3] = true;
        let v = run(&mut p, 701, 1400, &flags);
        assert_eq!(v, vec![(701 + SILENT_S, Verdict::Restart { chip: 3 })]);
    }

    #[test]
    fn without_work_the_hour_after_a_restart_does_not_run() {
        let mut p = LoneSilence::new();
        assert_eq!(run(&mut p, 0, 600, &one(4)).len(), 1);
        // Pool down for two hours right after the restart: no flags, but
        // no work either; that is not an hour of running.
        for t in 601..=601 + 2 * SPACING_S {
            assert_eq!(p.observe(t, false, &EIGHT_OK), None);
        }
        assert_eq!(p.tries[4], 1);
        // A chip already silent when the pool drops: its clock stops too.
        let t0 = 602 + 2 * SPACING_S;
        assert!(run(&mut p, t0, t0 + 300, &one(2)).is_empty());
        for t in t0 + 301..=t0 + 2000 {
            assert_eq!(p.observe(t, false, &one(2)), None);
        }
        let v = run(&mut p, t0 + 2001, t0 + 2001 + SILENT_S, &one(2));
        assert_eq!(
            v,
            vec![(t0 + 2001 + SILENT_S, Verdict::Restart { chip: 2 })]
        );
    }

    #[test]
    fn silent_again_one_second_before_the_hour_counts_against_the_chip() {
        let mut p = LoneSilence::new();
        assert_eq!(run(&mut p, 0, 600, &one(4)).len(), 1);
        assert!(run(&mut p, 601, 600 + SPACING_S, &EIGHT_OK).is_empty());
        assert_eq!(p.tries[4], 1);
        assert_eq!(
            run(&mut p, 601 + SPACING_S, 601 + SPACING_S, &EIGHT_OK),
            vec![]
        );
        assert_eq!(p.tries[4], 0);
    }

    #[test]
    fn a_chain_back_with_fewer_chips_starts_every_clock_over() {
        let mut p = LoneSilence::new();
        assert!(run(&mut p, 0, 500, &one(6)).is_empty());
        // Seven chips now: index 6 is another chip; its clock starts anew.
        let mut seven = [false; 7];
        seven[6] = true;
        assert!(run(&mut p, 501, 1100, &seven).is_empty());
        assert_eq!(
            run(&mut p, 1101, 1101, &seven),
            vec![(1101, Verdict::Restart { chip: 6 })]
        );
    }

    #[test]
    fn a_new_point_gives_a_given_up_chip_another_chance() {
        let mut p = LoneSilence::new();
        p.set_point(700, 1210);
        let mut t = 0;
        while p.given_up().count() == 0 {
            p.observe(t, true, &one(5));
            t += 1;
        }
        // The same point again changes nothing.
        p.set_point(700, 1210);
        assert_eq!(p.given_up().collect::<Vec<_>>(), vec![5]);
        p.set_point(650, 1180);
        assert_eq!(p.given_up().count(), 0);
        let v = run(&mut p, t, t + SPACING_S + SILENT_S, &one(5));
        assert!(v.iter().any(|(_, v)| *v == Verdict::Restart { chip: 5 }));
    }

    #[test]
    fn a_new_clock_offset_gives_that_chip_alone_another_chance() {
        let mut p = LoneSilence::new();
        p.set_point(625, 1130);
        p.set_chip_offsets(&[0, 0, -25]);
        let mut t = 0;
        while p.given_up().count() == 0 {
            p.observe(t, true, &one(5));
            t += 1;
        }
        // The same offsets again, and another chip's: chip 5 stays
        // given up.
        p.set_chip_offsets(&[0, 0, -25]);
        p.set_chip_offsets(&[0, 0, -50]);
        assert_eq!(p.given_up().collect::<Vec<_>>(), vec![5]);
        // Its own: a fresh chance, as after a new point.
        p.set_chip_offsets(&[0, 0, -50, 0, 0, -25]);
        assert_eq!(p.given_up().count(), 0);
        assert_eq!(p.tries[5], 0);
        let v = run(&mut p, t, t + SPACING_S + SILENT_S, &one(5));
        assert!(v.iter().any(|(_, v)| *v == Verdict::Restart { chip: 5 }));
        // An offset taken away is a change too.
        p.tries[2] = 2;
        p.set_chip_offsets(&[]);
        assert_eq!(p.tries[2], 0);
    }

    /// Report-only: the same ten minutes yield a report, repeated every
    /// ten minutes while the chip stays silent; no restart, no try
    /// counted, nothing given up, however long it goes on. The hour
    /// between restarts is not touched either, so a domain-latch reset
    /// is still free to happen.
    #[test]
    fn report_only_says_so_every_ten_minutes_and_never_restarts() {
        let mut p = LoneSilence::report_only();
        assert!(p.is_report_only());
        assert!(!LoneSilence::new().is_report_only());
        let v = run(&mut p, 0, 4 * SPACING_S, &one(1));
        assert!(!v.is_empty());
        assert!(v.iter().all(|(_, v)| *v == Verdict::Report { chip: 1 }));
        let times: Vec<u64> = v.iter().map(|(t, _)| *t).collect();
        assert_eq!(times[0], SILENT_S);
        assert!(times.windows(2).all(|w| w[1] - w[0] == SILENT_S));
        assert_eq!(times.len(), (4 * SPACING_S / SILENT_S) as usize);
        assert_eq!(p.tries[1], 0);
        assert_eq!(p.given_up().count(), 0);
        assert_eq!(p.last_restart, None);
        // Back, then silent again: the clock starts over.
        assert!(run(&mut p, 4 * SPACING_S + 1, 4 * SPACING_S + 100, &EIGHT_OK).is_empty());
        let t0 = 4 * SPACING_S + 101;
        let v = run(&mut p, t0, t0 + SILENT_S, &one(1));
        assert_eq!(v, vec![(t0 + SILENT_S, Verdict::Report { chip: 1 })]);
        // All chips silent and no work are not this policy's, as before.
        assert!(run(&mut p, t0 + SILENT_S + 1, t0 + 3 * SILENT_S, &[true; 8]).is_empty());
        let mut q = LoneSilence::report_only();
        for t in 0..=2 * SILENT_S {
            assert_eq!(q.observe(t, false, &one(2)), None);
        }
    }

    #[test]
    fn a_given_up_chip_and_all_others_silent_is_the_chain_restarts_case() {
        let mut p = LoneSilence::new();
        let mut t = 0;
        while p.given_up().count() == 0 {
            p.observe(t, true, &one(5));
            t += 1;
        }
        assert!(run(&mut p, t, t + 2 * SPACING_S, &[true; 8]).is_empty());
    }
}

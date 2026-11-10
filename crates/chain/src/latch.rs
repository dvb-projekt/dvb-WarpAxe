//! One hash domain of a chip stuck while the rest of the chip hashes:
//! when to switch the chips off and on for it.
//!
//! A BM1370 has four hash domains, a BM1373 eight, each with its own
//! counter the driver reads once a second ([`crate::HashrateMonitor`]).
//! A domain can latch mid-run: its counter stops or crawls while the
//! chip's other domains and its nonces go on (a NerdOctaxe, 2026-10-09:
//! "always domain 2", at 3 % of the others). The chip is not silent, so
//! [`crate::LoneSilence`] does not see it; the self-test would name it,
//! but only when someone runs one. This decides when the firmware
//! intervenes on its own:
//!
//! - a domain below [`STUCK_PERCENT`] of the mean of the chip's other
//!   domains over a window of [`WINDOW_S`] fresh samples with work going
//!   out (the window as a whole, since the counters are noisy second by
//!   second; a weak domain at 50 % is not a latch, the self-test's limit
//!   is 67 %),
//! - sooner for a domain that is dead from the chain's start: in the
//!   first window after the chain came up ([`DomainLatch::chain_ready`]),
//!   a domain below [`EARLY_DEAD_PERCENT`] after [`EARLY_WINDOW_S`]
//!   samples does not have to wait for the window's end (two Bitaxe
//!   Gammas in the field, v0.1.41: a domain at 0 % right after the
//!   start, healed by the reset ten minutes later). A domain that is
//!   only weak, or one that dies later, has the full window,
//! - at most once per [`SPACING_S`], shared with the single-chip restart
//!   through [`DomainLatch::note_reset`],
//! - and not again for a domain whose last [`MAX_TRIES`] resets each saw
//!   it stuck again before it had run [`SPACING_S`] clean: that domain is
//!   given up until a new clock or voltage ([`DomainLatch::set_point`])
//!   or the next boot.
//!
//! The window and the clean hour count samples, not seconds, so they
//! run only while work goes out and the chip answers its counter reads:
//! a pool outage stops them. The hour between two interventions is
//! uptime, as in [`crate::lone`], since it is shared with it. Time
//! (uptime, s) is passed in, so the policy runs under `cargo test`.

use crate::monitor::MAX_DOMAINS;
use crate::MAX_CHIPS;

/// Fresh samples (working seconds) a window holds before it is judged.
pub const WINDOW_S: u32 = 600;
/// A domain below this share of the mean of the chip's other domains
/// over a whole window is stuck.
pub const STUCK_PERCENT: u32 = 25;
/// Fresh samples after which the first window of a chain is looked at
/// for a domain dead from the start.
pub const EARLY_WINDOW_S: u32 = 180;
/// A domain below this share of the mean of the chip's other domains at
/// that early look is dead: nothing a longer look would change.
pub const EARLY_DEAD_PERCENT: u32 = 5;
/// Least time between two resets (shared with the single-chip restart),
/// and the clean running a domain has to show after one for the reset to
/// count as a success, s.
pub const SPACING_S: u64 = 3600;
/// Resets in a row that did not keep a domain running before it is
/// given up.
pub const MAX_TRIES: u8 = 3;

/// What to do now. A `GiveUp` is reported once; when a `Reset` for
/// another chip falls on the same second it wins, and the give-up shows
/// only in [`DomainLatch::given_up`] (the firmware records the event
/// from that list, not from the verdict).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Switch the chips off and on for this domain.
    Reset {
        /// The chip, as the chain counts them (from 0).
        chip: u8,
        /// The stuck domain (from 0).
        domain: u8,
        /// The domain's share of the mean of the chip's other domains
        /// over the window, %.
        percent: u8,
        /// Dead from the chain's start, found at the early look after
        /// [`EARLY_WINDOW_S`] samples instead of the full window.
        early: bool,
    },
    /// Stop trying for this domain until a new point or the next boot.
    GiveUp {
        /// The chip (from 0).
        chip: u8,
        /// The domain (from 0).
        domain: u8,
    },
}

/// One chip's counters as the policy sees them, once a second.
#[derive(Debug, Clone, Copy)]
pub struct ChipSample<'a> {
    /// The watchdog's flag: no nonces for a while. A silent chip is the
    /// single-chip case, not this one.
    pub silent: bool,
    /// The counters were read within the last few seconds; stale rates
    /// are the last reading, not the chip's state now.
    pub fresh: bool,
    /// The domain rates (GH/s, any common scale), one per domain.
    pub domains: &'a [f32],
}

/// The rates of one window, summed sample by sample.
#[derive(Debug, Clone, Copy)]
struct Window {
    sum: [f32; MAX_DOMAINS],
    samples: u32,
    /// The first window of a chain that has just come up, not yet
    /// looked at early.
    early: bool,
}

impl Window {
    const EMPTY: Window = Window {
        sum: [0.0; MAX_DOMAINS],
        samples: 0,
        early: false,
    };
    /// The first window after the chain came up.
    const FIRST: Window = Window {
        early: true,
        ..Window::EMPTY
    };

    fn add(&mut self, rates: &[f32]) {
        for (s, r) in self.sum.iter_mut().zip(rates) {
            *s += r.max(0.0);
        }
        self.samples += 1;
    }

    /// The domain furthest under `limit` (percent of the mean of the
    /// others), with its share in percent (rounded down, so a reported
    /// figure is always under the limit): `None` when none is. Domains
    /// given up are out of the judgement on both sides: not candidates,
    /// and not in the mean a sibling is held against (a dead one would
    /// pull that mean down).
    fn stuck(
        &self,
        domains: usize,
        given_up: &[bool; MAX_DOMAINS],
        limit: u32,
    ) -> Option<(u8, u8)> {
        let judged = (0..domains).filter(|&d| !given_up[d]);
        let total: f32 = judged.clone().map(|d| self.sum[d]).sum();
        let count = judged.clone().count();
        if count < 2 {
            return None;
        }
        let mut worst: Option<(u8, f32)> = None;
        for d in judged {
            let own = self.sum[d];
            let others = (total - own) / (count - 1) as f32;
            if others <= 0.0 {
                continue;
            }
            let share = own / others * 100.0;
            if share < limit as f32 && worst.is_none_or(|(_, p)| share < p) {
                worst = Some((d as u8, share));
            }
        }
        worst.map(|(d, share)| (d, share as u8))
    }
}

/// The policy's memory; lives across chain restarts, not across boots.
#[derive(Debug, Clone)]
pub struct DomainLatch {
    windows: [Window; MAX_CHIPS],
    /// Resets for the domain that did not (yet) keep it running.
    tries: [[u8; MAX_DOMAINS]; MAX_CHIPS],
    /// Clean working seconds since the last reset for the domain,
    /// credited window by window.
    clean_s: [[u32; MAX_DOMAINS]; MAX_CHIPS],
    given_up: [[bool; MAX_DOMAINS]; MAX_CHIPS],
    last_reset: Option<u64>,
    /// Chips on the chain at the last look: a chain that came back with
    /// another count numbers its chips differently.
    chips: usize,
    /// Clock (MHz) and core voltage (mV) the tries belong to.
    point: Option<(u32, u16)>,
    /// A chain has come up since the last look: its windows start as
    /// first windows.
    ready: bool,
}

impl Default for DomainLatch {
    fn default() -> Self {
        Self::new()
    }
}

impl DomainLatch {
    /// Nothing seen yet, as after a boot.
    pub const fn new() -> Self {
        Self {
            windows: [Window::EMPTY; MAX_CHIPS],
            tries: [[0; MAX_DOMAINS]; MAX_CHIPS],
            clean_s: [[0; MAX_DOMAINS]; MAX_CHIPS],
            given_up: [[false; MAX_DOMAINS]; MAX_CHIPS],
            last_reset: None,
            chips: 0,
            point: None,
            ready: false,
        }
    }

    /// The chain has come up (after a boot, a chain restart, a reset of
    /// this policy's own, chips switched on): every window starts over
    /// as the chain's first, which gets the early look for a domain dead
    /// from the start.
    pub fn chain_ready(&mut self) {
        self.ready = true;
    }

    /// Domains given up, as (chip, domain), until a new point or the
    /// next boot.
    pub fn given_up(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        (0..MAX_CHIPS as u8).flat_map(move |c| {
            (0..MAX_DOMAINS as u8)
                .filter(move |&d| self.given_up[usize::from(c)][usize::from(d)])
                .map(move |d| (c, d))
        })
    }

    /// The clock and core voltage set now. A new point (a tuner moving
    /// on, a change by hand) is a fresh chance for every domain: what
    /// latched at the old point says nothing about this one. The
    /// windows stay: a domain is judged against the chip's others at
    /// the same moment, whatever the clock.
    pub fn set_point(&mut self, mhz: u32, mv: u16) {
        if self.point.is_some_and(|p| p != (mhz, mv)) {
            self.tries = [[0; MAX_DOMAINS]; MAX_CHIPS];
            self.clean_s = [[0; MAX_DOMAINS]; MAX_CHIPS];
            self.given_up = [[false; MAX_DOMAINS]; MAX_CHIPS];
        }
        self.point = Some((mhz, mv));
    }

    /// Another intervention on the chain happened now (the single-chip
    /// restart): the hour between two counts from it, so the firmware
    /// intervenes at most once an hour in all.
    pub fn note_reset(&mut self, now_s: u64) {
        self.last_reset = Some(now_s);
    }

    /// One look at the chain, once a second. `working` says work is
    /// going out right now (pool up, chain past its bring-up): without
    /// it the counters say nothing, and no clock here moves. A chip with
    /// fewer than two domains (the BM1397 reports no domain counters)
    /// has nothing to compare and is left alone.
    pub fn observe(&mut self, now_s: u64, working: bool, chips: &[ChipSample]) -> Option<Verdict> {
        let n = chips.len().min(MAX_CHIPS);
        // No chips: the chain is coming up (the firmware looks during
        // the bring-up too). Not a changed count; a chain that comes
        // back with the chips it had keeps its clean-running credit.
        if n == 0 {
            return None;
        }
        if n != self.chips {
            self.windows = [Window::EMPTY; MAX_CHIPS];
            self.clean_s = [[0; MAX_DOMAINS]; MAX_CHIPS];
            self.chips = n;
        }
        if self.ready {
            self.windows = [Window::FIRST; MAX_CHIPS];
            self.ready = false;
        }
        if !working {
            return None;
        }
        let spaced = self
            .last_reset
            .is_none_or(|t| now_s.saturating_sub(t) >= SPACING_S);
        let mut gave_up = None;
        for (i, c) in chips[..n].iter().enumerate() {
            let domains = c.domains.len().min(MAX_DOMAINS);
            if c.silent {
                // The single-chip case; what the window held before the
                // chip fell silent is not this chip as it comes back.
                self.windows[i] = Window::EMPTY;
                continue;
            }
            if !c.fresh || domains < 2 {
                continue;
            }
            self.windows[i].add(&c.domains[..domains]);
            // The early look, once, in a chain's first window: a domain
            // that is dead from the start. Anything else (weak, or fine
            // so far) goes on to the window's end.
            let mut found = None;
            if self.windows[i].early && self.windows[i].samples >= EARLY_WINDOW_S {
                self.windows[i].early = false;
                // Only for a domain that still has a try: giving one up
                // is the full window's verdict, as it always was (three
                // minutes dead after the third reset is not yet that).
                found = self.windows[i]
                    .stuck(domains, &self.given_up[i], EARLY_DEAD_PERCENT)
                    .filter(|&(d, _)| self.tries[i][usize::from(d)] < MAX_TRIES)
                    .map(|(d, percent)| (d, percent, true));
            }
            if found.is_none() {
                if self.windows[i].samples < WINDOW_S {
                    continue;
                }
                let judged = self.windows[i];
                self.windows[i] = Window::EMPTY;
                let stuck = judged.stuck(domains, &self.given_up[i], STUCK_PERCENT);
                // A clean window is credit towards the hour; a stuck one
                // sends the domain's credit back to zero.
                for d in 0..domains {
                    if stuck.is_some_and(|(sd, _)| usize::from(sd) == d) {
                        self.clean_s[i][d] = 0;
                    } else if self.tries[i][d] > 0 && !self.given_up[i][d] {
                        self.clean_s[i][d] = self.clean_s[i][d].saturating_add(judged.samples);
                        if u64::from(self.clean_s[i][d]) >= SPACING_S {
                            self.tries[i][d] = 0;
                            self.clean_s[i][d] = 0;
                        }
                    }
                }
                found = stuck.map(|(d, percent)| (d, percent, false));
            }
            let Some((d, percent, early)) = found else {
                continue;
            };
            let di = usize::from(d);
            if self.tries[i][di] >= MAX_TRIES {
                self.given_up[i][di] = true;
                // Reported after the loop, so the chips after this one
                // keep this second's sample.
                gave_up.get_or_insert(Verdict::GiveUp {
                    chip: i as u8,
                    domain: d,
                });
                continue;
            }
            if !spaced {
                // Judged again when the window is full (after an early
                // look), or the next one is.
                continue;
            }
            self.tries[i][di] += 1;
            self.clean_s[i][di] = 0;
            self.last_reset = Some(now_s);
            // The chips are switched off: no window carries over.
            self.windows = [Window::EMPTY; MAX_CHIPS];
            return Some(Verdict::Reset {
                chip: i as u8,
                domain: d,
                percent,
                early,
            });
        }
        gave_up
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One chip's four domains, all healthy at a nominal 100.
    const FINE: [f32; 4] = [100.0; 4];

    fn chip(rates: &[f32]) -> ChipSample<'_> {
        ChipSample {
            silent: false,
            fresh: true,
            domains: rates,
        }
    }

    /// Eight chips, every domain healthy.
    fn healthy() -> Vec<[f32; 4]> {
        vec![FINE; 8]
    }

    /// Eight chips with chip `c`'s domain `d` at `share` of the others.
    fn stuck(c: usize, d: usize, share: f32) -> Vec<[f32; 4]> {
        let mut v = healthy();
        v[c][d] = 100.0 * share;
        v
    }

    /// Feed `rates` once a second from `from` to `to` (inclusive).
    fn run<const D: usize>(
        p: &mut DomainLatch,
        from: u64,
        to: u64,
        rates: &[[f32; D]],
    ) -> Vec<(u64, Verdict)> {
        let samples: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
        (from..=to)
            .filter_map(|t| p.observe(t, true, &samples).map(|v| (t, v)))
            .collect()
    }

    const RESET_4_2: Verdict = Verdict::Reset {
        chip: 4,
        domain: 2,
        percent: 3,
        early: false,
    };

    #[test]
    fn a_domain_at_three_percent_for_ten_minutes_resets_the_chips_once() {
        let mut p = DomainLatch::new();
        let v = run(&mut p, 1, 1500, &stuck(4, 2, 0.03));
        assert_eq!(v, vec![(u64::from(WINDOW_S), RESET_4_2)]);
    }

    #[test]
    fn a_weak_domain_at_half_its_share_is_no_latch() {
        let mut p = DomainLatch::new();
        assert!(run(&mut p, 1, 3 * u64::from(WINDOW_S), &stuck(1, 3, 0.5)).is_empty());
        // Just under the limit over the whole window: that is one.
        let v = run(&mut p, 1, u64::from(WINDOW_S), &stuck(1, 3, 0.24));
        assert_eq!(
            v,
            vec![(
                u64::from(WINDOW_S),
                Verdict::Reset {
                    chip: 1,
                    domain: 3,
                    percent: 24,
                    early: false,
                }
            )]
        );
        // Right at it: not.
        let mut p = DomainLatch::new();
        assert!(run(&mut p, 1, 2 * u64::from(WINDOW_S), &stuck(1, 3, 0.25)).is_empty());
    }

    #[test]
    fn the_window_is_judged_as_a_whole_not_by_single_samples() {
        let mut p = DomainLatch::new();
        // Noisy counters: the domain reads 0 every other second and
        // double its share in between, which averages to healthy.
        let mut v = Vec::new();
        for t in 1..=3 * u64::from(WINDOW_S) {
            let rates = if t % 2 == 0 {
                stuck(2, 0, 0.0)
            } else {
                stuck(2, 0, 2.0)
            };
            let s: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
            v.extend(p.observe(t, true, &s));
        }
        assert!(v.is_empty(), "{v:?}");
        // Half the window fine, half dead: 50 % over the window, no
        // reset yet; the next window, all dead, is one.
        let mut p = DomainLatch::new();
        assert!(run(&mut p, 1, 300, &healthy()).is_empty());
        assert!(run(&mut p, 301, 600, &stuck(2, 0, 0.0)).is_empty());
        let v = run(&mut p, 601, 1200, &stuck(2, 0, 0.0));
        assert_eq!(
            v,
            vec![(
                1200,
                Verdict::Reset {
                    chip: 2,
                    domain: 0,
                    percent: 0,
                    early: false,
                }
            )]
        );
    }

    #[test]
    fn only_fresh_samples_fill_the_window() {
        let mut p = DomainLatch::new();
        let rates = stuck(4, 2, 0.03);
        let mut samples: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
        samples[4].fresh = false;
        // The chip's counters never answer: its window never fills.
        for t in 1..=3 * u64::from(WINDOW_S) {
            assert_eq!(p.observe(t, true, &samples), None);
        }
        samples[4].fresh = true;
        let v: Vec<_> = (1..=u64::from(WINDOW_S))
            .filter_map(|t| p.observe(5000 + t, true, &samples))
            .collect();
        assert_eq!(v, vec![RESET_4_2]);
    }

    #[test]
    fn no_work_going_out_stops_the_clock() {
        let mut p = DomainLatch::new();
        let rates = stuck(4, 2, 0.03);
        let samples: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
        assert!(run(&mut p, 1, 400, &rates).is_empty());
        for t in 401..=4000 {
            assert_eq!(p.observe(t, false, &samples), None);
        }
        // 200 more working seconds complete the window.
        let v = run(&mut p, 4001, 4300, &rates);
        assert_eq!(v, vec![(4200, RESET_4_2)]);
    }

    #[test]
    fn a_silent_chip_is_not_this_case() {
        let mut p = DomainLatch::new();
        let rates = stuck(4, 2, 0.0);
        let mut samples: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
        samples[4].silent = true;
        for t in 1..=3 * u64::from(WINDOW_S) {
            assert_eq!(p.observe(t, true, &samples), None);
        }
        // Back from silence: the window starts afresh.
        samples[4].silent = false;
        let v: Vec<_> = (1..=u64::from(WINDOW_S) - 1)
            .filter_map(|t| p.observe(2000 + t, true, &samples))
            .collect();
        assert!(v.is_empty());
        assert_eq!(
            p.observe(2000 + u64::from(WINDOW_S), true, &samples),
            Some(Verdict::Reset {
                chip: 4,
                domain: 2,
                percent: 0,
                early: false,
            })
        );
    }

    #[test]
    fn a_chip_with_one_domain_or_none_is_left_alone() {
        let mut p = DomainLatch::new();
        let one = [[0.0f32; 1]; 2];
        let samples: Vec<ChipSample> = one.iter().map(|r| chip(r)).collect();
        for t in 1..=2 * u64::from(WINDOW_S) {
            assert_eq!(p.observe(t, true, &samples), None);
        }
        let none: [[f32; 0]; 2] = [[]; 2];
        let samples: Vec<ChipSample> = none.iter().map(|r| chip(r)).collect();
        for t in 1..=2 * u64::from(WINDOW_S) {
            assert_eq!(p.observe(t, true, &samples), None);
        }
    }

    #[test]
    fn a_chip_whose_other_domains_read_nothing_is_not_judged() {
        let mut p = DomainLatch::new();
        let mut rates = healthy();
        rates[3] = [0.0; 4];
        assert!(run(&mut p, 1, 2 * u64::from(WINDOW_S), &rates).is_empty());
    }

    #[test]
    fn resets_are_an_hour_apart_over_all_chips_and_domains() {
        let mut p = DomainLatch::new();
        assert_eq!(run(&mut p, 1, 600, &stuck(4, 2, 0.03)).len(), 1);
        // Another chip's domain latches right after: it waits for the
        // hour, then goes at the first full window past it.
        let v = run(&mut p, 601, 601 + 2 * SPACING_S, &stuck(6, 1, 0.0));
        assert_eq!(v.len(), 2, "{v:?}");
        let (t, verdict) = v[0];
        assert_eq!(
            verdict,
            Verdict::Reset {
                chip: 6,
                domain: 1,
                percent: 0,
                early: false,
            }
        );
        assert!(t >= 600 + SPACING_S, "{t}");
        assert!(t < 600 + SPACING_S + u64::from(WINDOW_S), "{t}");
        // Still stuck after that one: the next, an hour later again.
        assert_eq!(v[1].0, t + SPACING_S);
    }

    #[test]
    fn a_single_chip_restart_counts_against_the_hour() {
        let mut p = DomainLatch::new();
        assert!(run(&mut p, 1, 500, &stuck(4, 2, 0.03)).is_empty());
        p.note_reset(500);
        let v = run(&mut p, 501, 500 + SPACING_S + 1200, &stuck(4, 2, 0.03));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].0 >= 500 + SPACING_S, "{}", v[0].0);
    }

    #[test]
    fn a_domain_that_runs_an_hour_clean_after_its_reset_starts_fresh() {
        let mut p = DomainLatch::new();
        let mut t = 0;
        for _ in 0..10 {
            let v = run(&mut p, t + 1, t + 600, &stuck(4, 2, 0.03));
            assert_eq!(v, vec![(t + 600, RESET_4_2)]);
            // Back, and running clean for the full hour.
            assert!(run(&mut p, t + 601, t + 600 + SPACING_S, &healthy()).is_empty());
            t += 600 + SPACING_S;
        }
        assert_eq!(p.given_up().count(), 0);
        assert_eq!(p.tries[4][2], 0);
    }

    /// Three resets that do not hold, then the give-up: the domain is
    /// stuck from the start, every window says so, and the hour between
    /// resets is never a clean one.
    fn drive_to_give_up(p: &mut DomainLatch) -> (u64, usize) {
        let mut t = 0;
        let mut resets = 0;
        loop {
            t += 1;
            let rates = stuck(5, 1, 0.1);
            let s: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
            match p.observe(t, true, &s) {
                Some(Verdict::Reset {
                    chip: 5, domain: 1, ..
                }) => resets += 1,
                Some(Verdict::GiveUp { chip: 5, domain: 1 }) => return (t, resets),
                None => {}
                other => panic!("unexpected {other:?}"),
            }
            assert!(t < 10 * SPACING_S, "no give-up");
        }
    }

    #[test]
    fn three_resets_that_do_not_hold_give_the_domain_up() {
        let mut p = DomainLatch::new();
        let (t, resets) = drive_to_give_up(&mut p);
        assert_eq!(resets, usize::from(MAX_TRIES));
        // Reset at 600, at 600 + 1 h and at 600 + 2 h; the full window
        // after the third is the give-up.
        assert_eq!(t, 2 * SPACING_S + 2 * u64::from(WINDOW_S));
        assert_eq!(p.given_up().collect::<Vec<_>>(), vec![(5, 1)]);
        // Given up: nothing more for it, and it is out of the mean its
        // siblings are held against: domain 3 of the same chip at 30 %
        // of the two healthy ones (it would pass at 40 % of a mean that
        // the dead domain pulled down) is no latch; at 20 % it is one,
        // and so is another chip's domain.
        let mut rates = stuck(5, 1, 0.1);
        rates[5][3] = 30.0;
        assert!(run(&mut p, t + 1, t + 2 * SPACING_S, &rates).is_empty());
        rates[5][3] = 20.0;
        let v = run(&mut p, t + 2 * SPACING_S + 1, t + 3 * SPACING_S, &rates);
        assert_eq!(
            v.iter()
                .filter(|(_, v)| matches!(
                    v,
                    Verdict::Reset {
                        chip: 5,
                        domain: 3,
                        percent: 20,
                        early: false,
                    }
                ))
                .count(),
            1,
            "{v:?}"
        );
        let t = t + 3 * SPACING_S;
        let mut rates = stuck(5, 1, 0.1);
        rates[0][3] = 0.0;
        let v = run(&mut p, t + 1, t + SPACING_S + 1200, &rates);
        assert!(
            v.iter().any(|(_, v)| matches!(
                v,
                Verdict::Reset {
                    chip: 0,
                    domain: 3,
                    ..
                }
            )),
            "{v:?}"
        );
        assert!(v.iter().all(|(_, v)| !matches!(
            v,
            Verdict::Reset { chip: 5, .. } | Verdict::GiveUp { chip: 5, .. }
        )));
    }

    #[test]
    fn a_new_point_gives_a_given_up_domain_another_chance() {
        let mut p = DomainLatch::new();
        p.set_point(700, 1210);
        let (t, _) = drive_to_give_up(&mut p);
        // The same point again changes nothing.
        p.set_point(700, 1210);
        assert_eq!(p.given_up().collect::<Vec<_>>(), vec![(5, 1)]);
        p.set_point(650, 1210);
        assert_eq!(p.given_up().count(), 0);
        let v = run(&mut p, t + 1, t + SPACING_S + 1200, &stuck(5, 1, 0.1));
        assert!(v.iter().any(|(_, v)| matches!(
            v,
            Verdict::Reset {
                chip: 5,
                domain: 1,
                ..
            }
        )));
    }

    #[test]
    fn a_chain_back_with_another_chip_count_starts_every_window_over() {
        let mut p = DomainLatch::new();
        assert!(run(&mut p, 1, 500, &stuck(6, 0, 0.0)).is_empty());
        // Seven chips now: index 6 is another chip; its window starts
        // anew.
        let mut seven = vec![FINE; 7];
        seven[6][0] = 0.0;
        assert!(run(&mut p, 501, 1099, &seven).is_empty());
        assert_eq!(run(&mut p, 1100, 1100, &seven).len(), 1);
    }

    #[test]
    fn the_worst_of_two_stuck_domains_goes_first() {
        let mut p = DomainLatch::new();
        let mut rates = healthy();
        rates[2][1] = 10.0;
        rates[2][3] = 5.0;
        let v = run(&mut p, 1, 600, &rates);
        // Each against the mean of the other three (one of them stuck
        // too): domain 1 at 10 of (100+100+5)/3 = 15 %, domain 3 at 5
        // of (100+10+100)/3 = 7 %.
        assert_eq!(
            v,
            vec![(
                600,
                Verdict::Reset {
                    chip: 2,
                    domain: 3,
                    percent: 7,
                    early: false,
                }
            )]
        );
    }

    /// Like `run`, for a chain that has just come up.
    fn run_from_start<const D: usize>(
        p: &mut DomainLatch,
        from: u64,
        to: u64,
        rates: &[[f32; D]],
    ) -> Vec<(u64, Verdict)> {
        p.chain_ready();
        run(p, from, to, rates)
    }

    #[test]
    fn a_domain_dead_from_the_chains_start_is_reset_after_three_minutes() {
        for (share, percent) in [(0.0, 0), (0.03, 3), (0.049, 4)] {
            let mut p = DomainLatch::new();
            let v = run_from_start(&mut p, 1, 1500, &stuck(4, 2, share));
            assert_eq!(
                v,
                vec![(
                    u64::from(EARLY_WINDOW_S),
                    Verdict::Reset {
                        chip: 4,
                        domain: 2,
                        percent,
                        early: true
                    }
                )],
                "share {share}"
            );
        }
    }

    #[test]
    fn a_weak_domain_from_the_start_has_the_full_window() {
        // At 5 % and above it is not dead: the ten minutes, then the
        // usual verdict (stuck under 25 %, nothing at 50 %).
        for (share, percent) in [(0.05, 5), (0.2, 20)] {
            let mut p = DomainLatch::new();
            let v = run_from_start(&mut p, 1, 900, &stuck(4, 2, share));
            assert_eq!(
                v,
                vec![(
                    u64::from(WINDOW_S),
                    Verdict::Reset {
                        chip: 4,
                        domain: 2,
                        percent,
                        early: false
                    }
                )],
                "share {share}"
            );
        }
        let mut p = DomainLatch::new();
        assert!(run_from_start(&mut p, 1, 3 * u64::from(WINDOW_S), &stuck(4, 2, 0.5)).is_empty());
    }

    #[test]
    fn a_domain_that_dies_later_has_the_full_window() {
        // Fine for the chain's first ten minutes, dead from then on: no
        // early look in a later window, the reset ten minutes after.
        let mut p = DomainLatch::new();
        assert!(run_from_start(&mut p, 1, 600, &healthy()).is_empty());
        let v = run(&mut p, 601, 1500, &stuck(4, 2, 0.0));
        assert_eq!(
            v,
            vec![(
                1200,
                Verdict::Reset {
                    chip: 4,
                    domain: 2,
                    percent: 0,
                    early: false
                }
            )]
        );
        // Dead from the second minute of the first window: at the early
        // look it averages a third, which is not dead from the start;
        // the window's end finds it.
        let mut p = DomainLatch::new();
        assert!(run_from_start(&mut p, 1, 60, &healthy()).is_empty());
        let v = run(&mut p, 61, 900, &stuck(4, 2, 0.0));
        assert_eq!(
            v,
            vec![(
                600,
                Verdict::Reset {
                    chip: 4,
                    domain: 2,
                    percent: 10,
                    early: false
                }
            )]
        );
    }

    #[test]
    fn without_a_chain_start_there_is_no_early_look() {
        let mut p = DomainLatch::new();
        let v = run(&mut p, 1, 900, &stuck(4, 2, 0.0));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, u64::from(WINDOW_S));
        // And only a chain's first window has one: the start is over
        // once that window was looked at.
        let mut p = DomainLatch::new();
        assert!(run_from_start(&mut p, 1, 200, &healthy()).is_empty());
        let v = run(&mut p, 201, 2000, &stuck(4, 2, 0.0));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(matches!(v[0].1, Verdict::Reset { early: false, .. }));
    }

    #[test]
    fn the_early_look_counts_working_seconds_and_keeps_the_hour() {
        // A pool outage right after the start: the three minutes are
        // three minutes of work.
        let mut p = DomainLatch::new();
        let rates = stuck(4, 2, 0.0);
        let samples: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
        assert!(run_from_start(&mut p, 1, 100, &rates).is_empty());
        for t in 101..=2000 {
            assert_eq!(p.observe(t, false, &samples), None);
        }
        let v = run(&mut p, 2001, 2200, &rates);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, 2080);
        assert!(matches!(v[0].1, Verdict::Reset { early: true, .. }));
        // Within the hour of another intervention the early look does
        // nothing, and neither does the window's end: the hour is shared.
        let mut p = DomainLatch::new();
        p.note_reset(0);
        let v = run_from_start(&mut p, 1, SPACING_S + 1200, &rates);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].0 >= SPACING_S);
        assert!(matches!(v[0].1, Verdict::Reset { early: false, .. }));
    }

    #[test]
    fn a_domain_dead_after_every_reset_is_given_up_as_before() {
        // The chain comes up again after each reset; the domain is dead
        // each time. Three resets an hour apart, then given up.
        let mut p = DomainLatch::new();
        let rates = stuck(5, 1, 0.0);
        let samples: Vec<ChipSample> = rates.iter().map(|r| chip(r)).collect();
        p.chain_ready();
        let (mut resets, mut gave_up) = (Vec::new(), None);
        for t in 1..=4 * SPACING_S {
            match p.observe(t, true, &samples) {
                Some(Verdict::Reset { early, .. }) => {
                    resets.push((t, early));
                    p.chain_ready();
                }
                Some(Verdict::GiveUp { chip: 5, domain: 1 }) => {
                    gave_up = Some(t);
                    break;
                }
                None => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(resets.len(), usize::from(MAX_TRIES), "{resets:?}");
        // The first after three minutes; the next two wait for the hour.
        assert_eq!(resets[0], (u64::from(EARLY_WINDOW_S), true));
        assert!(resets[1].0 >= resets[0].0 + SPACING_S);
        assert!(resets[2].0 >= resets[1].0 + SPACING_S);
        // Given up at the end of the full window after the third: the
        // early look does not give a domain up.
        assert_eq!(gave_up, Some(resets[2].0 + u64::from(WINDOW_S)));
        assert_eq!(p.given_up().collect::<Vec<_>>(), vec![(5, 1)]);
    }

    #[test]
    fn eight_domains_and_more_chips_than_the_chain_holds() {
        let mut p = DomainLatch::new();
        let mut rates = vec![[100.0f32; 8]; MAX_CHIPS + 2];
        rates[MAX_CHIPS][5] = 0.0;
        rates[3][7] = 1.0;
        let v = run(&mut p, 1, 600, &rates);
        assert_eq!(
            v,
            vec![(
                600,
                Verdict::Reset {
                    chip: 3,
                    domain: 7,
                    percent: 1,
                    early: false,
                }
            )]
        );
    }
}

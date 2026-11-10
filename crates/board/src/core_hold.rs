//! A lower core voltage waits for the clock to come down.
//!
//! `PATCH /api/system` with a lower clock and a lower voltage hands the
//! two to different tasks: the board task sets the voltage in one step
//! within its next pass (100 ms), the mining loop ramps the clock in
//! PLL steps (6.25 MHz every 100 ms: 625 to 425 MHz takes 3.2 s). The
//! chips then ran at the old clock on the new, lower voltage for those
//! seconds, which is where hash errors and a dropped-out chip come from.
//! So a voltage *drop* is held back while the chips' clock is still
//! above the one asked for; a raise is never held, and neither is a drop
//! with nothing coming down (a voltage change alone, the bring-up
//! voltage after the chain init, a chain that is not running).
//!
//! The rule only, time and readings passed in, so it runs under
//! `cargo test` and against the simulated chain.

/// A drop is held no longer than this, ms: the longest ramp (the whole
/// PLL range in 6.25 MHz steps) takes about 15 s, and a voltage request
/// must not hang on a ramp that never ends. A clock the PLL cannot make
/// is another matter ([`Clock::unreachable`]): the chips are known to
/// stay above it, so the drop does not go at all.
pub const MAX_HOLD_MS: u64 = 30_000;

/// The clock as the mining loop reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clock {
    /// The clock asked for, MHz; 0 before the chain has started.
    pub wanted_mhz: u32,
    /// The request the chips' clock has arrived at (its ramp is over),
    /// MHz; 0 when none has.
    pub settled_mhz: u32,
    /// What the chips run at this moment, Hz (the last PLL word written,
    /// on the PLL's grid); 0 while no chain runs.
    pub now_hz: u32,
    /// The clock asked for has no PLL setting: the chips stay at the
    /// clock they have, and a drop that waits for them waits until a
    /// clock that can be made is asked for (and reached), or the
    /// voltage asked for is no longer a drop.
    pub unreachable: bool,
}

impl Clock {
    /// The chips still run above the clock asked for: a ramp down is
    /// under way or about to start. The settled request, not the clock's
    /// value, ends it: the PLL grid may sit a fraction above the request.
    pub const fn coming_down(&self) -> bool {
        self.wanted_mhz != 0
            && self.settled_mhz != self.wanted_mhz
            && self.now_hz as u64 > self.wanted_mhz as u64 * 1_000_000
    }
}

/// What to do with the voltage request this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Set it now (also: nothing to set).
    Apply,
    /// A drop, and the clock is still coming down: wait.
    Hold,
    /// A drop held for [`MAX_HOLD_MS`] without the clock arriving: set
    /// it now all the same, and say so.
    ApplyLate,
}

/// The board task's memory of a drop it is holding.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoreHold {
    since_ms: Option<u64>,
    /// The voltage the limit ran out for: it goes from then on (a
    /// regulator's refusal is retried every second, not held another
    /// 30 s each time) until it is applied or another one is asked for.
    late_mv: Option<u16>,
}

impl CoreHold {
    /// Nothing held.
    pub const fn new() -> Self {
        Self {
            since_ms: None,
            late_mv: None,
        }
    }

    /// A drop is being held right now.
    pub const fn holding(&self) -> bool {
        self.since_ms.is_some()
    }

    /// One pass: the voltage applied, the one asked for (mV), the clock.
    pub fn decide(&mut self, now_ms: u64, applied_mv: u16, wanted_mv: u16, clock: Clock) -> Step {
        if wanted_mv >= applied_mv || !clock.coming_down() {
            self.since_ms = None;
            self.late_mv = None;
            return Step::Apply;
        }
        if self.late_mv == Some(wanted_mv) {
            return Step::Apply;
        }
        self.late_mv = None;
        if clock.unreachable {
            // No ramp to time: the limit starts once a clock that can be
            // made is asked for.
            self.since_ms = Some(now_ms);
            return Step::Hold;
        }
        let since = *self.since_ms.get_or_insert(now_ms);
        if now_ms.saturating_sub(since) >= MAX_HOLD_MS {
            self.since_ms = None;
            self.late_mv = Some(wanted_mv);
            return Step::ApplyLate;
        }
        Step::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT_625: Clock = Clock {
        wanted_mhz: 625,
        settled_mhz: 625,
        now_hz: 625_000_000,
        unreachable: false,
    };

    #[test]
    fn a_drop_waits_while_the_clock_comes_down_and_goes_when_it_has_arrived() {
        let mut h = CoreHold::new();
        // The PATCH: 425 MHz asked for, the chips still at 625.
        let mut clock = Clock {
            wanted_mhz: 425,
            ..AT_625
        };
        assert_eq!(h.decide(0, 1200, 1100, clock), Step::Hold);
        clock.now_hz = 500_000_000;
        assert_eq!(h.decide(2000, 1200, 1100, clock), Step::Hold);
        assert!(h.holding());
        // The last PLL step is written on a grid point just above the
        // request, the ramp not yet closed: still held.
        clock.now_hz = 425_390_625;
        assert_eq!(h.decide(3200, 1200, 1100, clock), Step::Hold);
        // The ramp is over: the request is the settled one.
        clock.settled_mhz = 425;
        assert_eq!(h.decide(3300, 1200, 1100, clock), Step::Apply);
        assert!(!h.holding());
    }

    #[test]
    fn a_clock_right_on_the_request_is_down() {
        let mut h = CoreHold::new();
        let clock = Clock {
            wanted_mhz: 425,
            settled_mhz: 625,
            now_hz: 425_000_000,
            unreachable: false,
        };
        assert_eq!(h.decide(0, 1200, 1100, clock), Step::Apply);
    }

    #[test]
    fn a_raise_is_never_held() {
        let mut h = CoreHold::new();
        let down = Clock {
            wanted_mhz: 425,
            ..AT_625
        };
        assert_eq!(h.decide(0, 1100, 1200, down), Step::Apply);
        assert_eq!(h.decide(0, 1100, 1100, down), Step::Apply);
        let up = Clock {
            wanted_mhz: 700,
            ..AT_625
        };
        assert_eq!(h.decide(0, 1100, 1200, up), Step::Apply);
    }

    #[test]
    fn a_drop_with_nothing_coming_down_goes_at_once() {
        let mut h = CoreHold::new();
        // Voltage alone.
        assert_eq!(h.decide(0, 1200, 1100, AT_625), Step::Apply);
        // The clock on its way up (the drop is the user's own choice).
        let up = Clock {
            wanted_mhz: 700,
            ..AT_625
        };
        assert_eq!(h.decide(0, 1200, 1100, up), Step::Apply);
        // The bring-up ramp, below the target; the bring-up voltage goes
        // down to the configured one as before.
        let bring_up = Clock {
            wanted_mhz: 625,
            settled_mhz: 0,
            now_hz: 56_250_000,
            unreachable: false,
        };
        assert_eq!(h.decide(0, 1200, 1150, bring_up), Step::Apply);
        // No chain running (stopped, restarting, a board without chips).
        assert_eq!(h.decide(0, 1200, 1100, Clock::default()), Step::Apply);
        let stopped = Clock {
            wanted_mhz: 425,
            settled_mhz: 625,
            now_hz: 0,
            unreachable: false,
        };
        assert_eq!(h.decide(0, 1200, 1100, stopped), Step::Apply);
    }

    #[test]
    fn a_clock_that_never_arrives_does_not_hold_the_voltage_for_ever() {
        let mut h = CoreHold::new();
        let clock = Clock {
            wanted_mhz: 425,
            ..AT_625
        };
        assert_eq!(h.decide(1000, 1200, 1100, clock), Step::Hold);
        assert_eq!(
            h.decide(1000 + MAX_HOLD_MS - 1, 1200, 1100, clock),
            Step::Hold
        );
        assert_eq!(
            h.decide(1000 + MAX_HOLD_MS, 1200, 1100, clock),
            Step::ApplyLate
        );
        // The regulator refused it: it goes again on the next try, not
        // after another 30 s.
        assert_eq!(h.decide(2000 + MAX_HOLD_MS, 1200, 1100, clock), Step::Apply);
        assert_eq!(h.decide(9000 + MAX_HOLD_MS, 1200, 1100, clock), Step::Apply);
        // Another voltage is a new hold, from scratch.
        assert_eq!(h.decide(9001 + MAX_HOLD_MS, 1200, 1050, clock), Step::Hold);
        assert_eq!(
            h.decide(9001 + 2 * MAX_HOLD_MS, 1200, 1050, clock),
            Step::ApplyLate
        );
        // Applied: the memory is gone.
        assert_eq!(
            h.decide(9002 + 2 * MAX_HOLD_MS, 1050, 1050, clock),
            Step::Apply
        );
        assert_eq!(
            h.decide(9003 + 2 * MAX_HOLD_MS, 1050, 1000, clock),
            Step::Hold
        );
    }

    #[test]
    fn a_clock_the_pll_cannot_make_holds_the_drop_until_something_else_is_asked() {
        let mut h = CoreHold::new();
        // 425 MHz has no PLL setting: the chips stay at 625.
        let clock = Clock {
            wanted_mhz: 425,
            unreachable: true,
            ..AT_625
        };
        assert_eq!(h.decide(0, 1200, 1100, clock), Step::Hold);
        assert_eq!(h.decide(10 * MAX_HOLD_MS, 1200, 1100, clock), Step::Hold);
        // A raise still goes.
        assert_eq!(h.decide(10 * MAX_HOLD_MS, 1200, 1250, clock), Step::Apply);
        // A clock that can be made is asked for after ten minutes of
        // that: its ramp gets the full 30 s, the drop does not go on
        // the first pass.
        let ramping = Clock {
            wanted_mhz: 450,
            unreachable: false,
            ..AT_625
        };
        let t = 10 * MAX_HOLD_MS + 100;
        assert_eq!(h.decide(t, 1200, 1100, ramping), Step::Hold);
        assert_eq!(
            h.decide(t + MAX_HOLD_MS - 200, 1200, 1100, ramping),
            Step::Hold
        );
        // Reached: the drop goes.
        let down = Clock {
            wanted_mhz: 450,
            settled_mhz: 450,
            now_hz: 450_000_000,
            unreachable: false,
        };
        assert_eq!(h.decide(t + 3000, 1200, 1100, down), Step::Apply);
    }

    #[test]
    fn a_request_taken_back_ends_the_hold() {
        let mut h = CoreHold::new();
        let clock = Clock {
            wanted_mhz: 425,
            ..AT_625
        };
        assert_eq!(h.decide(0, 1200, 1100, clock), Step::Hold);
        // Back to the applied voltage: nothing to hold, and a later drop
        // gets the full time again.
        assert_eq!(h.decide(500, 1200, 1200, clock), Step::Apply);
        assert_eq!(h.decide(40_000, 1200, 1100, clock), Step::Hold);
    }
}

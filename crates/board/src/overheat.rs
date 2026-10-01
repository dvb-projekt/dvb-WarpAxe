//! When the chips may run again after an overheat stop.
//!
//! An overheat (chip or regulator over its limit, or no chip temperature
//! for too long) switches the core voltage off; a regulator fault
//! ([`crate::power_fault`]) takes the same path. AxeOS starts again on its
//! own once the regulator is 10 °C below its limit and at least 30 s have
//! passed; NerdOS stays off. The guard here follows AxeOS with a longer
//! pause, asks the chip to have cooled as well, wants real readings before
//! it lets go after a lost sensor, and stops trying after
//! [`MAX_TRIPS`] stops within [`TRIP_WINDOW_S`]: a board that keeps
//! overheating stays off until someone clears it by hand.
//!
//! Times are seconds of a monotonic clock (uptime); the caller samples.

/// Least time off after a stop, seconds.
pub const MIN_OFF_S: u32 = 60;
/// How far below its limit chip and regulator must be to start again, °C.
pub const HYSTERESIS_C: f32 = 10.0;
/// Stops within [`TRIP_WINDOW_S`] after which the guard latches.
pub const MAX_TRIPS: usize = 3;
/// The window for [`MAX_TRIPS`], seconds.
pub const TRIP_WINDOW_S: u32 = 3600;

/// Why the core voltage went off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// Chip or regulator over its limit.
    Hot,
    /// No chip temperature for too long.
    Blind,
    /// The regulator reported a fault (see [`crate::power_fault`]).
    PowerFault,
}

/// What the board reads now, for [`Guard::may_resume`].
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    /// Chip temperature, °C; `None` without a reading.
    pub chip_c: Option<f32>,
    /// The chip limit that stopped it, °C.
    pub chip_limit_c: f32,
    /// Regulator temperature, °C.
    pub vr_c: Option<f32>,
    /// The regulator limit, °C.
    pub vr_limit_c: f32,
    /// The board has a regulator temperature; a missing reading then
    /// counts as unknown, not as cool.
    pub vr_expected: bool,
    /// The board has read a chip temperature; a missing reading then
    /// counts as unknown, not as cool. A board without one stops only on
    /// the regulator or a power fault, and resumes on those alone.
    pub chip_expected: bool,
}

/// The overheat state of one board: the running stop and recent ones.
#[derive(Clone, Debug, Default)]
pub struct Guard {
    stopped: Option<(u32, Cause)>,
    trips: [Option<u32>; MAX_TRIPS],
    next: usize,
    latched: bool,
}

impl Guard {
    /// Not stopped, no history.
    pub const fn new() -> Self {
        Self {
            stopped: None,
            trips: [None; MAX_TRIPS],
            next: 0,
            latched: false,
        }
    }

    /// The core voltage went off at `now`.
    pub fn trip(&mut self, now: u32, cause: Cause) {
        self.stopped = Some((now, cause));
        self.trips[self.next] = Some(now);
        self.next = (self.next + 1) % MAX_TRIPS;
        let recent = self
            .trips
            .iter()
            .flatten()
            .filter(|&&t| now.saturating_sub(t) < TRIP_WINDOW_S)
            .count();
        if recent >= MAX_TRIPS {
            self.latched = true;
        }
    }

    /// Stopped and not yet running again.
    pub fn stopped(&self) -> Option<Cause> {
        self.stopped.map(|(_, c)| c)
    }

    /// Stops within the last [`TRIP_WINDOW_S`] before `now`, 0 to
    /// [`MAX_TRIPS`].
    pub fn recent_trips(&self, now: u32) -> usize {
        self.trips
            .iter()
            .flatten()
            .filter(|&&t| now.saturating_sub(t) < TRIP_WINDOW_S)
            .count()
    }

    /// Too many stops in a row: only [`Guard::clear`] starts it again.
    pub fn latched(&self) -> bool {
        self.latched
    }

    /// Whether the chips may run again at `now`.
    pub fn may_resume(&self, now: u32, r: &Reading) -> bool {
        let Some((since, _)) = self.stopped else {
            return false;
        };
        if self.latched || now.saturating_sub(since) < MIN_OFF_S {
            return false;
        }
        let chip_cool = match r.chip_c {
            Some(c) => c <= r.chip_limit_c - HYSTERESIS_C,
            None => !r.chip_expected,
        };
        let vr_cool = match r.vr_c {
            Some(c) => c <= r.vr_limit_c - HYSTERESIS_C,
            None => !r.vr_expected,
        };
        chip_cool && vr_cool
    }

    /// The chips run again after [`Guard::may_resume`]; the stop history
    /// stays for the latch.
    pub fn resumed(&mut self) {
        self.stopped = None;
    }

    /// Cleared by hand: forget stops and latch.
    pub fn clear(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(chip: Option<f32>, vr: Option<f32>) -> Reading {
        Reading {
            chip_c: chip,
            chip_limit_c: 70.0,
            vr_c: vr,
            vr_limit_c: 105.0,
            vr_expected: true,
            chip_expected: true,
        }
    }

    #[test]
    fn not_stopped_never_resumes() {
        let g = Guard::new();
        assert!(!g.may_resume(1000, &reading(Some(30.0), Some(40.0))));
    }

    #[test]
    fn waits_the_minimum_pause_even_when_cool() {
        let mut g = Guard::new();
        g.trip(100, Cause::Hot);
        let cool = reading(Some(40.0), Some(50.0));
        assert!(!g.may_resume(100 + MIN_OFF_S - 1, &cool));
        assert!(g.may_resume(100 + MIN_OFF_S, &cool));
    }

    #[test]
    fn needs_both_chip_and_regulator_below_limit_minus_hysteresis() {
        let mut g = Guard::new();
        g.trip(0, Cause::Hot);
        let t = MIN_OFF_S;
        assert!(
            !g.may_resume(t, &reading(Some(60.5), Some(50.0))),
            "chip 9.5 °C below"
        );
        assert!(
            g.may_resume(t, &reading(Some(60.0), Some(50.0))),
            "chip exactly 10 °C below"
        );
        assert!(
            !g.may_resume(t, &reading(Some(40.0), Some(95.5))),
            "regulator too warm"
        );
        assert!(g.may_resume(t, &reading(Some(40.0), Some(95.0))));
    }

    #[test]
    fn missing_readings_keep_it_off() {
        let mut g = Guard::new();
        g.trip(0, Cause::Blind);
        let t = MIN_OFF_S * 10;
        assert!(
            !g.may_resume(t, &reading(None, Some(40.0))),
            "no chip reading"
        );
        assert!(
            !g.may_resume(t, &reading(Some(40.0), None)),
            "regulator expected but silent"
        );
        let mut no_vr = reading(Some(40.0), None);
        no_vr.vr_expected = false;
        assert!(
            g.may_resume(t, &no_vr),
            "board without regulator temperature"
        );
    }

    #[test]
    fn a_board_that_never_read_a_chip_temperature_resumes_on_the_regulator_alone() {
        let mut g = Guard::new();
        g.trip(0, Cause::Hot);
        let t = MIN_OFF_S;
        let mut no_chip = reading(None, Some(95.5));
        no_chip.chip_expected = false;
        assert!(!g.may_resume(t, &no_chip), "regulator still too warm");
        no_chip.vr_c = Some(95.0);
        assert!(
            g.may_resume(t, &no_chip),
            "no chip sensor: the regulator decides"
        );
    }

    #[test]
    fn third_stop_within_the_hour_latches_until_cleared() {
        let mut g = Guard::new();
        let cool = reading(Some(40.0), Some(50.0));
        for i in 0..2 {
            g.trip(i * 600, Cause::Hot);
            assert!(!g.latched());
            assert!(g.may_resume(i * 600 + MIN_OFF_S, &cool));
            g.resumed();
        }
        assert_eq!(g.recent_trips(1200), 2);
        g.trip(1200, Cause::Hot);
        assert_eq!(g.recent_trips(1200), 3);
        assert_eq!(
            g.recent_trips(1200 + TRIP_WINDOW_S - 1),
            1,
            "first two aged out"
        );
        assert_eq!(g.recent_trips(1200 + TRIP_WINDOW_S), 0);
        assert!(g.latched());
        assert!(!g.may_resume(1200 + 10 * MIN_OFF_S, &cool));
        g.clear();
        assert!(!g.latched());
        assert_eq!(g.stopped(), None);
    }

    #[test]
    fn stops_spread_over_more_than_an_hour_do_not_latch() {
        let mut g = Guard::new();
        for i in 0..6 {
            g.trip(i * (TRIP_WINDOW_S / 2 + 1), Cause::Hot);
            g.resumed();
        }
        assert!(!g.latched());
    }

    #[test]
    fn uptime_wrap_does_not_panic_or_resume_early() {
        let mut g = Guard::new();
        g.trip(u32::MAX - 5, Cause::Hot);
        assert!(!g.may_resume(3, &reading(Some(40.0), Some(50.0))));
    }
}

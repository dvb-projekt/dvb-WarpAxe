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
    /// The core rail is off and the chip's sensor gives nothing, though
    /// it read while the rail was on: a diode inside the chip that only
    /// answers with the chip powered (a Bitaxe Gamma's, 2026-11-10). A
    /// reading will not come while the rail stays off, so after a hot
    /// stop or a regulator fault the chip is taken as cooled with the
    /// regulator and the time off, as AxeOS resumes; if it is not, it
    /// stops again and counts
    /// towards the latch. A sensor lost while the chips ran
    /// ([`Cause::Blind`]) still needs a real reading.
    pub chip_dark_off: bool,
}

/// The clock of a lost chip sensor: how long a sensor that has read has
/// been giving nothing, counted only while the core rail is on.
///
/// A chain hashing blind is stopped like one overheating
/// ([`Cause::Blind`]). But with the rail off nothing hashes, and on some
/// boards the chip's diode cannot be read then at all: a Bitaxe Gamma
/// whose chips were off for want of a pool went "blind" after a minute,
/// stopped as overheated, and never came back, because the reading the
/// guard waited for cannot come without the rail (2026-11-10). So a
/// missing reading with the rail off counts for nothing, whatever took
/// the rail down (no pool, chips off by hand, the firmware's own off
/// and on, an overheat stop), and the count starts anew when the rail
/// is back.
#[derive(Debug, Clone, Copy, Default)]
pub struct SensorWatch {
    seen: bool,
    lost_s: u32,
}

impl SensorWatch {
    /// Nothing read yet, as after a boot.
    pub const fn new() -> Self {
        Self {
            seen: false,
            lost_s: 0,
        }
    }

    /// One sample a second: a chip temperature was read or not, with the
    /// core rail on or off.
    pub fn sample(&mut self, reading: bool, rail_on: bool) {
        if reading {
            self.seen = true;
            self.lost_s = 0;
        } else if rail_on {
            self.lost_s = self.lost_s.saturating_add(1);
        } else {
            self.lost_s = 0;
        }
    }

    /// A chip temperature has been read since boot.
    pub fn seen(&self) -> bool {
        self.seen
    }

    /// Seconds without a reading, with the rail on.
    pub fn lost_s(&self) -> u32 {
        self.lost_s
    }

    /// Start the count over (the stop was cleared by hand, or the chips
    /// are back on).
    pub fn clear(&mut self) {
        self.lost_s = 0;
    }

    /// The chips run and a sensor that has read has given nothing for
    /// `limit_s` seconds: stop them.
    pub fn blind(&self, limit_s: u32) -> bool {
        self.seen && self.lost_s >= limit_s
    }

    /// No chip temperature since boot for `limit_s` seconds of running:
    /// a board without the sensor (it runs without the chip limit).
    pub fn never(&self, limit_s: u32) -> bool {
        !self.seen && self.lost_s >= limit_s
    }
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
        // A diode that cannot be read with the rail off: after a hot stop
        // or a regulator fault the regulator and the minute off decide,
        // as on AxeOS (still wrong once running, it stops again and counts
        // towards the latch). A sensor lost while the chips ran (`Blind`)
        // still needs a real reading.
        let blind_stop = matches!(self.stopped, Some((_, Cause::Blind)));
        let chip_cool = match r.chip_c {
            Some(c) => c <= r.chip_limit_c - HYSTERESIS_C,
            None => !r.chip_expected || (r.chip_dark_off && !blind_stop),
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

    /// A NerdOS two-fan board's chip line (70) with a TPS546 regulator
    /// line (105): the tests below count from those.
    fn reading(chip: Option<f32>, vr: Option<f32>) -> Reading {
        Reading {
            chip_c: chip,
            chip_limit_c: 70.0,
            vr_c: vr,
            vr_limit_c: 105.0,
            vr_expected: true,
            chip_expected: true,
            chip_dark_off: false,
        }
    }

    /// The limit the firmware uses.
    const LOST: u32 = 60;

    #[test]
    fn chips_off_for_want_of_a_pool_never_go_blind() {
        // The Gamma of 2026-11-10: the diode reads while the chips run,
        // the pools go, the rail is switched off and the diode gives
        // nothing from then on.
        let mut w = SensorWatch::new();
        for _ in 0..300 {
            w.sample(true, true);
        }
        for s in 0..3600 {
            w.sample(false, false);
            assert!(!w.blind(LOST), "blind after {s} s with the rail off");
            assert_eq!(w.lost_s(), 0);
        }
        // A pool answers: the rail comes back, the diode needs a few
        // seconds, then reads. Nothing was counted from the off time.
        for _ in 0..5 {
            w.sample(false, true);
        }
        assert_eq!(w.lost_s(), 5);
        assert!(!w.blind(LOST));
        w.sample(true, true);
        assert_eq!(w.lost_s(), 0);
    }

    #[test]
    fn a_sensor_lost_while_the_chips_run_is_blind_after_the_limit() {
        let mut w = SensorWatch::new();
        w.sample(true, true);
        for _ in 0..LOST - 1 {
            w.sample(false, true);
        }
        assert!(!w.blind(LOST));
        w.sample(false, true);
        assert!(w.blind(LOST));
        // The stop takes the rail down: the count is over, and starts
        // anew when the rail is back.
        w.sample(false, false);
        assert!(!w.blind(LOST));
        for _ in 0..LOST {
            w.sample(false, true);
        }
        assert!(w.blind(LOST), "still lost after the resume: blind again");
    }

    #[test]
    fn every_way_of_taking_the_rail_down_stops_the_count() {
        // Fifty seconds lost with the rail on, then off for a while (by
        // hand, the firmware's five-second off and on, a self-test's
        // wait), then on again: the fifty are gone.
        for off_s in [1u32, 5, 45, 600] {
            let mut w = SensorWatch::new();
            w.sample(true, true);
            for _ in 0..50 {
                w.sample(false, true);
            }
            for _ in 0..off_s {
                w.sample(false, false);
            }
            for _ in 0..LOST - 1 {
                w.sample(false, true);
            }
            assert!(!w.blind(LOST), "off for {off_s} s");
            w.sample(false, true);
            assert!(w.blind(LOST), "off for {off_s} s");
        }
    }

    #[test]
    fn a_board_whose_sensor_never_read_is_not_blind() {
        let mut w = SensorWatch::new();
        for _ in 0..LOST {
            w.sample(false, true);
        }
        assert!(!w.blind(LOST));
        assert!(w.never(LOST));
        assert!(!w.seen());
        // Off from the boot on (no pool from the start): not counted
        // either.
        let mut w = SensorWatch::new();
        for _ in 0..600 {
            w.sample(false, false);
        }
        assert!(!w.never(LOST) && !w.blind(LOST));
        // `clear` starts the count over, as a reset by hand does.
        let mut w = SensorWatch::new();
        w.sample(true, true);
        for _ in 0..59 {
            w.sample(false, true);
        }
        w.clear();
        w.sample(false, true);
        assert_eq!(w.lost_s(), 1);
    }

    #[test]
    fn a_hot_stop_resumes_without_a_reading_the_rail_would_have_to_be_on_for() {
        // A diode that cannot be read with the rail off: after a hot
        // stop the regulator and the time off decide, as on AxeOS.
        let dark = |vr: f32| Reading {
            chip_c: None,
            chip_dark_off: true,
            ..reading(None, Some(vr))
        };
        let mut g = Guard::new();
        g.trip(0, Cause::Hot);
        assert!(
            !g.may_resume(MIN_OFF_S - 1, &dark(60.0)),
            "the minute first"
        );
        assert!(!g.may_resume(MIN_OFF_S, &dark(96.0)), "the regulator first");
        assert!(g.may_resume(MIN_OFF_S, &dark(95.0)));
        // Without the flag (a sensor that reads with the rail off, and
        // gives nothing now) the reading is waited for, as before.
        assert!(!g.may_resume(MIN_OFF_S, &reading(None, Some(60.0))));
        // A regulator fault on a board whose diode is dark with the rail
        // off (Gamma, two-chip boards): back on after the minute with the
        // regulator cool, and the third stop within the window latches.
        let mut g = Guard::new();
        g.trip(0, Cause::PowerFault);
        assert!(!g.may_resume(MIN_OFF_S - 1, &dark(60.0)));
        assert!(g.may_resume(MIN_OFF_S, &dark(60.0)));
        let mut g = Guard::new();
        for t in [0, 100, 200] {
            g.trip(t, Cause::PowerFault);
        }
        assert!(g.latched());
        assert!(!g.may_resume(10_000, &dark(60.0)));
        // A sensor lost while the chips ran still needs a real reading.
        let mut g = Guard::new();
        g.trip(0, Cause::Blind);
        assert!(!g.may_resume(10 * MIN_OFF_S, &dark(60.0)));
        assert!(g.may_resume(10 * MIN_OFF_S, &reading(Some(55.0), Some(60.0))));
        // And the latch holds whatever the readings say.
        let mut g = Guard::new();
        for t in [0, 100, 200] {
            g.trip(t, Cause::Hot);
        }
        assert!(g.latched());
        assert!(!g.may_resume(10_000, &dark(60.0)));
    }

    /// The Bitaxe Gamma under its stock lines (chip 75, regulator 105):
    /// back on once the regulator is at 95, as AxeOS does
    /// (`TPS546_THROTTLE_TEMP - 10`), and the chip at 65.
    #[test]
    fn bitaxe_gamma_resumes_where_axeos_does() {
        let mut g = Guard::new();
        g.trip(0, Cause::Hot);
        let gamma = |chip: f32, vr: f32| Reading {
            chip_c: Some(chip),
            chip_limit_c: 75.0,
            vr_c: Some(vr),
            vr_limit_c: 105.0,
            vr_expected: true,
            chip_expected: true,
            chip_dark_off: false,
        };
        let t = MIN_OFF_S;
        assert!(!g.may_resume(t, &gamma(60.0, 95.5)), "regulator above 95");
        assert!(g.may_resume(t, &gamma(60.0, 95.0)), "AxeOS's 105 - 10");
        assert!(!g.may_resume(t, &gamma(65.5, 90.0)), "chip above 65");
        assert!(g.may_resume(t, &gamma(65.0, 90.0)));
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

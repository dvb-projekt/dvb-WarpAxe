//! Several chip temperature sensors on one board: what the fan and the
//! stop line act on.
//!
//! The Naja Duo and the Gamma Turbo carry two chips, each with its own
//! diode on the EMC2103's two channels. The firmware read the first
//! only: the second chip had neither a fan control nor an overheat stop.
//! ESP-Miner (v2.15.4, `power_management_task.c`, `fan_controller_task.c`)
//! stops when either diode is over the line and runs the fan on the
//! hotter of the two; this does the same, and adds the rule the firmware
//! has for a single sensor: a reading that fails is unknown, never cool.
//!
//! - The hottest readable chip is the temperature, if every sensor that
//!   has ever read since boot reads now.
//! - A sensor that read before and gives nothing now makes the
//!   temperature unknown (`None`): the caller runs the fans at full and
//!   counts towards its lost-sensor stop, as for a single sensor.
//! - Unless a readable chip is already over the stop line: that is hot
//!   whatever the silent one would say, and stops at once.
//! - A sensor that has never read since boot is left out of the
//!   temperature, as a board whose only sensor never reads runs without
//!   the chip limit. But its chip has no stop line of its own then, so
//!   the caller runs the fans at full once that has lasted
//!   ([`ChipTemps::never_read_s`]) while another sensor reads: a reboot
//!   must not take a chip's protection away with a failed diode. When
//!   it reads after all, everything is as normal.
//! - Right after the chips start, a second diode's reading over the
//!   stop line is held back for [`SETTLE_SAMPLES`] samples
//!   ([`ChipTemps::chips_started`]): it is left out of the temperature
//!   and does not stop the chips; the caller runs the fans at full
//!   meanwhile ([`ChipTemps::held`]). The EMC2103 gives the running
//!   average of a diode's last four conversions (datasheet 5.11.4), and
//!   a second chip's diode reads the top of the scale while its chip is
//!   unpowered or in reset: a Gamma Turbo's cold second chip read
//!   108.375 °C (three conversions at 127.875 °C and one real one), then
//!   80.6 °C, in the first second after its start, and was stopped for
//!   overheating on every start. A reading still over the line when the
//!   window is over stops as ever. The first sensor (the chip's own
//!   diode, the only one on a single-chip board) keeps its rule: the
//!   scale's top was seen on the second diode only, and the first is
//!   the stop line that holds during the window.

/// For how many samples (seconds) after the chips' start a second
/// diode's reading over the stop line is held back. The EMC2103 converts
/// four times a second by default (datasheet table 6.18, configuration 2
/// = 0x0E) and averages four conversions, so the last reading from
/// before the start has left the average 1 s after the diode's first
/// real conversion, 1.25 s with the conversion's phase. The samples are
/// a second apart and the first falls up to a second after the start,
/// so three of them cover between two and three seconds: the average
/// has turned over twice. The chips' start is slower than that to heat
/// a chip from under the line to damage, the first chip's diode watches
/// throughout, and the fans run at full.
pub const SETTLE_SAMPLES: u8 = 3;

/// The sensors' memory: which have read since boot.
#[derive(Debug, Clone, Copy)]
pub struct ChipTemps<const N: usize> {
    seen: [bool; N],
    /// Samples (seconds) each sensor has gone without ever reading
    /// while another one read.
    never_s: [u32; N],
    /// Samples left of the settle window after the chips' start.
    settle: u8,
    /// Sensors whose reading the last sample held back.
    held: [bool; N],
}

impl<const N: usize> Default for ChipTemps<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ChipTemps<N> {
    /// Nothing read yet, as after a boot.
    pub const fn new() -> Self {
        Self {
            seen: [false; N],
            never_s: [0; N],
            settle: 0,
            held: [false; N],
        }
    }

    /// The chips have just been powered or released from reset: the
    /// settle window starts (over), [`SETTLE_SAMPLES`] samples from here.
    pub fn chips_started(&mut self) {
        self.settle = SETTLE_SAMPLES;
    }

    /// The last sample held sensor `i`'s reading back: over the stop
    /// line inside the settle window. The caller runs the fans at full.
    pub fn held(&self, i: usize) -> bool {
        self.held.get(i).copied().unwrap_or(false)
    }

    /// Samples left of the settle window, 0 outside it.
    pub fn settle_left(&self) -> u8 {
        self.settle
    }

    /// For how many samples sensor `i` has not read, never having read
    /// since boot, while another sensor did: 0 once it has read, and
    /// while no sensor has read at all (that is the caller's "no chip
    /// temperature" case, not one diode's).
    pub fn never_read_s(&self, i: usize) -> u32 {
        self.never_s.get(i).copied().unwrap_or(0)
    }

    /// Sensor `i` has read at least once since boot.
    pub fn seen(&self, i: usize) -> bool {
        self.seen.get(i).copied().unwrap_or(false)
    }

    /// One sample: each chip's reading (°C) or `None`, and the stop line
    /// in force. The temperature to act on, or `None` when it is unknown.
    pub fn sample(&mut self, readings: [Option<f32>; N], stop_c: f32) -> Option<f32> {
        let mut hottest: Option<f32> = None;
        let mut missing = false;
        let settling = self.settle > 0;
        self.settle = self.settle.saturating_sub(1);
        for (i, r) in readings.into_iter().enumerate() {
            self.held[i] = false;
            match r {
                Some(c) => {
                    self.seen[i] = true;
                    // A second diode over the line right after the
                    // start: the average still holds readings from
                    // before. Left out, as a diode that never read.
                    if settling && i > 0 && c > stop_c {
                        self.held[i] = true;
                    } else {
                        hottest = Some(hottest.map_or(c, |h| h.max(c)));
                    }
                }
                None => missing |= self.seen[i],
            }
        }
        for (i, never) in self.never_s.iter_mut().enumerate() {
            let another_reads = readings
                .iter()
                .enumerate()
                .any(|(j, r)| j != i && r.is_some());
            // A second in which no other sensor reads either does not
            // count, and does not start the count over: one failed read
            // of the working diode must not take the fans off full.
            if self.seen[i] {
                *never = 0;
            } else if another_reads {
                *never = never.saturating_add(1);
            }
        }
        match hottest {
            Some(c) if c > stop_c => Some(c),
            _ if missing => None,
            h => h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOP: f32 = 75.0;

    #[test]
    fn the_hotter_chip_is_the_temperature() {
        let mut t = ChipTemps::<2>::new();
        assert_eq!(t.sample([Some(58.0), Some(64.5)], STOP), Some(64.5));
        assert_eq!(t.sample([Some(66.0), Some(61.0)], STOP), Some(66.0));
        assert_eq!(t.sample([Some(60.0), Some(60.0)], STOP), Some(60.0));
        assert!(t.seen(0) && t.seen(1));
        assert!(!t.seen(2));
    }

    #[test]
    fn the_second_chip_over_the_line_is_over_the_line() {
        let mut t = ChipTemps::<2>::new();
        // Chip 1 cool, chip 2 hot: the reading the stop acts on is chip
        // 2's (the case the single-diode firmware never saw).
        let acting = t.sample([Some(52.0), Some(76.5)], STOP);
        assert_eq!(acting, Some(76.5));
        assert!(acting.is_some_and(|c| c > STOP));
        // And the other way round.
        assert_eq!(t.sample([Some(77.0), Some(50.0)], STOP), Some(77.0));
        // Right on the line is not over it, as for a single sensor.
        assert_eq!(t.sample([Some(75.0), Some(60.0)], STOP), Some(75.0));
    }

    #[test]
    fn a_sensor_that_stops_reading_is_unknown_not_cool() {
        let mut t = ChipTemps::<2>::new();
        assert_eq!(t.sample([Some(60.0), Some(62.0)], STOP), Some(62.0));
        // Chip 2's diode fails: not the 60 °C of chip 1, which would
        // call the board cool while chip 2 may be anything.
        assert_eq!(t.sample([Some(60.0), None], STOP), None);
        assert_eq!(t.sample([None, Some(62.0)], STOP), None);
        assert_eq!(t.sample([None, None], STOP), None);
        // Back: the hotter of the two again.
        assert_eq!(t.sample([Some(61.0), Some(63.0)], STOP), Some(63.0));
    }

    #[test]
    fn a_hot_chip_stops_at_once_whatever_the_silent_one_reads() {
        let mut t = ChipTemps::<2>::new();
        t.sample([Some(60.0), Some(62.0)], STOP);
        // Chip 2's diode fails while chip 1 is over the line: hot, not
        // sixty seconds of "unknown" first.
        assert_eq!(t.sample([Some(79.0), None], STOP), Some(79.0));
        assert_eq!(t.sample([None, Some(81.0)], STOP), Some(81.0));
    }

    #[test]
    fn a_sensor_that_never_read_since_boot_is_left_out() {
        // A board variant without the second diode: chip 1 alone.
        let mut t = ChipTemps::<2>::new();
        for _ in 0..120 {
            assert_eq!(t.sample([Some(60.0), None], STOP), Some(60.0));
        }
        assert!(t.seen(0) && !t.seen(1));
        // Once it has read, it counts from then on.
        assert_eq!(t.sample([Some(60.0), Some(55.0)], STOP), Some(60.0));
        assert_eq!(t.sample([Some(60.0), None], STOP), None);
        // No sensor has ever read: nothing to act on (the caller's
        // "no chip temperature since boot").
        let mut t = ChipTemps::<2>::new();
        assert_eq!(t.sample([None, None], STOP), None);
        assert!(!t.seen(0) && !t.seen(1));
    }

    #[test]
    fn a_sensor_that_never_read_is_counted_while_the_other_reads() {
        let mut t = ChipTemps::<2>::new();
        // Chip 2's diode has not read since boot, chip 1's does.
        for s in 1..=90 {
            assert_eq!(t.sample([Some(60.0), None], STOP), Some(60.0));
            assert_eq!((t.never_read_s(0), t.never_read_s(1)), (0, s));
        }
        // A second in which chip 1 does not read either is not counted,
        // and does not start the count over.
        t.sample([None, None], STOP);
        assert_eq!(t.never_read_s(1), 90);
        t.sample([Some(60.0), None], STOP);
        assert_eq!(t.never_read_s(1), 91);
        // It reads after all: over, and a later failure is a lost
        // sensor (unknown), not a never-read one.
        assert_eq!(t.sample([Some(60.0), Some(58.0)], STOP), Some(60.0));
        assert_eq!(t.never_read_s(1), 0);
        assert_eq!(t.sample([Some(60.0), None], STOP), None);
        assert_eq!(t.never_read_s(1), 0);
        // The mirror: chip 1's diode never reads, chip 2's does.
        let mut t = ChipTemps::<2>::new();
        for _ in 0..5 {
            t.sample([None, Some(55.0)], STOP);
        }
        assert_eq!((t.never_read_s(0), t.never_read_s(1)), (5, 0));
        // No sensor at all, or a single one: nothing to count.
        let mut t = ChipTemps::<2>::new();
        t.sample([None, None], STOP);
        assert_eq!((t.never_read_s(0), t.never_read_s(1)), (0, 0));
        let mut t = ChipTemps::<1>::new();
        t.sample([None], STOP);
        assert_eq!((t.never_read_s(0), t.never_read_s(7)), (0, 0));
    }

    #[test]
    fn a_second_diode_s_stale_average_after_the_start_does_not_stop() {
        // The Gamma Turbo of 2026-11-12, limit 70 °C: chip 2's diode at
        // the scale's top while off (no reading), then the average
        // running out of it.
        const LIMIT: f32 = 70.0;
        let mut t = ChipTemps::<2>::new();
        assert_eq!(t.sample([Some(31.0), None], LIMIT), Some(31.0));
        t.chips_started();
        assert_eq!(t.settle_left(), SETTLE_SAMPLES);
        let acting = t.sample([Some(54.0), Some(108.375)], LIMIT);
        assert_eq!(acting, Some(54.0));
        assert!(!acting.is_some_and(|c| c > LIMIT));
        assert!(t.held(1) && !t.held(0));
        assert!(t.seen(1));
        assert_eq!(t.sample([Some(56.0), Some(80.625)], LIMIT), Some(56.0));
        assert!(t.held(1));
        // The real temperature: nothing held, the hotter chip again.
        assert_eq!(t.sample([Some(57.0), Some(50.5)], LIMIT), Some(57.0));
        assert!(!t.held(1));
        assert_eq!(t.settle_left(), 0);
        assert_eq!(t.sample([Some(55.0), Some(58.0)], LIMIT), Some(58.0));
        assert!(!t.held(0) && !t.held(1) && !t.held(2));
    }

    #[test]
    fn a_second_diode_still_hot_after_the_window_stops() {
        let mut t = ChipTemps::<2>::new();
        t.chips_started();
        for _ in 0..SETTLE_SAMPLES {
            assert_eq!(t.sample([Some(55.0), Some(96.0)], STOP), Some(55.0));
            assert!(t.held(1));
        }
        // The window is over and the reading is not: over the line.
        let acting = t.sample([Some(55.0), Some(96.0)], STOP);
        assert_eq!(acting, Some(96.0));
        assert!(acting.is_some_and(|c| c > STOP));
        assert!(!t.held(1));
        // A reading that crosses the line later, with no start before
        // it, stops at once as ever.
        let mut t = ChipTemps::<2>::new();
        t.chips_started();
        for _ in 0..10 {
            assert_eq!(t.sample([Some(55.0), Some(60.0)], STOP), Some(60.0));
        }
        assert_eq!(t.sample([Some(55.0), Some(76.0)], STOP), Some(76.0));
        assert!(!t.held(1));
        // And without a start there is no window at all.
        let mut t = ChipTemps::<2>::new();
        assert_eq!(t.sample([Some(55.0), Some(108.375)], STOP), Some(108.375));
    }

    #[test]
    fn the_window_starts_over_with_each_start() {
        let mut t = ChipTemps::<2>::new();
        t.chips_started();
        assert_eq!(t.sample([Some(50.0), Some(108.0)], STOP), Some(50.0));
        assert_eq!(t.sample([Some(50.0), Some(45.0)], STOP), Some(50.0));
        // The reset released, then the chips answered: counted from the
        // later one.
        t.chips_started();
        t.sample([Some(50.0), Some(45.0)], STOP);
        t.chips_started();
        assert_eq!(t.settle_left(), SETTLE_SAMPLES);
        // Long after: an overheat stop, the chips off (no reading from
        // chip 2 with the rail down), then on again.
        for _ in 0..30 {
            t.sample([Some(50.0), Some(45.0)], STOP);
        }
        assert_eq!(t.sample([Some(40.0), None], STOP), None);
        t.chips_started();
        assert_eq!(t.sample([Some(52.0), Some(104.25)], STOP), Some(52.0));
        assert!(t.held(1));
        assert_eq!(t.sample([Some(53.0), Some(78.375)], STOP), Some(53.0));
        assert_eq!(t.sample([Some(53.0), Some(48.0)], STOP), Some(53.0));
        assert_eq!(t.sample([Some(53.0), Some(77.0)], STOP), Some(77.0));
    }

    #[test]
    fn the_first_chip_and_the_lost_sensor_rules_hold_inside_the_window() {
        // The chip's own diode over the line stops at once, window or
        // not, on two sensors and on one.
        let mut t = ChipTemps::<2>::new();
        t.chips_started();
        assert_eq!(t.sample([Some(79.0), Some(50.0)], STOP), Some(79.0));
        assert!(!t.held(0));
        // Both over: the first chip's reading is the one that stops.
        assert_eq!(t.sample([Some(79.0), Some(108.0)], STOP), Some(79.0));
        assert!(t.held(1) && !t.held(0));
        let mut t = ChipTemps::<1>::new();
        t.chips_started();
        assert_eq!(t.sample([Some(90.0)], STOP), Some(90.0));
        assert!(!t.held(0));
        // A sensor that read before and gives nothing is unknown inside
        // the window too, whichever it is; a held reading does not make
        // up for a silent first chip.
        let mut t = ChipTemps::<2>::new();
        t.sample([Some(60.0), Some(62.0)], STOP);
        t.chips_started();
        assert_eq!(t.sample([Some(60.0), None], STOP), None);
        t.chips_started();
        assert_eq!(t.sample([None, Some(108.0)], STOP), None);
        assert!(t.held(1));
        // A diode that never read is counted as ever, and a held
        // reading is a reading: the count is over with it.
        let mut t = ChipTemps::<2>::new();
        t.chips_started();
        for s in 1..=5 {
            assert_eq!(t.sample([Some(60.0), None], STOP), Some(60.0));
            assert_eq!(t.never_read_s(1), s);
        }
        t.chips_started();
        assert_eq!(t.sample([Some(60.0), Some(108.0)], STOP), Some(60.0));
        assert_eq!(t.never_read_s(1), 0);
        assert!(t.seen(1));
    }

    #[test]
    fn one_sensor_behaves_as_it_always_did() {
        let mut t = ChipTemps::<1>::new();
        assert_eq!(t.sample([None], STOP), None);
        assert_eq!(t.sample([Some(64.0)], STOP), Some(64.0));
        assert_eq!(t.sample([None], STOP), None);
        assert_eq!(t.sample([Some(90.0)], STOP), Some(90.0));
    }
}

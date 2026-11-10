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

/// The sensors' memory: which have read since boot.
#[derive(Debug, Clone, Copy)]
pub struct ChipTemps<const N: usize> {
    seen: [bool; N],
    /// Samples (seconds) each sensor has gone without ever reading
    /// while another one read.
    never_s: [u32; N],
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
        }
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
        for (seen, r) in self.seen.iter_mut().zip(readings) {
            match r {
                Some(c) => {
                    *seen = true;
                    hottest = Some(hottest.map_or(c, |h| h.max(c)));
                }
                None => missing |= *seen,
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
    fn one_sensor_behaves_as_it_always_did() {
        let mut t = ChipTemps::<1>::new();
        assert_eq!(t.sample([None], STOP), None);
        assert_eq!(t.sample([Some(64.0)], STOP), Some(64.0));
        assert_eq!(t.sample([None], STOP), None);
        assert_eq!(t.sample([Some(90.0)], STOP), Some(90.0));
    }
}

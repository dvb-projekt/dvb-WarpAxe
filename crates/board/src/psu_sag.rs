//! The supply's voltage under load: a weak power supply or a thin cable.
//!
//! The regulator reads the board's input voltage (READ_VIN). With the
//! chips off it shows what the supply gives without load; with the chips
//! hashing, what reaches the board under load. A supply at its limit or a
//! long, thin cable lets the voltage sag; the miner still runs, until a
//! higher clock or a warmer day takes it under the regulator's
//! undervoltage line. This watch only reports: no stop, no change to the
//! clock.
//!
//! - The idle voltage is the median of the samples taken while the chips
//!   are not running (rail off, in reset, chain not answered yet), at
//!   boot and at every chips off/on; kept until the next such phase.
//! - The load voltage is the median of the last [`WINDOW`] samples with
//!   the chips running.
//! - The sag is idle minus load. It is reported once both are known; a
//!   warning holds when the sag reaches [`WARN_SHARE`] of the board's
//!   nominal input for a full window, and goes when it is back under
//!   [`CLEAR_SHARE`].

/// Samples (seconds) the load voltage is the median of.
pub const WINDOW: usize = 60;
/// Idle samples needed before an idle voltage counts.
pub const IDLE_MIN: usize = 3;
/// A sag of this share of the nominal input warns (0.4 V on 5 V,
/// 0.96 V on 12 V). A healthy supply and cable stay well under it: the
/// boards on our bench sag 1–2 %.
pub const WARN_SHARE: f32 = 0.08;
/// The warning goes again under this share.
pub const CLEAR_SHARE: f32 = 0.06;

/// What the watch reports.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Reading {
    /// The input voltage without load, V.
    pub idle_v: Option<f32>,
    /// The input voltage under load (median of the window), V.
    pub load_v: Option<f32>,
    /// Idle minus load, V; `None` until both are known.
    pub sag_v: Option<f32>,
    /// The sag has reached [`WARN_SHARE`] of the nominal input.
    pub warn: bool,
}

const IDLE_KEEP: usize = 16;

/// The watch: fed one sample a second. No allocation (`no_std`).
#[derive(Debug, Clone)]
pub struct PsuSag {
    nominal_v: f32,
    idle: [f32; IDLE_KEEP],
    idle_n: usize,
    idle_v: Option<f32>,
    load: [f32; WINDOW],
    load_n: usize,
    load_at: usize,
    warn: bool,
    was_running: bool,
}

/// The median of `v` (the upper one of an even count).
fn median(v: &[f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let mut s = [0f32; WINDOW];
    let s = &mut s[..v.len()];
    s.copy_from_slice(v);
    // Insertion sort: at most 60 values once a second.
    for i in 1..s.len() {
        let mut j = i;
        while j > 0 && s[j - 1] > s[j] {
            s.swap(j - 1, j);
            j -= 1;
        }
    }
    Some(s[s.len() / 2])
}

impl PsuSag {
    /// For a board with this nominal input voltage (5 or 12 V).
    pub fn new(nominal_v: f32) -> Self {
        Self {
            nominal_v,
            idle: [0.0; IDLE_KEEP],
            idle_n: 0,
            idle_v: None,
            load: [0.0; WINDOW],
            load_n: 0,
            load_at: 0,
            warn: false,
            was_running: false,
        }
    }

    /// One sample: the input voltage (`None` = no reading) and whether
    /// the chips are running.
    pub fn sample(&mut self, vin: Option<f32>, chips_running: bool) -> Reading {
        if chips_running != self.was_running {
            self.was_running = chips_running;
            if chips_running {
                self.load_n = 0;
                self.load_at = 0;
            } else {
                // A new idle phase: its own samples, the old idle voltage
                // until enough of them.
                self.idle_n = 0;
            }
        }
        if let Some(v) = vin.filter(|v| *v > 0.5) {
            if chips_running {
                self.load[self.load_at] = v;
                self.load_at = (self.load_at + 1) % WINDOW;
                self.load_n = (self.load_n + 1).min(WINDOW);
            } else {
                if self.idle_n < IDLE_KEEP {
                    self.idle[self.idle_n] = v;
                    self.idle_n += 1;
                }
                if self.idle_n >= IDLE_MIN {
                    self.idle_v = median(&self.idle[..self.idle_n]);
                }
            }
        }
        let load_v = median(&self.load[..self.load_n]);
        let sag_v = match (self.idle_v, load_v) {
            (Some(i), Some(l)) if chips_running => Some((i - l).max(0.0)),
            _ => None,
        };
        if let Some(s) = sag_v {
            let share = if self.nominal_v > 0.0 {
                s / self.nominal_v
            } else {
                0.0
            };
            if !self.warn && self.load_n == WINDOW && share >= WARN_SHARE {
                self.warn = true;
            } else if self.warn && share < CLEAR_SHARE {
                self.warn = false;
            }
        }
        // No load, no verdict: a warning that held stays until the chips
        // run again and say otherwise.
        Reading {
            idle_v: self.idle_v,
            load_v,
            sag_v,
            warn: self.warn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(w: &mut PsuSag, v: f32, n: usize, running: bool) -> Reading {
        let mut r = Reading::default();
        for _ in 0..n {
            r = w.sample(Some(v), running);
        }
        r
    }

    #[test]
    fn a_healthy_supply_reports_its_small_sag_and_no_warning() {
        let mut w = PsuSag::new(12.0);
        run(&mut w, 12.4, 5, false);
        let r = run(&mut w, 12.2, WINDOW, true);
        assert_eq!(r.idle_v, Some(12.4));
        assert_eq!(r.load_v, Some(12.2));
        assert!((r.sag_v.unwrap() - 0.2).abs() < 1e-4);
        assert!(!r.warn);
    }

    #[test]
    fn a_weak_supply_warns_after_a_full_window_and_clears_again() {
        let mut w = PsuSag::new(5.0);
        run(&mut w, 5.2, 5, false);
        // 0.45 V sag = 9 % of 5 V: not before the window is full.
        let r = run(&mut w, 4.75, WINDOW - 1, true);
        assert!(!r.warn);
        let r = run(&mut w, 4.75, 1, true);
        assert!(r.warn);
        // Back to 0.2 V (4 %): the window's median follows, the warning goes.
        let r = run(&mut w, 5.0, WINDOW, true);
        assert!(!r.warn);
    }

    #[test]
    fn no_idle_reading_means_no_sag_and_a_missing_vin_is_skipped() {
        let mut w = PsuSag::new(12.0);
        let r = run(&mut w, 12.0, WINDOW, true);
        assert_eq!(r.sag_v, None);
        assert!(!r.warn);
        let r = w.sample(None, true);
        assert_eq!(r.load_v, Some(12.0));
        // Two idle samples are not enough yet.
        let mut w = PsuSag::new(12.0);
        run(&mut w, 12.4, IDLE_MIN - 1, false);
        let r = run(&mut w, 12.0, 5, true);
        assert_eq!(r.idle_v, None);
    }

    #[test]
    fn a_chips_off_on_cycle_takes_a_new_idle_voltage() {
        let mut w = PsuSag::new(12.0);
        run(&mut w, 12.4, 5, false);
        run(&mut w, 12.1, 10, true);
        // A different supply plugged in while off: the new idle counts.
        let r = run(&mut w, 12.0, 5, false);
        assert_eq!(r.idle_v, Some(12.0));
        let r = run(&mut w, 11.9, 3, true);
        assert!((r.sag_v.unwrap() - 0.1).abs() < 1e-4);
    }
}

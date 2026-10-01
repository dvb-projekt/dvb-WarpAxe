//! Fan regulator: a PID on the chip temperature, the same controller
//! and gains as ESP-Miner's fan task (Arduino-PID semantics, reverse
//! action, 100 ms sample time, output clamped to [min fan, 100]).

/// ESP-Miner gains and defaults.
pub const KP: f32 = 5.0;
/// Integral gain, per second.
pub const KI: f32 = 0.1;
/// Derivative gain, per second.
pub const KD: f32 = 2.0;
/// Sample period the gains are tuned for, ms.
pub const SAMPLE_MS: u32 = 100;
/// AxeOS `temptarget` default, °C.
pub const DEFAULT_TARGET_C: f32 = 60.0;
/// AxeOS `minFanSpeed` default, %.
pub const DEFAULT_MIN_PERCENT: f32 = 25.0;

#[derive(Debug, Clone)]
/// The regulator state between samples.
pub struct FanPid {
    kp: f32,
    ki: f32,
    kd: f32,
    out_min: f32,
    out_max: f32,
    sum: f32,
    last_input: Option<f32>,
}

impl FanPid {
    /// A regulator with the reference gains, output within
    /// `min_percent..=100`.
    pub fn new(min_percent: f32) -> Self {
        let mut pid = FanPid {
            kp: 0.0,
            ki: 0.0,
            kd: 0.0,
            out_min: 0.0,
            out_max: 100.0,
            sum: 0.0,
            last_input: None,
        };
        pid.set_gains(KP, KI, KD);
        pid.set_min_percent(min_percent);
        pid
    }

    /// Gains in the ESP-Miner/NerdOS convention: `ki` and `kd` per
    /// second. Negative or non-finite values count as zero.
    pub fn set_gains(&mut self, kp: f32, ki: f32, kd: f32) {
        let ok = |g: f32| if g.is_finite() { g.max(0.0) } else { 0.0 };
        let dt = SAMPLE_MS as f32 / 1000.0;
        // Reverse action: more temperature, more fan.
        self.kp = -ok(kp);
        self.ki = -(ok(ki) * dt);
        self.kd = -(ok(kd) / dt);
    }

    /// Lower output bound, %; the integral is clamped into the new window.
    pub fn set_min_percent(&mut self, min_percent: f32) {
        self.out_min = min_percent.clamp(0.0, 100.0);
        self.sum = self.sum.clamp(self.out_min, self.out_max);
    }

    /// Start from a known output, e.g. the current duty when switching
    /// from manual to automatic (Arduino PID "bumpless" transfer).
    pub fn reset(&mut self, output: f32, input: f32) {
        self.sum = output.clamp(self.out_min, self.out_max);
        self.last_input = Some(input);
    }

    /// One sample: the fan duty in percent for this chip temperature.
    pub fn compute(&mut self, temp_c: f32, target_c: f32) -> f32 {
        let error = target_c - temp_c;
        self.sum = (self.sum + self.ki * error).clamp(self.out_min, self.out_max);
        let d_input = temp_c - self.last_input.unwrap_or(temp_c);
        self.last_input = Some(temp_c);
        (self.kp * error + self.sum - self.kd * d_input).clamp(self.out_min, self.out_max)
    }
}

/// How close to the target the chips must be for the warm-up hold to
/// end, °C.
pub const WARMUP_MARGIN_C: f32 = 3.0;
/// The hold also ends when the temperature has not risen by
/// [`WARMUP_RISE_C`] within this many seconds (a board that settles
/// below the target at full fan), …
pub const WARMUP_FLAT_S: u32 = 30;
/// … the rise that counts, °C, …
pub const WARMUP_RISE_C: f32 = 0.5;
/// … and at the latest after this many seconds.
pub const WARMUP_MAX_S: u32 = 120;

/// Fans at full while the chips warm up after the core rail comes on.
///
/// A PID started on a cold board winds down to its minimum at once (the
/// error is far below the target) and only climbs back as the chips
/// approach the target; a multi-chip board heats faster than that
/// climb. NerdOctaxe-γ v2.2, 165 W, 2026-10-01: core on at 38 °C, fans
/// at 28 % after 28 s, 70 °C limit crossed 60 s after the chain came
/// up, twice. The hold keeps the fans at full until the chips are
/// within [`WARMUP_MARGIN_C`] of the target, or stop rising, or
/// [`WARMUP_MAX_S`] have passed; the PID then takes over from full
/// duty (its integral preset to 100), so nothing winds down.
#[derive(Debug, Clone)]
pub struct Warmup {
    started_s: u32,
    peak_c: Option<f32>,
    peak_at_s: u32,
    done: bool,
}

impl Warmup {
    /// The core rail came on at `now_s`.
    pub fn start(now_s: u32) -> Self {
        Warmup {
            started_s: now_s,
            peak_c: None,
            peak_at_s: now_s,
            done: false,
        }
    }

    /// Whether the fans must still run at full at `now_s`, given the
    /// chip temperature (`None`: no reading, which holds) and the PID's
    /// target. Once over, stays over.
    pub fn holding(&mut self, now_s: u32, temp_c: Option<f32>, target_c: f32) -> bool {
        if self.done {
            return false;
        }
        let since_start = now_s.saturating_sub(self.started_s);
        if since_start >= WARMUP_MAX_S {
            self.done = true;
            return false;
        }
        if let Some(t) = temp_c {
            if t >= target_c - WARMUP_MARGIN_C {
                self.done = true;
                return false;
            }
            match self.peak_c {
                Some(p) if t < p + WARMUP_RISE_C => {}
                _ => {
                    self.peak_c = Some(t);
                    self.peak_at_s = now_s;
                }
            }
            if since_start >= WARMUP_FLAT_S && now_s.saturating_sub(self.peak_at_s) >= WARMUP_FLAT_S
            {
                self.done = true;
                return false;
            }
        }
        true
    }

    /// When the hold began, s.
    pub fn started_s(&self) -> u32 {
        self.started_s
    }

    /// The hold is over.
    pub fn done(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warm_up_hold_ends_near_the_target() {
        let mut w = Warmup::start(10);
        assert!(w.holding(11, Some(38.0), 60.0));
        assert!(w.holding(40, Some(50.0), 60.0));
        assert!(!w.holding(50, Some(57.0), 60.0), "within 3 °C of 60");
        assert!(!w.holding(51, Some(40.0), 60.0), "stays over");
        assert!(w.done());
    }

    #[test]
    fn the_warm_up_hold_ends_when_the_chips_stop_rising_or_at_the_cap() {
        // A single-chip board that settles at 45 °C under full fan.
        let mut w = Warmup::start(0);
        for s in 0..20 {
            assert!(w.holding(s, Some(30.0 + s as f32), 60.0));
        }
        for s in 20..49 {
            assert!(w.holding(s, Some(49.0 + (s % 2) as f32 * 0.2), 60.0), "{s}");
        }
        assert!(!w.holding(50, Some(49.1), 60.0), "flat for 30 s");
        // Not before 30 s from the start, even when flat from the first
        // sample (the chain is not up yet, the chips idle).
        let mut w = Warmup::start(100);
        for s in 100..130 {
            assert!(w.holding(s, Some(30.0), 60.0), "{s}");
        }
        assert!(!w.holding(130, Some(30.0), 60.0));
        // No reading: holds until the cap.
        let mut w = Warmup::start(0);
        assert!(w.holding(WARMUP_MAX_S - 1, None, 60.0));
        assert!(!w.holding(WARMUP_MAX_S, None, 60.0));
    }

    /// A thermal model of a 165 W eight-chip board: the die settles at
    /// 58 °C under full fan and 157 °C at 25 %, time constant 60 s; the
    /// board sensor follows the die with 20 s, the airflow follows the
    /// fan duty with 15 s (heatsink mass and spin-up). The field data of
    /// 2026-10-01 (38 → 56 °C in 28 s at 28 % fan, 70 °C limit crossed
    /// 60 s after the chain came up) is of that kind. The PID alone lets
    /// the sensor through the 70 °C limit; with the warm-up hold it
    /// stays under.
    fn run_board(hold: bool) -> (f32, f32) {
        let (ambient, watts, r_full, k) = (25.0f32, 165.0f32, 0.2f32, 3.0f32);
        let (tau_die, tau_sensor, tau_fan) = (60.0f32, 20.0f32, 15.0f32);
        let target = 60.0;
        let mut pid = FanPid::new(25.0);
        pid.reset(100.0, target);
        let mut warm = hold.then(|| Warmup::start(0));
        let (mut die, mut sensor, mut airflow) = (38.0f32, 38.0f32, 100.0f32);
        let mut peak = sensor;
        let mut duty_at_end = 0.0;
        let dt = SAMPLE_MS as f32 / 1000.0;
        for i in 0..(600 * 1000 / SAMPLE_MS) {
            let now_s = i * SAMPLE_MS / 1000;
            let holding = warm
                .as_mut()
                .is_some_and(|w| w.holding(now_s, Some(sensor), target));
            if !holding && warm.is_some() {
                pid.reset(100.0, sensor);
                warm = None;
            }
            let duty = if holding {
                100.0
            } else {
                pid.compute(sensor, target)
            };
            airflow += (duty - airflow) / tau_fan * dt;
            let steady = ambient + watts * r_full * (1.0 + k * (1.0 - airflow / 100.0));
            die += (steady - die) / tau_die * dt;
            sensor += (die - sensor) / tau_sensor * dt;
            peak = peak.max(sensor);
            duty_at_end = duty;
        }
        (peak, duty_at_end)
    }

    #[test]
    fn a_165_w_board_crosses_70_c_on_the_pid_alone_and_stays_under_with_the_hold() {
        let (peak_bare, _) = run_board(false);
        assert!(peak_bare > 70.0, "bare PID peaks at {peak_bare}");
        let (peak_hold, duty) = run_board(true);
        assert!(peak_hold < 67.0, "with the hold: {peak_hold}");
        assert!(
            duty > 80.0 && duty <= 100.0,
            "settles on a high duty: {duty}"
        );
    }

    #[test]
    fn hot_chip_drives_the_fan_up_cold_chip_rests_at_minimum() {
        let mut pid = FanPid::new(25.0);
        let mut out = 0.0;
        for _ in 0..1000 {
            out = pid.compute(70.0, 60.0);
        }
        assert_eq!(out, 100.0);
        for _ in 0..2000 {
            out = pid.compute(40.0, 60.0);
        }
        assert_eq!(out, 25.0);
    }

    #[test]
    fn at_target_the_integral_holds_the_duty() {
        let mut pid = FanPid::new(25.0);
        pid.reset(60.0, 60.0);
        for _ in 0..10 {
            assert_eq!(pid.compute(60.0, 60.0), 60.0);
        }
        // One degree hot: a derivative kick on the step, then 5 % over
        // the held duty from the proportional term, integral creeping.
        let kick = pid.compute(61.0, 60.0);
        assert!(kick > 65.0 && kick <= 100.0, "{kick}");
        let held = pid.compute(61.0, 60.0);
        assert!(held > 65.0 && held < 66.0, "{held}");
    }

    #[test]
    fn new_gains_change_the_response_not_the_state() {
        let mut soft = FanPid::new(25.0);
        let mut hard = FanPid::new(25.0);
        soft.reset(50.0, 60.0);
        hard.reset(50.0, 60.0);
        hard.set_gains(KP * 2.0, KI, KD);
        assert!(hard.compute(60.1, 60.0) > soft.compute(60.1, 60.0));
        let mut off = FanPid::new(25.0);
        off.reset(50.0, 60.0);
        off.set_gains(0.0, 0.0, 0.0);
        assert_eq!(off.compute(80.0, 60.0), 50.0);
        off.set_gains(f32::NAN, -1.0, 0.0);
        assert_eq!(off.compute(80.0, 60.0), 50.0);
    }

    #[test]
    fn output_never_leaves_the_window() {
        let mut pid = FanPid::new(30.0);
        for t in [-20.0, 150.0, 60.0, 0.0, 200.0] {
            let o = pid.compute(t, 60.0);
            assert!((30.0..=100.0).contains(&o), "{t} -> {o}");
        }
    }
}

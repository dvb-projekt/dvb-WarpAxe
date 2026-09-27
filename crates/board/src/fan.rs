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

#[cfg(test)]
mod tests {
    use super::*;

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

//! The board around the chip: watts, degrees, a regulator, a fan.
//!
//! [`SimChip`](crate::SimChip) knows nothing of power or heat — a real
//! chip does not either; those are the board's. This module is the
//! minimal physics an operating-point regulator reacts to:
//!
//! - **Power**: `P = P_idle + k · f · V²`, the dynamic-power law every
//!   tuner assumes. `k` is fitted once per board from a measured point.
//! - **Chip temperature**: a first-order lag towards
//!   `T_ambient + R_th · P`, time constant `τ` — the heatsink.
//! - **VR temperature**: the same shape with its own `R` and a warmer
//!   offset; the regulator's power stage runs hotter than the die.
//! - **Fan**: a PID-less proportional curve on chip temperature, because
//!   that is all the firmware's own fan control amounts to here.
//!
//! And the failure that started this project:
//! [`BoardFault::SilentVrShutdown`] — above a threshold the regulator's
//! output collapses to zero: the chip stops, power falls to idle, the
//! board cools, and **no flag is raised**. The reported core voltage
//! set-point stays where it was; only `core_voltage_actual` and the
//! power say what happened.
//!
//! Numbers for the Bitaxe Gamma come from WarpTune's measurements
//! (`warptune.toml`, journal 2026-09): 20.3 W at 640 MHz / 1140 mV,
//! `thermal_r_c_per_w = 1.75`, chip ~55 °C and VR ~70 °C at 23 °C
//! ambient.

/// Parameters of one board design.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardSpec {
    /// Name as the firmware would report it (`boardVersion`).
    pub name: &'static str,
    /// Nominal input voltage, V (5 or 12).
    pub input_v: f64,
    /// Idle draw with the chip unclocked, W (MCU, fan, regulator losses).
    pub idle_w: f64,
    /// Dynamic-power coefficient: W per (Hz · V²).
    pub k_w_per_hz_v2: f64,
    /// Chip thermal resistance to ambient, K/W.
    pub r_chip_k_per_w: f64,
    /// Chip thermal time constant, s.
    pub tau_chip_s: f64,
    /// VR thermal resistance, K/W, plus a fixed offset above ambient.
    pub r_vr_k_per_w: f64,
    /// VR offset above ambient at zero load, K.
    pub vr_offset_k: f64,
    /// VR thermal time constant, s.
    pub tau_vr_s: f64,
    /// Fan: 100 % at this chip temperature and above.
    pub fan_full_at_c: f64,
    /// Fan: minimum duty, %.
    pub fan_min_pct: f64,
    /// Regulator voltage drop under load, mV per W.
    pub vr_droop_mv_per_w: f64,
    /// Firmware overheat threshold, °C — the board stops the chip above it.
    pub overheat_c: f64,
    /// Firmware power limit, W (`maxPower`).
    pub max_power_w: f64,
}

impl BoardSpec {
    /// Bitaxe Gamma 60x (one BM1370, 5 V). Fitted to 20.3 W at
    /// 640 MHz / 1140 mV with 3 W idle.
    pub const fn gamma() -> Self {
        BoardSpec {
            name: "602",
            input_v: 5.0,
            idle_w: 3.0,
            k_w_per_hz_v2: 2.08e-8,
            r_chip_k_per_w: 1.75,
            tau_chip_s: 60.0,
            r_vr_k_per_w: 1.6,
            vr_offset_k: 15.0,
            tau_vr_s: 40.0,
            fan_full_at_c: 62.0,
            fan_min_pct: 35.0,
            vr_droop_mv_per_w: 0.5,
            overheat_c: 75.0,
            max_power_w: 40.0,
        }
    }
}

/// Board-level misbehaviour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoardFault {
    /// Above this hash clock the VR output collapses to 0 V. The chip
    /// stops (no nonces), power drops to idle, temperatures decay. No
    /// flag. The set-point still reads as commanded. Cleared only by a
    /// restart. The Gaia, 2026-09-22, 11:44.
    SilentVrShutdown {
        /// Threshold in Hz.
        above_hz: u64,
    },
    /// The chip temperature sensor reads a constant (0 on the Gaia:
    /// `asicTemps: [0]`). The firmware's own thermal loop is blind.
    ChipSensorStuck {
        /// The value it reports.
        at_c: f64,
    },
    /// Ambient rises by this much (a closed cabinet, a hot day).
    AmbientRise {
        /// Kelvin.
        by_k: f64,
    },
}

/// The board's state.
#[derive(Debug, Clone, PartialEq)]
pub struct Board {
    spec: BoardSpec,
    ambient_c: f64,
    /// Commanded core voltage, mV.
    core_mv: u16,
    /// Hash clock the chip is running at, Hz (0 = unclocked).
    freq_hz: u64,
    chip_c: f64,
    vr_c: f64,
    vr_alive: bool,
    faults: Vec<BoardFault>,
}

impl Board {
    /// A board at ambient, chip unclocked, VR alive.
    pub fn new(spec: BoardSpec, ambient_c: f64) -> Self {
        Board {
            ambient_c,
            core_mv: 0,
            freq_hz: 0,
            chip_c: ambient_c,
            vr_c: ambient_c + spec.vr_offset_k,
            vr_alive: true,
            faults: Vec::new(),
            spec,
        }
    }

    /// The design.
    pub fn spec(&self) -> &BoardSpec {
        &self.spec
    }

    /// Inject a board fault.
    pub fn inject(&mut self, fault: BoardFault) {
        if let BoardFault::AmbientRise { by_k } = fault {
            self.ambient_c += by_k;
        }
        self.faults.push(fault);
        self.check_vr();
    }

    /// Set the operating point the firmware commands. Order matters on
    /// a real board and is the firmware's job; here both land at once.
    pub fn set_point(&mut self, freq_hz: u64, core_mv: u16) {
        self.freq_hz = freq_hz;
        self.core_mv = core_mv;
        self.check_vr();
    }

    /// Simulate a firmware restart: the VR comes back, the point is
    /// whatever the firmware sets next.
    pub fn restart(&mut self) {
        self.vr_alive = true;
        self.freq_hz = 0;
        self.core_mv = 0;
    }

    fn check_vr(&mut self) {
        for f in &self.faults {
            if let BoardFault::SilentVrShutdown { above_hz } = f {
                if self.freq_hz > *above_hz && self.core_mv > 0 {
                    self.vr_alive = false;
                }
            }
        }
    }

    /// Is the chip actually being powered and clocked?
    pub fn chip_running(&self) -> bool {
        self.vr_alive && self.freq_hz > 0 && self.core_mv > 0
    }

    /// Effective hash clock: what the chip really runs at (0 when the
    /// VR is down or the point is unset).
    pub fn effective_freq_hz(&self) -> u64 {
        if self.chip_running() {
            self.freq_hz
        } else {
            0
        }
    }

    /// Commanded core voltage, mV (what the API reports as `coreVoltage`).
    pub fn core_mv(&self) -> u16 {
        self.core_mv
    }

    /// Measured core voltage, mV (`coreVoltageActual`): commanded minus
    /// droop under load, or 0 with the VR down.
    pub fn core_mv_actual(&self) -> u16 {
        if !self.vr_alive {
            return 0;
        }
        let droop = self.power_w() * self.spec.vr_droop_mv_per_w;
        (f64::from(self.core_mv) - droop).max(0.0) as u16
    }

    /// Instantaneous input power, W.
    pub fn power_w(&self) -> f64 {
        if !self.chip_running() {
            return self.spec.idle_w;
        }
        let v = f64::from(self.core_mv) / 1000.0;
        self.spec.idle_w + self.spec.k_w_per_hz_v2 * self.freq_hz as f64 * v * v
    }

    /// Input current, A.
    pub fn current_a(&self) -> f64 {
        self.power_w() / self.spec.input_v
    }

    /// Chip temperature as the sensor reports it, °C.
    pub fn chip_c(&self) -> f64 {
        for f in &self.faults {
            if let BoardFault::ChipSensorStuck { at_c } = f {
                return *at_c;
            }
        }
        self.chip_c
    }

    /// True chip temperature, °C, regardless of the sensor.
    pub fn chip_c_true(&self) -> f64 {
        self.chip_c
    }

    /// VR temperature, °C.
    pub fn vr_c(&self) -> f64 {
        self.vr_c
    }

    /// Ambient, °C.
    pub fn ambient_c(&self) -> f64 {
        self.ambient_c
    }

    /// Fan duty, %: proportional between ambient+10 and `fan_full_at_c`.
    pub fn fan_pct(&self) -> f64 {
        let lo = self.ambient_c + 10.0;
        let hi = self.spec.fan_full_at_c;
        let t = self.chip_c();
        if t >= hi {
            100.0
        } else if t <= lo {
            self.spec.fan_min_pct
        } else {
            self.spec.fan_min_pct + (100.0 - self.spec.fan_min_pct) * (t - lo) / (hi - lo)
        }
    }

    /// Would the firmware's overheat guard trip?
    pub fn overheated(&self) -> bool {
        self.chip_c() >= self.spec.overheat_c
    }

    /// Advance the thermal state by `dt_s` seconds (first-order lags).
    pub fn step(&mut self, dt_s: f64) {
        let p = self.power_w();
        // Fan helps: at 100 % the thermal resistance is the spec value;
        // at minimum duty it is ~40 % higher. Crude but gives the
        // regulator a fan effect to see.
        let fan_factor = 1.4 - 0.4 * (self.fan_pct() / 100.0);
        let chip_target = self.ambient_c + self.spec.r_chip_k_per_w * fan_factor * p;
        let vr_target = self.ambient_c + self.spec.vr_offset_k + self.spec.r_vr_k_per_w * p;
        let a_chip = 1.0 - (-dt_s / self.spec.tau_chip_s).exp();
        let a_vr = 1.0 - (-dt_s / self.spec.tau_vr_s).exp();
        self.chip_c += (chip_target - self.chip_c) * a_chip;
        self.vr_c += (vr_target - self.vr_c) * a_vr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gamma() -> Board {
        Board::new(BoardSpec::gamma(), 23.0)
    }

    #[test]
    fn fitted_point_reproduces_the_measured_watts() {
        let mut b = gamma();
        b.set_point(640_625_000, 1140);
        assert!((b.power_w() - 20.3).abs() < 0.4, "{}", b.power_w());
        b.set_point(525_000_000, 1150);
        assert!((b.power_w() - 17.4).abs() < 0.5, "{}", b.power_w());
        b.set_point(0, 1150);
        assert_eq!(b.power_w(), 3.0, "unclocked: idle");
    }

    #[test]
    fn temperatures_settle_towards_ambient_plus_r_times_p() {
        let mut b = gamma();
        b.set_point(640_625_000, 1140);
        for _ in 0..600 {
            b.step(1.0);
        }
        // At 20.3 W: 23 + 1.75 · 20.3 ≈ 58.5 with the fan flat out; the
        // fan curve sits at ~95 % there, so the settled point is ~59.4.
        assert!((b.chip_c() - 59.0).abs() < 1.5, "chip {}", b.chip_c());
        assert!(b.vr_c() > b.chip_c(), "VR runs hotter: {}", b.vr_c());
        assert!(b.fan_pct() > 90.0, "fan {}", b.fan_pct());
    }

    /// The Gaia of 2026-09-22: above the threshold the VR dies quietly.
    #[test]
    fn silent_vr_shutdown_drops_power_and_actual_voltage_but_keeps_the_setpoint() {
        let mut b = gamma();
        b.inject(BoardFault::SilentVrShutdown {
            above_hz: 600_000_000,
        });
        b.set_point(575_000_000, 1180);
        for _ in 0..300 {
            b.step(1.0);
        }
        assert!(b.chip_running());
        let hot = b.chip_c();
        b.set_point(625_000_000, 1200);
        assert!(!b.chip_running());
        assert_eq!(b.effective_freq_hz(), 0);
        assert_eq!(b.power_w(), 3.0);
        assert_eq!(b.core_mv(), 1200, "set-point still reads as commanded");
        assert_eq!(b.core_mv_actual(), 0, "…but the actual voltage is gone");
        for _ in 0..120 {
            b.step(1.0);
        }
        assert!(
            b.chip_c() < hot - 10.0,
            "board cools: {} → {}",
            hot,
            b.chip_c()
        );
        // A PATCH back down does not revive it.
        b.set_point(525_000_000, 1150);
        assert!(!b.chip_running());
        // A restart does.
        b.restart();
        b.set_point(525_000_000, 1150);
        assert!(b.chip_running());
    }

    #[test]
    fn stuck_sensor_hides_the_true_temperature() {
        let mut b = gamma();
        b.inject(BoardFault::ChipSensorStuck { at_c: 0.0 });
        b.set_point(640_625_000, 1140);
        for _ in 0..600 {
            b.step(1.0);
        }
        assert_eq!(b.chip_c(), 0.0);
        assert!(b.chip_c_true() > 50.0);
        assert_eq!(
            b.fan_pct(),
            BoardSpec::gamma().fan_min_pct,
            "fan follows the lying sensor"
        );
    }

    #[test]
    fn ambient_rise_raises_the_settled_temperature() {
        let mut a = gamma();
        let mut b = gamma();
        b.inject(BoardFault::AmbientRise { by_k: 10.0 });
        for x in [&mut a, &mut b] {
            x.set_point(525_000_000, 1150);
            for _ in 0..600 {
                x.step(1.0);
            }
        }
        // Less than the full 10 K: the fan is harder at work on the warmer board.
        let diff = b.chip_c() - a.chip_c();
        assert!(diff > 6.0 && diff < 11.0, "diff {diff}");
    }

    #[test]
    fn droop_lowers_actual_voltage_with_load() {
        let mut b = gamma();
        b.set_point(525_000_000, 1150);
        let low = b.core_mv_actual();
        b.set_point(640_625_000, 1150);
        assert!(b.core_mv_actual() < low);
        assert!(b.core_mv_actual() > 1100);
    }
}

//! WarpTune's regulator, in-process, against the simulated device.
//!
//! No HTTP, no wall clock. [`Bench`] owns a [`Device`] and a
//! [`Regulator`], steps both in virtual time, turns the device's
//! `/api/system/info` document into the regulator's [`Sample`] the same
//! way the sidecar does (serde → `SystemInfo` → `Sample::from_info`), and
//! applies every [`Decision`] the way the runner does: write the point,
//! `note_applied`, and reboot the device when the decision says so.
//!
//! What the regulator sees is exactly what it would see over the wire;
//! what it cannot see is what a test asserts on — the device's ground
//! truth. A six-minute live run becomes a test that finishes in
//! milliseconds and fails with a journal-shaped diff.
//!
//! The [`Limits`] used here shorten the gates (settle, judge, startup)
//! and loosen the hashrate verdicts: over a thirty-second window the
//! simulated hashrate is ±20 % noisy, and these scenarios are about the
//! guards and the step memory, not about hashrate judgements.

#![forbid(unsafe_code)]

use warpaxe_axeos::device::Device;
use warpaxe_core::chip::bm1370::BM1370;
use warpaxe_sim::{BoardFault, BoardSpec, Fault};
use warppool_tune::axeos::SystemInfo;
use warppool_tune::config::VoltageAnchor;
use warppool_tune::regulator::Point;
use warppool_tune::{Action, Decision, Ladder, Limits, MinerSettings, Mode, Regulator, Sample};

/// One applied decision, for assertions.
#[derive(Debug, Clone)]
pub struct Applied {
    /// Virtual time of the decision, s.
    pub at_s: u64,
    /// The decision as the regulator returned it.
    pub decision: Decision,
}

/// A device and a regulator on the same virtual clock.
pub struct Bench {
    /// The simulated miner.
    pub device: Device,
    /// WarpTune's regulator.
    pub reg: Regulator,
    now_s: u64,
    /// Seconds between samples (the sidecar's `tick_secs`).
    pub tick_s: u64,
    /// Every decision that was not a plain hold, in order.
    pub applied: Vec<Applied>,
    /// Points written to the device, in order.
    pub writes: Vec<(u64, Point)>,
    /// Reboots the regulator requested.
    pub restarts: u32,
    /// Samples the regulator could not read (e.g. no temperature).
    pub sample_errors: u32,
}

/// Regulator limits for tests: short gates, hashrate verdicts loosened.
pub fn fast_limits() -> Limits {
    Limits {
        settle_s: 6,
        judge_after_s: 30,
        startup_s: 4,
        hash_settle_s: 10,
        hashrate_up_min_ratio: 0.70,
        hashrate_down_ratio: 0.40,
        ..Limits::default()
    }
}

/// Max-mode settings like the Gamma's in `warptune.toml`, with the
/// measured thermal resistance.
pub fn max_settings(target_temp_c: f64, max_watts: f64, cap_mhz: u16) -> MinerSettings {
    MinerSettings {
        mode: Mode::Max,
        target_temp_c,
        max_watts,
        voltage_offset_mv: 0,
        voltage_anchor: VoltageAnchor::default(),
        beyond_spec: true,
        ladder_cap_mhz: Some(cap_mhz),
        thermal_r_c_per_w: Some(1.75),
        eco_cap_mhz: None,
        voltage_trial: false,
        voltage_min_mv: None,
        voltage_max_mv: None,
    }
}

impl Bench {
    /// A Bitaxe Gamma at 23 °C ambient, regulator built like the runner
    /// builds it for a BM1370, device already running at `stock`.
    pub fn gamma(settings: MinerSettings, limits: Limits, stock: Point, seed: u64) -> Self {
        let mut device = Device::new(&BM1370, BoardSpec::gamma(), 23.0, seed);
        device.set_point(Some(stock.0), Some(stock.1));
        device.tick(1000);
        let ladder = Ladder::for_asic(
            "BM1370",
            settings.voltage_offset_mv,
            settings.beyond_spec,
            settings.ladder_cap_mhz,
        )
        .expect("BM1370 ladder");
        let decide_secs = 10;
        let reg = Regulator::new(ladder, settings, limits, Some(stock), decide_secs);
        Bench {
            device,
            reg,
            now_s: 1,
            tick_s: 2,
            applied: Vec::new(),
            writes: Vec::new(),
            restarts: 0,
            sample_errors: 0,
        }
    }

    /// Virtual time, s.
    pub fn now_s(&self) -> u64 {
        self.now_s
    }

    /// Inject a chip fault.
    pub fn chip_fault(&mut self, f: Fault) {
        self.device.inject_chip(f);
    }

    /// Inject a board fault.
    pub fn board_fault(&mut self, f: BoardFault) {
        self.device.inject_board(f);
    }

    /// Run `secs` of virtual time: tick the device, sample, decide, apply.
    pub fn run_for(&mut self, secs: u64) {
        let end = self.now_s + secs;
        while self.now_s < end {
            self.device.tick(self.tick_s * 1000);
            self.now_s += self.tick_s;
            let info: SystemInfo = serde_json::from_value(self.device.system_info())
                .expect("device JSON is what the sidecar parses");
            let sample = match Sample::from_info(&info, self.now_s) {
                Ok(s) => s,
                Err(_) => {
                    self.sample_errors += 1;
                    self.reg.on_probe_failure();
                    continue;
                }
            };
            if let Some(d) = self.reg.on_sample(&sample) {
                self.apply(d);
            }
        }
    }

    /// Apply a decision the way `runner.rs` does.
    fn apply(&mut self, d: Decision) {
        if d.action.changes_operating_point() {
            self.device.set_point(Some(d.to.0), Some(d.to.1));
            self.writes.push((self.now_s, d.to));
            self.reg.note_applied(self.now_s);
        }
        if d.restart {
            self.device.restart();
            self.restarts += 1;
        }
        if d.action != Action::Hold || d.restart {
            self.applied.push(Applied {
                at_s: self.now_s,
                decision: d,
            });
        }
    }

    /// Decisions of one action.
    pub fn of(&self, action: Action) -> Vec<&Applied> {
        self.applied
            .iter()
            .filter(|a| a.decision.action == action)
            .collect()
    }

    /// First decision whose reason contains `needle`.
    pub fn first_reason(&self, needle: &str) -> Option<&Applied> {
        self.applied
            .iter()
            .find(|a| a.decision.reason.contains(needle))
    }

    /// The device's current point as the API reports it.
    pub fn device_point(&self) -> Point {
        let info = self.device.system_info();
        (
            info["frequency"].as_u64().unwrap_or(0) as u16,
            info["coreVoltage"].as_u64().unwrap_or(0) as u16,
        )
    }

    /// A short journal-like rendering for failure messages.
    pub fn transcript(&self) -> String {
        let mut s = String::new();
        for a in &self.applied {
            let d = &a.decision;
            s.push_str(&format!(
                "{:>5}s {:<8} {}→{} {}{}\n",
                a.at_s,
                d.action.as_str(),
                d.from.0,
                d.to.0,
                if d.restart { "[restart] " } else { "" },
                d.reason.chars().take(110).collect::<String>()
            ));
        }
        s
    }
}

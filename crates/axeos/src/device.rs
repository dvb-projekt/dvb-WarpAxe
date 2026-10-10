//! A whole miner, as a tuner sees it: chip on a board under a firmware.
//!
//! [`Device`] is the model behind the HTTP API. It owns a
//! [`SimChain`], a [`Board`], and the little bit of firmware behaviour
//! that turns the two into API numbers: bring the chip up, set the PLL
//! for the commanded frequency, hand it a job every 1.5 s, count the
//! nonces that come back as shares, keep the rolling hashrate averages
//! ESP-Miner reports, stop on overheat. Time is stepped by the caller
//! ([`Device::tick`]) so a test can run an hour in a millisecond and the
//! server can run at wall-clock speed or any multiple of it.
//!
//! Numbers follow ESP-Miner where a tuner depends on them: `hashRate` is
//! nonce-based (found nonces × difficulty × 2³² over a window), not the
//! nominal `frequency × smallCoreCount` — that one is `expectedHashrate`.
//! A chip that has stopped therefore shows a decaying hashrate while its
//! expected figure stays put, which is exactly the trap the real
//! firmware laid on 2026-09-22.

use warpaxe_core::chip::{render, ChipProfile, Rendered};
use warpaxe_core::decoder::RxDecoder;
use warpaxe_core::frame::{encode_cmd, encode_job, Group, Op, MAX_FRAME_LEN};
use warpaxe_core::job::{Job, JobIds};
use warpaxe_core::pll::solve_mhz;
use warpaxe_core::regs::{self, reg, write_payload};
use warpaxe_core::result::JobResult;
use warpaxe_sim::{Board, BoardFault, BoardSpec, Fault, SimChain};

use std::collections::VecDeque;

/// MurmurHash3's 32-bit finalizer: spreads a nonce's structured bits
/// into something uniform enough for a share-or-not coin flip.
fn mix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// Interval between jobs, as the NerdAxe firmware uses it.
const JOB_INTERVAL_MS: u64 = 1500;
/// Window for `hashRate`, s.
const HR_WINDOW_S: u64 = 60;
/// Window for `hashRate_10m`, s.
const HR_10M_S: u64 = 600;

/// Static identity the API reports.
#[derive(Debug, Clone)]
pub struct Identity {
    /// `hostname`.
    pub hostname: String,
    /// `macAddr`.
    pub mac: String,
    /// `version`.
    pub version: String,
    /// `stratumUser` (`<wallet>.<worker>`).
    pub stratum_user: String,
}

impl Default for Identity {
    fn default() -> Self {
        Identity {
            hostname: "warpaxe-sim".into(),
            mac: "02:57:41:52:50:00".into(),
            version: "warpaxe-axeos 0.1.0".into(),
            stratum_user: "bc1qsimulator.warpaxe".into(),
        }
    }
}

/// The simulated miner.
#[derive(Debug)]
pub struct Device {
    profile: &'static ChipProfile,
    chain: SimChain,
    board: Board,
    identity: Identity,
    rx: RxDecoder,
    job_ids: JobIds,
    /// Commanded operating point, as the API reports it.
    frequency_mhz: u16,
    core_mv: u16,
    /// Wall of the simulation, ms.
    now_ms: u64,
    boot_ms: u64,
    last_job_ms: u64,
    /// Nonce arrival times (ms) for the rolling hashrate windows.
    nonces: VecDeque<u64>,
    shares_accepted: u64,
    shares_rejected: u64,
    duplicate_hw_nonces: u64,
    last_nonce: Option<u32>,
    pool_difficulty: u32,
    overheat_mode: bool,
    mining_paused: bool,
    restarts: u32,
    dirty_point: bool,
}

impl Device {
    /// A miner at ambient, booted, running at the profile's stock point.
    pub fn new(profile: &'static ChipProfile, board: BoardSpec, ambient_c: f64, seed: u64) -> Self {
        let stock_mhz = (profile.stock.0 / 1_000_000) as u16;
        let stock_mv = profile.stock.1;
        let mut d = Device {
            profile,
            chain: SimChain::new(profile, 1, seed),
            board: Board::new(board, ambient_c),
            identity: Identity::default(),
            rx: RxDecoder::new(),
            job_ids: JobIds::with_step(profile.job_id_step),
            frequency_mhz: stock_mhz,
            core_mv: stock_mv,
            now_ms: 0,
            boot_ms: 0,
            last_job_ms: 0,
            nonces: VecDeque::new(),
            shares_accepted: 0,
            shares_rejected: 0,
            duplicate_hw_nonces: 0,
            last_nonce: None,
            pool_difficulty: profile.default_difficulty,
            overheat_mode: false,
            mining_paused: false,
            restarts: 0,
            dirty_point: true,
        };
        d.boot();
        d
    }

    /// Replace the reported identity.
    pub fn set_identity(&mut self, id: Identity) {
        self.identity = id;
    }

    /// Pool share difficulty the firmware submits at.
    pub fn set_pool_difficulty(&mut self, d: u32) {
        self.pool_difficulty = d.max(1);
    }

    /// The chip profile.
    pub fn profile(&self) -> &'static ChipProfile {
        self.profile
    }

    /// The board (read-only view for `/sim/state`).
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// The chain (read-only view for `/sim/state`).
    pub fn chain(&self) -> &SimChain {
        &self.chain
    }

    /// Simulated time, ms.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Inject a chip fault.
    pub fn inject_chip(&mut self, f: Fault) {
        self.chain.inject_all(f);
    }

    /// Inject a board fault.
    pub fn inject_board(&mut self, f: BoardFault) {
        self.board.inject(f);
    }

    /// Clear every chip fault (board faults are physical and stay).
    pub fn clear_chip_faults(&mut self) {
        for i in 0..self.chain.chips().len() {
            self.chain.chip_mut(i).clear_faults();
        }
    }

    /// `PATCH /api/system {frequency, coreVoltage}`: takes effect on the
    /// next tick, like the firmware's power-management loop.
    pub fn set_point(&mut self, frequency_mhz: Option<u16>, core_mv: Option<u16>) {
        if let Some(f) = frequency_mhz {
            self.frequency_mhz = f;
        }
        if let Some(v) = core_mv {
            self.core_mv = v;
        }
        self.dirty_point = true;
    }

    /// `POST /api/system/restart`: the firmware reboots. Counters and
    /// averages reset, the VR comes back, faults on the chip stay.
    pub fn restart(&mut self) {
        self.restarts += 1;
        self.board.restart();
        self.rx = RxDecoder::new();
        self.nonces.clear();
        self.shares_accepted = 0;
        self.shares_rejected = 0;
        self.duplicate_hw_nonces = 0;
        self.last_nonce = None;
        self.overheat_mode = false;
        self.mining_paused = false;
        self.boot_ms = self.now_ms;
        self.boot();
    }

    /// Bring the chip up: the profile's init, then the operating point.
    fn boot(&mut self) {
        let mut buf = [0u8; MAX_FRAME_LEN];
        for r in render(self.profile, 1) {
            match r {
                Rendered::Cmd {
                    group,
                    op,
                    payload,
                    len,
                } => {
                    let k = encode_cmd(&mut buf, group, op, &payload[..len as usize]).unwrap();
                    self.chain.feed(&buf[..k]);
                }
                Rendered::Delay(ms) => self.chain.advance(u64::from(ms) * 1000),
                // The emulated chain has no host rate to move.
                Rendered::CountChips | Rendered::HostBaud(_) => {}
            }
        }
        // Swallow the chip-id reply; the firmware has counted its chips.
        self.chain.drain();
        self.dirty_point = true;
        self.apply_point();
        self.last_job_ms = self.now_ms.saturating_sub(JOB_INTERVAL_MS);
    }

    /// Push the commanded point to board and chip. The board decides
    /// whether the chip actually runs (VR alive?) and at what clock.
    fn apply_point(&mut self) {
        if !self.dirty_point {
            return;
        }
        self.dirty_point = false;
        let hz = solve_mhz(&self.profile.pll, u32::from(self.frequency_mhz))
            .map_or(0, |w| w.frequency_hz(self.profile.pll.xtal_hz));
        self.board.set_point(hz, self.core_mv);
        // The chip sees the PLL write regardless; whether it is powered
        // is the board's call, applied in tick() via effective_freq_hz.
        if let Some(w) = solve_mhz(&self.profile.pll, u32::from(self.frequency_mhz)) {
            let mut buf = [0u8; 16];
            let k = encode_cmd(
                &mut buf,
                Group::All,
                Op::Write,
                &write_payload(0, reg::PLL0_PARAMETER, w.register_bytes(&self.profile.pll)),
            )
            .unwrap();
            self.chain.feed(&buf[..k]);
            let hcn = match self.profile.hcn {
                warpaxe_core::chip::Hcn::Computed { errata } => regs::hash_counting_number(
                    w.frequency_hz(self.profile.pll.xtal_hz) as u32,
                    self.profile.cores,
                    1,
                    errata,
                ),
                warpaxe_core::chip::Hcn::Fixed(v) => v,
                warpaxe_core::chip::Hcn::None => 0,
            };
            let k = encode_cmd(
                &mut buf,
                Group::All,
                Op::Write,
                &write_payload(0, reg::HASH_COUNTING_NUMBER, hcn.to_be_bytes()),
            )
            .unwrap();
            self.chain.feed(&buf[..k]);
        }
    }

    /// Whether the chip is actually clocked right now, per the board.
    fn chip_powered(&self) -> bool {
        self.board.chip_running() && !self.mining_paused
    }

    /// Advance the world by `dt_ms`.
    pub fn tick(&mut self, dt_ms: u64) {
        let end = self.now_ms + dt_ms;
        // Step in ≤1 s slices so the board's thermal lag and the job
        // cadence stay sane at any dt.
        while self.now_ms < end {
            let step = (end - self.now_ms).min(1000);
            self.step_once(step);
        }
    }

    fn step_once(&mut self, dt_ms: u64) {
        self.apply_point();
        self.board.step(dt_ms as f64 / 1000.0);

        // Overheat: the firmware parks the chip until a restart.
        if self.board.overheated() && !self.overheat_mode {
            self.overheat_mode = true;
            self.mining_paused = true;
            self.board.set_point(0, 0);
        }

        // Jobs, while the chip is powered. If the board has cut the VR,
        // the firmware still sends jobs into the void — like the real one.
        if !self.mining_paused && self.now_ms.saturating_sub(self.last_job_ms) >= JOB_INTERVAL_MS {
            self.send_job();
            self.last_job_ms = self.now_ms;
        }

        // The chip only hashes when the board powers it. Model that by
        // gating the chain's clock: with the VR down, no time "passes"
        // for the chip's nonce process.
        if self.chip_powered() {
            self.chain.advance(dt_ms * 1000);
        }
        self.now_ms += dt_ms;

        // Nonces → shares.
        let bytes = self.chain.drain();
        let mut found = Vec::new();
        self.rx.push_slice(&bytes, |f| found.push(f));
        for f in found {
            if !f.response.is_job() {
                continue;
            }
            let r = JobResult::parse(&f.response.payload);
            if self.last_nonce == Some(r.nonce) {
                self.duplicate_hw_nonces += 1;
                self.shares_rejected += 1;
                continue;
            }
            self.last_nonce = Some(r.nonce);
            // A nonce meets the ASIC difficulty; it is a pool share with
            // probability asic_diff / pool_diff. The nonce's own bits are
            // not uniform (the chip stamps its address and core into the
            // high ones), so mix them first — a 32-bit finalizer, not a
            // hash of anything, just an unbiased coin.
            let asic_diff = self.chain.chips()[0].ticket_difficulty();
            let mixed = mix32(r.nonce ^ u32::from(r.job_id) << 24);
            let submit = self.pool_difficulty <= asic_diff
                || (u64::from(mixed) % u64::from(self.pool_difficulty)) < u64::from(asic_diff);
            if submit {
                self.shares_accepted += 1;
            }
            self.nonces.push_back(self.now_ms);
        }
        let cutoff = self.now_ms.saturating_sub(HR_10M_S * 1000);
        while self.nonces.front().is_some_and(|&t| t < cutoff) {
            self.nonces.pop_front();
        }
    }

    fn send_job(&mut self) {
        let job = Job {
            job_id: self.job_ids.advance(),
            starting_nonce: 0,
            nbits: 0x1705dd01,
            ntime: (self.now_ms / 1000) as u32,
            merkle_root: [0x11; 32],
            prev_block_hash: [0x22; 32],
            version: 0x2000_0000,
        };
        let mut buf = [0u8; MAX_FRAME_LEN];
        let k = encode_job(&mut buf, &job.to_bytes()).unwrap();
        self.chain.feed(&buf[..k]);
    }

    /// Nonce-based hashrate over the last `window_s` seconds, GH/s,
    /// the way ESP-Miner derives it: nonces × difficulty × 2³² / time.
    fn hashrate_ghs(&self, window_s: u64) -> f64 {
        let uptime_ms = self.now_ms - self.boot_ms;
        let span_ms = (window_s * 1000).min(uptime_ms).max(1);
        let cutoff = self.now_ms.saturating_sub(span_ms);
        let n = self.nonces.iter().filter(|&&t| t >= cutoff).count() as f64;
        let diff = f64::from(self.chain.chips()[0].ticket_difficulty());
        n * diff * 4_294_967_296.0 / (span_ms as f64 / 1000.0) / 1e9
    }

    /// Expected (nominal) hashrate, GH/s: `frequency × smallCoreCount`.
    pub fn expected_hashrate_ghs(&self) -> f64 {
        f64::from(self.frequency_mhz) * f64::from(self.profile.small_cores) / 1000.0
    }

    /// `uptimeSeconds`.
    pub fn uptime_s(&self) -> u64 {
        (self.now_ms - self.boot_ms) / 1000
    }

    /// The `/api/system/info` document.
    pub fn system_info(&self) -> serde_json::Value {
        let b = &self.board;
        let spec = b.spec();
        let hr = self.hashrate_ghs(HR_WINDOW_S);
        let hr10 = self.hashrate_ghs(HR_10M_S);
        let rejected_reasons = if self.shares_rejected > 0 {
            serde_json::json!([{"message": "Duplicate share", "count": self.shares_rejected}])
        } else {
            serde_json::json!([])
        };
        serde_json::json!({
            "power": b.power_w(),
            "voltage": b.input_v() * 1000.0,
            "current": b.current_a() * 1000.0,
            "temp": b.chip_c(),
            "vrTemp": b.vr_c(),
            "hashRate": hr,
            "hashRate_10m": hr10,
            "expectedHashrate": self.expected_hashrate_ghs(),
            "frequency": self.frequency_mhz,
            "coreVoltage": self.core_mv,
            "coreVoltageActual": b.core_mv_actual(),
            "sharesAccepted": self.shares_accepted,
            "sharesRejected": self.shares_rejected,
            "sharesRejectedReasons": rejected_reasons,
            "duplicateHWNonces": self.duplicate_hw_nonces,
            "poolDifficulty": self.pool_difficulty,
            "uptimeSeconds": self.uptime_s(),
            "fanspeed": b.fan_pct(),
            "autofanspeed": 1,
            "temptarget": spec.fan_full_at_c,
            "asicCount": 1,
            "smallCoreCount": self.profile.small_cores,
            "ASICModel": self.profile.name,
            "boardVersion": spec.name,
            "deviceModel": "Gamma",
            "version": self.identity.version,
            "hostname": self.identity.hostname,
            "macAddr": self.identity.mac,
            "stratumUser": self.identity.stratum_user,
            "overheat_mode": self.overheat_mode,
            "power_fault": false,
            "hardware_fault": false,
            "miningPaused": self.mining_paused,
            "maxPower": spec.max_power_w,
            "nominalVoltage": spec.input_v,
            "defaultFrequency": self.profile.stock.0 / 1_000_000,
            "defaultCoreVoltage": self.profile.stock.1,
        })
    }

    /// The `/api/system/asic` document (ESP-Miner shape).
    pub fn asic_info(&self) -> serde_json::Value {
        serde_json::json!({
            "ASICModel": self.profile.name,
            "deviceModel": "Gamma",
            "asicCount": 1,
            "defaultFrequency": self.profile.stock.0 / 1_000_000,
            "defaultVoltage": self.profile.stock.1,
            "frequencyOptions": self.profile.frequency_options_mhz,
            "voltageOptions": self.profile.voltage_options_mv,
        })
    }

    /// Ground truth for tests: what really happened, not what the API says.
    pub fn sim_state(&self) -> serde_json::Value {
        let c = &self.chain.chips()[0];
        serde_json::json!({
            "now_ms": self.now_ms,
            "restarts": self.restarts,
            "chip_running": self.board.chip_running(),
            "effective_freq_hz": self.board.effective_freq_hz(),
            "chip_pll_freq_hz": c.frequency_hz(),
            "chip_true_temp_c": self.board.chip_c_true(),
            "chip_state": format!("{:?}", c.state()),
            "chip_stats": format!("{:?}", c.stats()),
            "rx_stats": format!("{:?}", self.rx.stats()),
            "wire_stats": format!("{:?}", self.chain.wire_stats()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use warpaxe_core::chip::bm1370::BM1370;

    fn gamma() -> Device {
        Device::new(&BM1370, BoardSpec::gamma(), 23.0, 1)
    }

    #[test]
    fn boots_at_stock_and_hashes() {
        let mut d = gamma();
        let info = d.system_info();
        assert_eq!(info["frequency"], 525);
        assert_eq!(info["coreVoltage"], 1150);
        assert_eq!(info["expectedHashrate"], 525.0 * 2040.0 / 1000.0);
        assert_eq!(info["ASICModel"], "BM1370");
        assert_eq!(info["boardVersion"], "602");
        d.tick(600_000);
        let info = d.system_info();
        let hr = info["hashRate_10m"].as_f64().unwrap();
        // ~0.974 nonces/s → hashrate ≈ expected within statistics.
        assert!((hr - 1071.0).abs() < 150.0, "hashRate_10m {hr}");
        assert!(info["sharesAccepted"].as_u64().unwrap() > 500);
        assert_eq!(info["sharesRejected"], 0);
        assert_eq!(info["uptimeSeconds"], 600);
        let p = info["power"].as_f64().unwrap();
        assert!((p - 17.4).abs() < 0.6, "power {p}");
        assert!(info["temp"].as_f64().unwrap() > 45.0);
    }

    #[test]
    fn patch_changes_the_point_and_the_chip_follows() {
        let mut d = gamma();
        d.set_point(Some(600), Some(1200));
        d.tick(1000);
        let info = d.system_info();
        assert_eq!(info["frequency"], 600);
        assert_eq!(info["coreVoltage"], 1200);
        assert_eq!(d.chain().chips()[0].frequency_hz(), 600_000_000);
        assert_eq!(d.board().effective_freq_hz(), 600_000_000);
        assert!(info["power"].as_f64().unwrap() > 20.0);
    }

    /// The Gaia of 2026-09-22 in API terms: after the step the power
    /// collapses, coreVoltageActual reads 0, no fault flag, the nominal
    /// expectedHashrate stays — and the nonce-based hashRate decays
    /// instead of lying.
    #[test]
    fn silent_vr_shutdown_looks_like_the_real_one() {
        let mut d = gamma();
        d.inject_board(BoardFault::SilentVrShutdown {
            above_hz: 600_000_000,
        });
        d.tick(300_000);
        let before = d.system_info();
        assert!(before["hashRate"].as_f64().unwrap() > 800.0);

        d.set_point(Some(625), Some(1200));
        d.tick(30_000);
        let after = d.system_info();
        assert_eq!(after["power"].as_f64().unwrap(), 3.0, "idle watts");
        assert_eq!(after["coreVoltageActual"], 0);
        assert_eq!(after["coreVoltage"], 1200, "set-point still as commanded");
        assert_eq!(after["overheat_mode"], false);
        assert_eq!(after["power_fault"], false);
        assert_eq!(after["miningPaused"], false);
        assert_eq!(after["expectedHashrate"], 625.0 * 2040.0 / 1000.0);
        let hr_after = after["hashRate"].as_f64().unwrap();
        assert!(
            hr_after < before["hashRate"].as_f64().unwrap(),
            "hashRate must fall"
        );
        d.tick(120_000);
        assert_eq!(
            d.system_info()["hashRate"].as_f64().unwrap(),
            0.0,
            "…to zero"
        );
        assert!(d.system_info()["temp"].as_f64().unwrap() < before["temp"].as_f64().unwrap());

        // A PATCH down does not revive it; a restart does.
        d.set_point(Some(525), Some(1150));
        d.tick(60_000);
        assert_eq!(d.system_info()["power"].as_f64().unwrap(), 3.0);
        d.restart();
        d.tick(60_000);
        assert!(d.system_info()["power"].as_f64().unwrap() > 15.0);
        assert!(d.system_info()["hashRate"].as_f64().unwrap() > 0.0);
        assert_eq!(d.system_info()["uptimeSeconds"], 60);
    }

    #[test]
    fn overheat_parks_the_chip_until_restart() {
        let mut d = gamma();
        d.inject_board(BoardFault::AmbientRise { by_k: 30.0 });
        d.set_point(Some(690), Some(1250));
        d.tick(900_000);
        let info = d.system_info();
        assert_eq!(info["overheat_mode"], true);
        assert_eq!(info["miningPaused"], true);
        assert_eq!(info["power"].as_f64().unwrap(), 3.0);
        d.restart();
        assert_eq!(d.system_info()["overheat_mode"], false);
    }

    #[test]
    fn chip_hang_shows_as_hashrate_collapse_with_power_still_drawn() {
        let mut d = gamma();
        d.inject_chip(Fault::HangAbove { hz: 600_000_000 });
        d.tick(120_000);
        d.set_point(Some(625), Some(1200));
        d.tick(300_000);
        let info = d.system_info();
        assert!(
            info["power"].as_f64().unwrap() > 20.0,
            "still drawing power"
        );
        assert_eq!(info["hashRate"].as_f64().unwrap(), 0.0, "but not hashing");
        assert!(info["expectedHashrate"].as_f64().unwrap() > 1200.0);
    }

    #[test]
    fn duplicate_nonces_are_counted_and_rejected() {
        let mut d = gamma();
        d.inject_chip(Fault::DuplicateNonce { per_mille: 500 });
        d.tick(300_000);
        let info = d.system_info();
        assert!(info["duplicateHWNonces"].as_u64().unwrap() > 50);
        assert!(info["sharesRejected"].as_u64().unwrap() > 50);
        assert_eq!(
            info["sharesRejectedReasons"][0]["message"],
            "Duplicate share"
        );
    }

    #[test]
    fn pool_difficulty_above_asic_difficulty_thins_the_shares() {
        let mut a = gamma();
        let mut b = gamma();
        b.set_pool_difficulty(1024);
        a.tick(600_000);
        b.tick(600_000);
        let sa = a.system_info()["sharesAccepted"].as_u64().unwrap();
        let sb = b.system_info()["sharesAccepted"].as_u64().unwrap();
        assert!(
            sb * 3 < sa,
            "diff 1024 vs 256: about a quarter of the shares ({sb} vs {sa})"
        );
    }

    #[test]
    fn asic_info_lists_the_profile_options() {
        let d = gamma();
        let a = d.asic_info();
        assert_eq!(a["frequencyOptions"][2], 525);
        assert_eq!(a["voltageOptions"][3], 1150);
        assert_eq!(a["defaultFrequency"], 525);
    }
}

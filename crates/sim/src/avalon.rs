//! A Canaan Avalon Q as its MM controller presents it.
//!
//! The Q is a different animal from the Bitaxes: its MM firmware owns
//! the chips, runs per-chip frequency scaling and a fan PID, and offers
//! the outside world three **work modes** and the cgminer API. Nothing
//! here simulates an A3197S; this is the controller and the board as a
//! tuner sees them — the `MM ID0:Summary` token string, `summary`,
//! `pools`, `devs`, `version`, and the one write, `ascset|0,workmode,set,n`
//! (plus `0,workmode,get`).
//!
//! What matters for a regulator, and what this models:
//!
//! - **Mode switch = hashboard re-initialisation.** After `ascset` the
//!   power supply falls to standby, `GHSspd` reads 0, and it takes one
//!   to three minutes before hashing resumes and the wattage ramps to
//!   the mode's rating. A regulator that treats that as a dead chip is
//!   wrong; one that never notices a board that *stays* dead after the
//!   re-init is also wrong. WarpTune gives the Q 180 s of settle and a
//!   300 s hold on its power-collapse guard for exactly this.
//! - **`MHS 15m` decays, it does not slide.** cgminer's 15-minute figure
//!   is an exponential average with τ = 900 s: fifteen minutes after a
//!   mode switch it still carries 37 % of the old rate. A regulator that
//!   judges a step on it has to know that.
//! - **No core-voltage readback.** The dead-chip guard's voltage leg is
//!   blind here; only the hashrate leg and the power guard can see a
//!   dead board.
//! - **`STATE[1]` no matter what.** The MM does not flag a dead hashboard
//!   in the summary token a tuner reads.
//! - **Hot by design.** `TarT` is 65/80/85 °C by mode; in Super the fan
//!   is flat out and the chips sit at 91–92 °C. The MM protects itself
//!   around 100 °C.
//!
//! Mode numbers are the operator's measurements (2026-09-11, MM
//! 25052801, 23 °C room) as recorded in WarpTune's `avalon.rs`; the
//! document shapes are from a capture of the same device
//! (2026-09-23, `avalon-q_cgminer_capture_2026-09-23.json`).

/// One work mode's ratings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeSpec {
    /// `WORKMODE` index.
    pub index: u8,
    /// Name.
    pub name: &'static str,
    /// Nominal chip frequency the MM reports as `Freq`, MHz.
    pub nominal_mhz: f64,
    /// Rated draw, W (`PS[… watts]` once settled).
    pub rated_w: f64,
    /// Rated hashrate, GH/s.
    pub rated_ghs: f64,
    /// MM fan target, °C (`TarT`).
    pub tart_c: f64,
    /// Settled chip average, °C (`TAvg`) at 23 °C ambient.
    pub tavg_c: f64,
    /// Fan duty once settled, %.
    pub fan_pct: f64,
    /// `MPO`, the mode's power cap the MM reports, W.
    pub mpo_w: f64,
}

/// The three modes, measured on the operator's device.
pub const MODES: [ModeSpec; 3] = [
    ModeSpec {
        index: 0,
        name: "Eco",
        nominal_mhz: 290.0,
        rated_w: 840.0,
        rated_ghs: 54_000.0,
        tart_c: 65.0,
        tavg_c: 65.0,
        fan_pct: 55.0,
        mpo_w: 800.0,
    },
    ModeSpec {
        index: 1,
        name: "Standard",
        nominal_mhz: 422.0,
        rated_w: 1365.0,
        rated_ghs: 79_400.0,
        tart_c: 80.0,
        tavg_c: 80.0,
        fan_pct: 87.0,
        mpo_w: 1250.0,
    },
    ModeSpec {
        index: 2,
        name: "Super",
        nominal_mhz: 493.0,
        rated_w: 1690.0,
        rated_ghs: 92_000.0,
        tart_c: 85.0,
        tavg_c: 92.0,
        fan_pct: 100.0,
        mpo_w: 1600.0,
    },
];

/// Standby draw of the PSU with the hashboard down, W.
pub const STANDBY_W: f64 = 55.0;
/// Pool share difficulty the operator's Q mines at.
pub const POOL_DIFF: f64 = 181_717.0;

/// Board-level misbehaviour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AvalonFault {
    /// After the re-init that follows a switch *into* this mode, the
    /// hashboard never comes back: power stays at standby, `GHSspd` 0,
    /// `STATE[1]` unchanged. The Q's version of the silent VR death.
    CollapseInMode {
        /// `WORKMODE` index the fault is armed for.
        mode: u8,
    },
    /// `ascset` is refused (`STATUS: E`). The mode does not change.
    RefuseWorkmode,
    /// Room gets warmer by this much.
    AmbientRise {
        /// Kelvin.
        by_k: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Hashing in the current mode.
    Working,
    /// Hashboard re-initialising after a mode switch: standby power, no
    /// hashrate, until `until_s`; then a ramp to `ramp_until_s`.
    Reinit { until_s: u64, ramp_until_s: u64 },
    /// Hashboard dead after a re-init (fault). Stays until the next
    /// mode switch re-initialises it again.
    Dead,
}

/// The simulated Q.
#[derive(Debug, Clone)]
pub struct AvalonQ {
    ambient_c: f64,
    mode: u8,
    phase: Phase,
    /// Seconds since the MM booted (`Elapsed`).
    elapsed_s: u64,
    /// Seconds the hashboard needs to re-initialise after a switch.
    pub reinit_s: u64,
    /// Seconds the wattage and hashrate take to ramp after re-init.
    pub ramp_s: u64,
    tavg_c: f64,
    ghs_spd: f64,
    /// cgminer's `MHS 15m`: an exponentially decaying average of the
    /// spot rate with τ = 900 s — not a hard window. Fifteen minutes
    /// after a switch it still carries e⁻¹ ≈ 37 % of the old rate.
    ema_15m: f64,
    /// Hashes since boot, for `GHSavg`.
    total_gh: f64,
    accepted: u64,
    rejected: u64,
    stale: u64,
    share_acc: f64,
    faults: Vec<AvalonFault>,
    noise: u32,
    /// `ascset` calls seen, for tests.
    pub sets: Vec<u8>,
}

impl AvalonQ {
    /// A Q that has been running in `mode` long enough to be settled.
    pub fn settled(mode: u8, ambient_c: f64, seed: u32) -> Self {
        let m = MODES[usize::from(mode.min(2))];
        AvalonQ {
            ambient_c,
            mode: m.index,
            phase: Phase::Working,
            elapsed_s: 3600,
            reinit_s: 120,
            ramp_s: 60,
            tavg_c: m.tavg_c + (ambient_c - 23.0),
            ghs_spd: m.rated_ghs,
            ema_15m: m.rated_ghs,
            total_gh: m.rated_ghs * 3600.0,
            accepted: 0,
            rejected: 0,
            stale: 0,
            share_acc: 0.0,
            faults: Vec::new(),
            noise: seed.max(1),
            sets: Vec::new(),
        }
    }

    /// Current mode's ratings.
    pub fn mode(&self) -> ModeSpec {
        MODES[usize::from(self.mode)]
    }

    /// Is the hashboard hashing?
    pub fn hashing(&self) -> bool {
        self.phase == Phase::Working
    }

    /// Is the hashboard dead (fault)?
    pub fn dead(&self) -> bool {
        self.phase == Phase::Dead
    }

    /// Re-initialising?
    pub fn reinitialising(&self) -> bool {
        matches!(self.phase, Phase::Reinit { .. })
    }

    /// Inject a fault.
    pub fn inject(&mut self, f: AvalonFault) {
        if let AvalonFault::AmbientRise { by_k } = f {
            self.ambient_c += by_k;
        }
        self.faults.push(f);
    }

    /// `Elapsed`.
    pub fn elapsed_s(&self) -> u64 {
        self.elapsed_s
    }

    /// Chip average, °C.
    pub fn tavg_c(&self) -> f64 {
        self.tavg_c
    }

    /// Headline wattage the PSU reports.
    pub fn power_w(&self) -> f64 {
        match self.phase {
            Phase::Working => self.mode().rated_w,
            Phase::Dead => STANDBY_W,
            Phase::Reinit {
                until_s,
                ramp_until_s,
            } => {
                if self.elapsed_s < until_s {
                    STANDBY_W
                } else {
                    let f =
                        (self.elapsed_s - until_s) as f64 / (ramp_until_s - until_s).max(1) as f64;
                    STANDBY_W + (self.mode().rated_w - STANDBY_W) * f.min(1.0)
                }
            }
        }
    }

    /// `GHSspd`, the noisy spot rate.
    pub fn ghs_spd(&self) -> f64 {
        self.ghs_spd
    }

    /// `MHS 15m` in GH/s: cgminer's decaying average, τ = 900 s.
    pub fn ghs_15m(&self) -> f64 {
        self.ema_15m
    }

    /// Fan duty, %.
    pub fn fan_pct(&self) -> f64 {
        match self.phase {
            Phase::Working => self.mode().fan_pct,
            _ => 30.0,
        }
    }

    /// `ascset|0,workmode,set,n`. Returns whether the MM accepted it.
    pub fn set_workmode(&mut self, n: u8) -> bool {
        self.sets.push(n);
        if self.faults.contains(&AvalonFault::RefuseWorkmode) || n > 2 {
            return false;
        }
        if n == self.mode && self.phase == Phase::Working {
            return true;
        }
        self.mode = n;
        self.phase = Phase::Reinit {
            until_s: self.elapsed_s + self.reinit_s,
            ramp_until_s: self.elapsed_s + self.reinit_s + self.ramp_s,
        };
        self.ghs_spd = 0.0;
        true
    }

    fn noise(&mut self) -> f64 {
        // LCG, ±1 uniform-ish; enough for a jittery GHSspd.
        self.noise = self
            .noise
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        (f64::from(self.noise >> 8) / 16_777_216.0) * 2.0 - 1.0
    }

    /// Advance `dt_s` seconds.
    pub fn tick(&mut self, dt_s: u64) {
        let end = self.elapsed_s + dt_s;
        while self.elapsed_s < end {
            let step = (end - self.elapsed_s).min(5);
            self.step(step);
        }
    }

    fn step(&mut self, dt_s: u64) {
        self.elapsed_s += dt_s;
        // Phase transitions.
        if let Phase::Reinit {
            until_s,
            ramp_until_s,
        } = self.phase
        {
            if self.elapsed_s >= until_s {
                let collapses = self.faults.iter().any(
                    |f| matches!(f, AvalonFault::CollapseInMode { mode } if *mode == self.mode),
                );
                if collapses {
                    self.phase = Phase::Dead;
                } else if self.elapsed_s >= ramp_until_s {
                    self.phase = Phase::Working;
                }
            }
        }
        // Hashrate.
        let m = self.mode();
        let target_ghs = match self.phase {
            Phase::Working => m.rated_ghs,
            Phase::Dead => 0.0,
            Phase::Reinit {
                until_s,
                ramp_until_s,
            } => {
                if self.elapsed_s < until_s {
                    0.0
                } else {
                    let f =
                        (self.elapsed_s - until_s) as f64 / (ramp_until_s - until_s).max(1) as f64;
                    m.rated_ghs * f.min(1.0)
                }
            }
        };
        let n = self.noise();
        self.ghs_spd = if target_ghs > 0.0 {
            (target_ghs * (1.0 + 0.08 * n)).max(0.0)
        } else {
            0.0
        };
        let a15 = 1.0 - (-(dt_s as f64) / 900.0).exp();
        self.ema_15m += (self.ghs_spd - self.ema_15m) * a15;
        self.total_gh += self.ghs_spd * dt_s as f64;
        // Shares at the pool difficulty.
        self.share_acc += self.ghs_spd * 1e9 / (POOL_DIFF * 4_294_967_296.0) * dt_s as f64;
        while self.share_acc >= 1.0 {
            self.share_acc -= 1.0;
            self.accepted += 1;
            if self.accepted.is_multiple_of(730) {
                self.rejected += 1;
            }
            if self.accepted.is_multiple_of(2900) {
                self.stale += 1;
            }
        }
        // Temperature: first-order lag towards the mode's settled point
        // (scaled by ambient) while working, towards ambient+15 otherwise.
        let target_c = if self.phase == Phase::Working {
            m.tavg_c + (self.ambient_c - 23.0)
        } else {
            self.ambient_c + 15.0
        };
        let a = 1.0 - (-(dt_s as f64) / 120.0).exp();
        self.tavg_c += (target_c - self.tavg_c) * a;
    }

    /// `GHSavg`: since boot.
    pub fn ghs_avg(&self) -> f64 {
        self.total_gh / self.elapsed_s.max(1) as f64
    }

    /// The `MM ID0:Summary` token string.
    pub fn mm_summary(&self) -> String {
        let m = self.mode();
        let tmax = self.tavg_c
            + if self.phase == Phase::Working {
                6.0 + 12.0 * (m.index as f64 / 2.0)
            } else {
                2.0
            };
        let freq = if self.hashing() {
            m.nominal_mhz - 0.46
        } else {
            0.0
        };
        let vin_x100 = 1209u32;
        let watts = self.power_w().round() as u32;
        format!(
            "'STATS':{{Ver[Q-25052801_14a19a2] LVer[25052801_14a19a2] BVer[25052801_14a19a2] \
             HashMcu0Ver[Q_hb_v1.1] FanMcuVer[Q_fb_v1.2] CPU[K230] FW[Release] DNA[0201000054ea1a94] \
             STATE[1] MEMFREE[67468] NETFAIL[] SSID[] RSSI[0] NetDevType[0] \
             SYSTEMSTATU[Work: In Work, Hash Board: 1] Elapsed[{elapsed}] BOOTBY[0x01.00000000] \
             LW[0] MH[2] DHW[0] HW[2] DH[3.330%] ITemp[{itemp}] HBITemp[{hbi}] HBOTemp[{hbo}] \
             TMax[{tmax}] TAvg[{tavg}] TarT[{tart}] Fan1[1575] Fan2[1576] Fan3[1590] Fan4[1586] \
             FanR[{fan}%] SoftOffTime[0] SoftOnTime[0] Filter[150865] FanErr[0] SoloAllowed[1] \
             PS[0 {vin} 2299 34 799 2300 {watts}] PCOMM_E[0] GHSspd[{spd:.2}] DHspd[3.330%] \
             GHSmm[{spd:.2}] GHSavg[{avg:.2}] WU[789735.56] Freq[{freq:.2}] MGHS[{avg:.2}] TA[160] \
             Core[A3197S] BIN[47] PING[2] SoftOFF[0] ECHU[0] ECMM[256] PLL0[13219 6028 4107 966] \
             SF0[276 294 315 336] CRC[0] COMCRC[0] ATA0[800-65-2317-276-20] LcdOnoff[1] \
             Activation[1] WORKMODE[{mode}] WORKLEVEL[0] MPO[{mpo}] CALIALL[7] ADJ[1] Nonce Mask[25]}}",
            elapsed = self.elapsed_s,
            itemp = (self.ambient_c + 10.0).round(),
            hbi = (self.tavg_c - 4.0).round(),
            hbo = (self.tavg_c - 2.0).round(),
            tmax = tmax.round(),
            tavg = self.tavg_c.round(),
            tart = m.tart_c,
            fan = self.fan_pct().round(),
            vin = vin_x100,
            watts = watts,
            spd = self.ghs_spd,
            avg = self.ghs_avg(),
            freq = freq,
            mode = m.index,
            mpo = m.mpo_w,
        )
    }

    fn status(code: &str, msg: &str, n: u32) -> serde_json::Value {
        serde_json::json!([{
            "STATUS": code, "When": 1_790_146_669u64, "Code": n,
            "Msg": msg, "Description": "cgminer 4.11.1"
        }])
    }

    /// The `estats` reply.
    pub fn estats(&self) -> serde_json::Value {
        serde_json::json!({
            "STATUS": Self::status("S", "CGMiner stats", 70),
            "STATS": [{
                "STATS": 0, "ID": "AVALON0", "Elapsed": self.elapsed_s,
                "Calls": 0, "Wait": 0.0, "Max": 0.0, "Min": 99999999.0,
                "MM ID0:Summary": self.mm_summary(),
            }],
            "id": 1
        })
    }

    /// The `summary` reply.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "STATUS": Self::status("S", "Summary", 11),
            "SUMMARY": [{
                "Elapsed": self.elapsed_s,
                "MHS av": self.ghs_avg() * 1000.0,
                "MHS 5s": self.ghs_spd * 1000.0,
                "MHS 1m": self.ghs_spd * 1000.0,
                "MHS 5m": self.ghs_15m() * 1000.0,
                "MHS 15m": self.ghs_15m() * 1000.0,
                "Found Blocks": 0,
                "Accepted": self.accepted,
                "Rejected": self.rejected,
                "Hardware Errors": 2,
                "Stale": self.stale,
                "Difficulty Accepted": self.accepted as f64 * POOL_DIFF,
                "Difficulty Rejected": self.rejected as f64 * POOL_DIFF,
                "Pool Rejected%": if self.accepted > 0 { self.rejected as f64 * 100.0 / (self.accepted + self.rejected) as f64 } else { 0.0 },
            }],
            "id": 1
        })
    }

    /// The `pools` reply.
    pub fn pools(&self) -> serde_json::Value {
        let user = "bc1qsimulator.AvalonQ";
        serde_json::json!({
            "STATUS": Self::status("S", "2 Pool(s)", 7),
            "POOLS": [
                {"POOL": 0, "URL": "stratum+tcp://127.0.0.1:3333", "Status": "Alive", "Priority": 0,
                 "Accepted": self.accepted, "Rejected": self.rejected, "Stale": self.stale,
                 "User": user, "Stratum Active": true, "Stratum Difficulty": POOL_DIFF},
                {"POOL": 1, "URL": "stratum+tcp://127.0.0.1:3333", "Status": "Alive", "Priority": 1,
                 "Accepted": 0, "Rejected": 0, "Stale": 0, "User": user, "Stratum Active": false,
                 "Stratum Difficulty": 0.0}
            ],
            "id": 1
        })
    }

    /// The `devs` reply.
    pub fn devs(&self) -> serde_json::Value {
        serde_json::json!({
            "STATUS": Self::status("S", "1 ASC(s)", 9),
            "DEVS": [{
                "ASC": 0, "Name": "AVALON", "ID": 0, "Enabled": "Y", "Status": "Alive",
                "Temperature": 0.0, "MHS av": self.ghs_avg() * 1000.0,
                "MHS 15m": self.ghs_15m() * 1000.0, "Accepted": self.accepted,
                "Rejected": self.rejected, "Hardware Errors": 2, "Device Elapsed": self.elapsed_s,
            }],
            "id": 1
        })
    }

    /// The `version` reply.
    pub fn version(&self) -> serde_json::Value {
        serde_json::json!({
            "STATUS": Self::status("S", "CGMiner versions", 22),
            "VERSION": [{
                "CGMiner": "4.11.1", "API": "3.7", "PROD": "Avalon Q", "MODEL": "Q",
                "HWTYPE": "Q_MM1v1_X1", "SWTYPE": "MM319", "LVERSION": "25052801_14a19a2",
                "BVERSION": "25052801_14a19a2", "CGVERSION": "25052801_14a19a2",
                "HBMCUVERSION": "Q_hb_v1.1", "FANMCUVERSION": "Q_fb_v1.2",
                "DNA": "0201000054ea1a94", "MAC": "00e04c1615ac"
            }],
            "id": 1
        })
    }

    /// `ascset` with its parameter, as the real MM 25052801 answers it
    /// (measured 2026-09-26): `workmode` takes a verb, `set,<n>` or `get`;
    /// the bare `0,workmode,<n>` is refused with code 120.
    pub fn ascset(&mut self, parameter: &str) -> serde_json::Value {
        let parts: Vec<&str> = parameter.split(',').map(str::trim).collect();
        let err = |msg: &str| serde_json::json!({"STATUS": Self::status("E", &format!("ASC 0 set failed: {msg}"), 120), "id": 1});
        match parts.as_slice() {
            ["0", "workmode"] => err("missing workmode setting"),
            ["0", "workmode", "get"] => serde_json::json!({
                "STATUS": Self::status("I", &format!("ASC 0 set info: workmode {}", self.mode().index), 118),
                "id": 1
            }),
            ["0", "workmode", "set", n] => {
                if n.parse::<u8>().is_ok_and(|n| self.set_workmode(n)) {
                    serde_json::json!({"STATUS": Self::status("S", "ASC 0 set OK", 119), "id": 1})
                } else {
                    err("invalid or refused")
                }
            }
            ["0", "workmode", ..] => err("workmode unknown argument"),
            _ => err("invalid or refused"),
        }
    }

    /// Dispatch a cgminer command.
    pub fn command(&mut self, command: &str, parameter: Option<&str>) -> serde_json::Value {
        match command {
            "estats" | "stats" => self.estats(),
            "summary" => self.summary(),
            "pools" => self.pools(),
            "devs" => self.devs(),
            "version" => self.version(),
            "ascset" => self.ascset(parameter.unwrap_or("")),
            other => serde_json::json!({
                "STATUS": Self::status("E", &format!("Invalid command: {other}"), 14), "id": 1
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settled_q_reports_its_mode_ratings() {
        let q = AvalonQ::settled(0, 23.0, 1);
        assert_eq!(q.power_w(), 840.0);
        assert!((q.ghs_15m() - 54_000.0).abs() < 1.0);
        assert_eq!(q.fan_pct(), 55.0);
        let s = q.mm_summary();
        assert!(
            s.contains("WORKMODE[0]")
                && s.contains("TarT[65]")
                && s.contains("PS[0 1209 2299 34 799 2300 840]"),
            "{s}"
        );
        assert!(s.contains("STATE[1]"));
    }

    #[test]
    fn a_mode_switch_reinitialises_then_ramps() {
        let mut q = AvalonQ::settled(1, 23.0, 1);
        assert!(q.set_workmode(2));
        assert!(q.reinitialising());
        assert_eq!(q.power_w(), STANDBY_W);
        assert_eq!(q.ghs_spd(), 0.0);
        q.tick(60);
        assert_eq!(q.power_w(), STANDBY_W, "still re-initialising at 60 s");
        q.tick(90); // 150 s: ramping
        assert!(
            q.power_w() > STANDBY_W && q.power_w() < 1690.0,
            "{}",
            q.power_w()
        );
        q.tick(60); // 210 s: working
        assert!(q.hashing());
        assert_eq!(q.power_w(), 1690.0);
        assert!(q.ghs_spd() > 80_000.0);
        assert!(q.mm_summary().contains("WORKMODE[2]"));
    }

    #[test]
    fn collapse_in_mode_keeps_state_1_and_standby_watts() {
        let mut q = AvalonQ::settled(1, 23.0, 1);
        q.inject(AvalonFault::CollapseInMode { mode: 2 });
        q.set_workmode(2);
        q.tick(400);
        assert!(q.dead());
        assert_eq!(q.power_w(), STANDBY_W);
        assert_eq!(q.ghs_spd(), 0.0);
        assert!(
            q.mm_summary().contains("STATE[1]"),
            "the MM does not flag it"
        );
        assert!(q.mm_summary().contains("WORKMODE[2]"));
        // Back to Standard re-initialises and revives.
        q.set_workmode(1);
        q.tick(300);
        assert!(q.hashing());
        assert_eq!(q.power_w(), 1365.0);
    }

    #[test]
    fn refuse_workmode_answers_e_and_changes_nothing() {
        let mut q = AvalonQ::settled(0, 23.0, 1);
        q.inject(AvalonFault::RefuseWorkmode);
        let r = q.ascset("0,workmode,set,2");
        assert_eq!(r["STATUS"][0]["STATUS"], "E");
        assert_eq!(q.mode().index, 0);
        assert!(q.hashing());
    }

    #[test]
    fn workmode_takes_a_verb_like_the_real_mm() {
        let mut q = AvalonQ::settled(0, 23.0, 1);
        let code = |v: &serde_json::Value| {
            (
                v["STATUS"][0]["STATUS"].as_str().unwrap().to_string(),
                v["STATUS"][0]["Code"].as_u64().unwrap(),
                v["STATUS"][0]["Msg"].as_str().unwrap().to_string(),
            )
        };
        assert_eq!(
            code(&q.ascset("0,workmode,1")),
            (
                "E".into(),
                120,
                "ASC 0 set failed: workmode unknown argument".into()
            )
        );
        assert_eq!(
            code(&q.ascset("0,workmode,help")),
            (
                "E".into(),
                120,
                "ASC 0 set failed: workmode unknown argument".into()
            )
        );
        assert_eq!(code(&q.ascset("0,workmode")).0, "E");
        assert_eq!(q.mode().index, 0, "bare forms change nothing");
        assert_eq!(
            code(&q.ascset("0,workmode,get")),
            ("I".into(), 118, "ASC 0 set info: workmode 0".into())
        );
        assert_eq!(
            code(&q.ascset("0,workmode,set,1")),
            ("S".into(), 119, "ASC 0 set OK".into())
        );
        assert_eq!(q.mode().index, 1);
    }

    #[test]
    fn fifteen_minute_average_decays_after_a_switch() {
        let mut q = AvalonQ::settled(2, 23.0, 3);
        assert!(q.ghs_15m() > 90_000.0);
        q.set_workmode(0);
        q.tick(200);
        assert!(
            q.ghs_15m() > 70_000.0,
            "still mostly Super: {}",
            q.ghs_15m()
        );
        q.tick(880); // 1080 s after the switch: e^-1 of the old rate left
        let g = q.ghs_15m();
        assert!(
            g > 60_000.0 && g < 72_000.0,
            "37 % of Super still in it: {g}"
        );
        q.tick(3000);
        assert!(
            (q.ghs_15m() - 54_000.0).abs() < 2_000.0,
            "settled on Eco: {}",
            q.ghs_15m()
        );
    }

    #[test]
    fn shares_accrue_at_the_pool_difficulty() {
        let mut q = AvalonQ::settled(2, 23.0, 1);
        q.tick(600);
        // 92 TH/s at diff 181717: 0.118 shares/s → ~71 in 600 s.
        let a = q.summary()["SUMMARY"][0]["Accepted"].as_u64().unwrap();
        assert!((60..=85).contains(&a), "{a}");
    }
}

//! The Avalon Q path — the one WarpTune has never seen fail live.
//!
//! Real Avalon limits throughout (settle 180 s, judge 900 s, write
//! verification 12 ticks, 300 s hold on the power-collapse guard,
//! failsafe 100 °C): virtual time makes them cheap, and shortening them
//! would hide exactly the interactions these scenarios are about — a
//! re-init that looks like a dead board for two minutes, a 15-minute
//! judging window that straddles it, a dead-chip guard that is half
//! blind because the Q reports no core voltage.

use warpaxe_sim::{AvalonFault, AvalonQ};
use warppool_tune::avalon::{mode_for_mhz, parse_estats, AVALON_Q_FAILSAFE_C, AVALON_Q_MODES};
use warppool_tune::config::VoltageAnchor;
use warppool_tune::regulator::Point;
use warppool_tune::{Action, Decision, Ladder, Limits, MinerSettings, Mode, Regulator, Sample};

/// One applied decision.
#[derive(Debug, Clone)]
struct Applied {
    at_s: u64,
    decision: Decision,
}

/// The Q and the regulator on one virtual clock.
struct AvalonBench {
    q: AvalonQ,
    reg: Regulator,
    now_s: u64,
    applied: Vec<Applied>,
    writes: Vec<(u64, u8)>,
    restart_requests: u32,
}

/// What `runner.rs` sets for `kind = "avalon"`.
fn avalon_limits() -> Limits {
    let mut l = Limits {
        asic_failsafe_c: AVALON_Q_FAILSAFE_C,
        ..Limits::default()
    };
    l.write_verify_ticks = 12;
    l.settle_s = l.settle_s.max(180);
    l.judge_after_s = l.judge_after_s.max(900);
    l.power_collapse_hold_s = l.power_collapse_hold_s.max(300);
    l
}

/// The operator's Avalon settings from `warptune.toml`.
fn avalon_settings() -> MinerSettings {
    MinerSettings {
        mode: Mode::Max,
        target_temp_c: 96.0,
        max_watts: 1780.0,
        voltage_offset_mv: 0,
        voltage_anchor: VoltageAnchor::default(),
        beyond_spec: false,
        ladder_cap_mhz: None,
        thermal_r_c_per_w: Some(0.04),
        eco_cap_mhz: None,
        voltage_trial: false,
        voltage_min_mv: None,
        voltage_max_mv: None,
    }
}

impl AvalonBench {
    /// A Q settled in `mode`, regulator built like the runner builds it.
    fn new(mode: u8, seed: u32) -> Self {
        let q = AvalonQ::settled(mode, 23.0, seed);
        let points: Vec<Point> = AVALON_Q_MODES
            .iter()
            .map(|w| (w.nominal_mhz, w.pseudo_mv))
            .collect();
        let top = AVALON_Q_MODES[AVALON_Q_MODES.len() - 1];
        let ladder = Ladder::from_modes("A3197S", &points, (top.nominal_mhz, top.pseudo_mv), None)
            .expect("mode ladder");
        let m = AVALON_Q_MODES[usize::from(mode)];
        let stock = (m.nominal_mhz, m.pseudo_mv);
        let reg = Regulator::new(ladder, avalon_settings(), avalon_limits(), Some(stock), 30);
        AvalonBench {
            q,
            reg,
            now_s: 0,
            applied: Vec::new(),
            writes: Vec::new(),
            restart_requests: 0,
        }
    }

    /// Build the sample the sidecar would: `parse_estats` is WarpTune's
    /// own parser; the summary and pool fields are filled the way its
    /// private `apply_summary` / `stratum_user` do (the TCP test below
    /// exercises those for real).
    fn sample(&self) -> Sample {
        let mut st = parse_estats(&self.q.estats()).expect("MM ID0:Summary present");
        let s = self.q.summary();
        let s0 = &s["SUMMARY"][0];
        st.accepted = s0["Accepted"].as_u64();
        st.rejected = s0["Rejected"].as_u64();
        st.stale = s0["Stale"].as_u64();
        st.ghs_15m = s0["MHS 15m"].as_f64().map(|m| m / 1000.0);
        st.stratum_user = Some("bc1qsimulator.AvalonQ".into());
        Sample::from_info(&st.to_system_info(), self.now_s).expect("sample")
    }

    /// Run `secs` of virtual time at the operator's 10 s poll / 30 s decide.
    fn run_for(&mut self, secs: u64) {
        let end = self.now_s + secs;
        while self.now_s < end {
            self.q.tick(10);
            self.now_s += 10;
            let s = self.sample();
            if let Some(d) = self.reg.on_sample(&s) {
                self.apply(d);
            }
        }
    }

    fn apply(&mut self, d: Decision) {
        if d.action.changes_operating_point() {
            let m = mode_for_mhz(d.to.0);
            self.q.set_workmode(m.index);
            self.writes.push((self.now_s, m.index));
            self.reg.note_applied(self.now_s);
        }
        if d.restart {
            // runner.rs: "restart not supported on this device".
            self.restart_requests += 1;
        }
        if d.action != Action::Hold || d.restart {
            self.applied.push(Applied {
                at_s: self.now_s,
                decision: d,
            });
        }
    }

    fn first_reason(&self, needle: &str) -> Option<&Applied> {
        self.applied
            .iter()
            .find(|a| a.decision.reason.contains(needle))
    }

    fn of(&self, action: Action) -> Vec<&Applied> {
        self.applied
            .iter()
            .filter(|a| a.decision.action == action)
            .collect()
    }

    fn transcript(&self) -> String {
        self.applied
            .iter()
            .map(|a| {
                let d = &a.decision;
                format!(
                    "{:>5}s {:<8} {}→{} {}{}\n",
                    a.at_s,
                    d.action.as_str(),
                    d.from.0,
                    d.to.0,
                    if d.restart { "[restart] " } else { "" },
                    d.reason.chars().take(120).collect::<String>()
                )
            })
            .collect()
    }
}

/// Standard → Super on a healthy Q: two minutes of re-init at standby
/// power and zero hashrate, then a ramp. The regulator must neither call
/// that a dead chip nor a power collapse, and must not judge the step
/// bad on a 15-minute window that still contains the re-init.
#[test]
#[ignore = "FINDING A (2026-09-23): the 15-minute judging window contains the ~180 s re-init, so a healthy Standard→Super switch reads 86 % at judge time and is stepped down (urgent), then again Standard→Eco: a cascade. Un-ignore when WarpTune judges the Avalon on a window that starts after settle (e.g. judge_after_s = settle_s + 900)."]
fn a_healthy_mode_switch_is_neither_a_dead_chip_nor_a_collapse() {
    let mut b = AvalonBench::new(1, 1);
    b.reg.seed_known_good(&[493]);
    b.run_for(2400);
    let t = b.transcript();
    let up = b
        .of(Action::Up)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no step to Super:\n{t}"));
    assert_eq!(up.decision.to.0, 493, "{t}");
    assert!(
        b.first_reason("chip not hashing").is_none(),
        "re-init mistaken for a dead chip:\n{t}"
    );
    assert!(
        b.first_reason("power collapse").is_none(),
        "re-init mistaken for a collapse:\n{t}"
    );
    assert!(
        b.of(Action::Down).is_empty(),
        "healthy switch judged bad:\n{t}"
    );
    assert_eq!(b.restart_requests, 0);
    assert!(
        b.q.hashing() && b.q.mode().index == 2,
        "ends in Super, hashing: {t}"
    );
    assert_eq!(b.writes.len(), 1, "exactly one ascset:\n{t}");
}

/// The Q's silent death: the hashboard never comes back from the
/// re-init into Super. STATE stays 1, no core-voltage readback exists.
/// The regulator has only the power guard (with its 300 s hold) and the
/// hashrate leg of the dead-chip guard. It must wait out the re-init
/// window, then bring the board back to Standard, and never try Super
/// again.
#[test]
#[ignore = "FINDING B (2026-09-23): recovery itself works (dead-chip hashrate leg at +270 s, restore to Standard), but the re-init after the restore triggers FINDING A and the Q ends in Eco. Un-ignore with A."]
fn a_collapse_after_reinit_is_recovered_without_retrying_super() {
    let mut b = AvalonBench::new(1, 5);
    b.reg.seed_known_good(&[493]);
    b.q.inject(AvalonFault::CollapseInMode { mode: 2 });
    b.run_for(3600);
    let t = b.transcript();

    let up = b
        .of(Action::Up)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no step to Super:\n{t}"));
    assert_eq!(up.decision.to.0, 493);

    // Nothing urgent before the re-init could possibly have finished.
    let premature: Vec<&Applied> = b
        .applied
        .iter()
        .filter(|a| a.at_s > up.at_s && a.at_s < up.at_s + 180 && a.decision.urgent)
        .collect();
    assert!(
        premature.is_empty(),
        "acted during the re-init window: {premature:?}\n{t}"
    );

    // Recovery: a decision back to Standard, by either guard.
    let recovery = b
        .applied
        .iter()
        .find(|a| a.at_s > up.at_s && a.decision.to.0 == 422 && a.decision.action != Action::Up)
        .unwrap_or_else(|| panic!("never came back to Standard:\n{t}"));
    let by_power = recovery.decision.reason.contains("power collapse");
    let by_hash = recovery.decision.reason.contains("chip not hashing");
    assert!(
        by_power || by_hash,
        "recovery by an unexpected path: {}\n{t}",
        recovery.decision.reason
    );
    assert!(
        recovery.at_s - up.at_s <= 700,
        "recovery took {} s after the step:\n{t}",
        recovery.at_s - up.at_s
    );

    // The board is alive again in Standard, and Super is a ceiling.
    assert!(
        b.q.hashing() && b.q.mode().index == 1,
        "alive in Standard:\n{t}"
    );
    assert!(
        b.reg.bad_steps(b.now_s).contains(&493),
        "Super marked unstable: {:?}",
        b.reg.bad_steps(b.now_s)
    );
    let retries = b
        .of(Action::Up)
        .iter()
        .filter(|a| a.at_s > recovery.at_s)
        .count();
    assert_eq!(retries, 0, "retried Super after the collapse:\n{t}");
    let supers = b.writes.iter().filter(|(_, m)| *m == 2).count();
    assert_eq!(supers, 1, "ascset to Super exactly once:\n{t}");
}

/// An `ascset` the MM refuses: the regulator's write verification must
/// notice the mode did not change, and must not keep hammering.
#[test]
#[ignore = "FINDING C (2026-09-23): a refused ascset is retried every dwell cycle (240 s) indefinitely — 10 writes in 40 min, no back-off, the step is never marked. Un-ignore when WarpTune backs off or marks a step whose write does not verify."]
fn a_refused_workmode_is_noticed_by_write_verification() {
    let mut b = AvalonBench::new(1, 2);
    b.reg.seed_known_good(&[493]);
    b.q.inject(AvalonFault::RefuseWorkmode);
    b.run_for(2400);
    let t = b.transcript();
    assert!(!b.writes.is_empty(), "it tried:\n{t}");
    assert!(b.q.mode().index == 1 && b.q.hashing(), "device unchanged");
    assert!(
        b.writes.len() <= 4,
        "kept hammering ascset: {} writes\n{t}",
        b.writes.len()
    );
}

/// The wire, for real: WarpTune's own `AvalonClient` against the
/// cgminer server — three round trips for `stats`, one for `ascset`.
#[test]
fn warptune_avalon_client_reads_and_writes_the_emulated_q_over_tcp() {
    use std::sync::{Arc, Mutex};
    let dev = Arc::new(Mutex::new(AvalonQ::settled(0, 23.0, 9)));
    let addr = warpaxe_cgminer::spawn("127.0.0.1:0", Arc::clone(&dev)).unwrap();
    let host = addr.to_string();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = warppool_tune::avalon::AvalonClient::new();

    let st = rt.block_on(client.stats(&host)).expect("stats over tcp");
    assert_eq!(st.workmode, Some(0));
    assert_eq!(st.mode().unwrap().name, "Eco");
    assert_eq!(st.power_w, Some(840.0));
    assert_eq!(st.tavg_c, Some(65.0));
    assert!((st.ghs_15m.unwrap() - 54_000.0).abs() < 1.0);
    assert!(st.state_working);
    assert_eq!(st.dna.as_deref(), Some("0201000054ea1a94"));
    assert_eq!(st.stratum_user.as_deref(), Some("bc1qsimulator.AvalonQ"));
    let info = st.to_system_info();
    assert_eq!(info.frequency, Some(290.0));
    assert_eq!(
        info.device_identity().as_deref(),
        Some("serial:0201000054ea1a94")
    );
    Sample::from_info(&info, 0).expect("a regulator sample from the wire");

    rt.block_on(client.set_workmode(&host, 2))
        .expect("ascset over tcp");
    assert_eq!(dev.lock().unwrap().mode().index, 2);
    assert!(dev.lock().unwrap().reinitialising());

    dev.lock().unwrap().inject(AvalonFault::RefuseWorkmode);
    assert!(
        rt.block_on(client.set_workmode(&host, 1)).is_err(),
        "E status → error"
    );
}

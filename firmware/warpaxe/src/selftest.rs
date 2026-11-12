//! The self-test: a guided check of the miner while it mines, with one
//! PASS/FAIL and a list of what was measured against what. Started from
//! the web UI (System) or `POST /api/v1/selftest`; `GET` follows it and
//! keeps the last result until the next run or a reboot; `DELETE`
//! cancels. The checks and their limits are `warpaxe_board::selftest`
//! (docs/10); this module only collects.
//!
//! What it does, in order (about 80 s):
//!
//! 1. a snapshot of the board (chips, regulator, temperatures, input),
//!    after waiting for a chain that is not up yet (chips that were off
//!    for want of a pool come back on for the test) and, briefly, for
//!    work from a pool;
//! 2. the hash window, with work from a pool: once a second for
//!    [`WINDOW_S`] seconds, every
//!    chip's total, error and domain rates from the chips' own counters
//!    as the miner reads them, a chip that has not answered a counter
//!    read for a few seconds (`miner::AsicMonitor::fresh`) counting as
//!    hashing nothing. Without work, or with the pool gone for more
//!    than [`WORK_LOST_S`] seconds of it, there is no window and the
//!    hash checks are skipped (`Note::NoWork`);
//! 3. on a stacked regulator that asks for it (`tps546::Config::
//!    phase_check`: Naja Duo, Gamma Hex), each phase's current on its
//!    own, read by the board task under the load the window left
//!    ([`board::phase_currents`], ESP-Miner's `TPS546_check_phase_
//!    currents`); a phase under the floor fails the `phases` check and
//!    switches the chips off, as a dead slave phase leaves the master
//!    alone with the whole current;
//! 4. the fan step: both fan outputs held at least at 100 % for
//!    [`FAN_HOLD_S`] seconds through the board task's own loop
//!    (`board::fan_boost`), speed read before and at the end, then the
//!    hold released and the PID back in charge. The boost only raises
//!    the duty, never lowers it, and expires on its own in the board
//!    task if this thread died; it is released on every exit path here.
//!
//! The test never touches the clock or the core voltage, and nothing of
//! the board task's limits is bypassed: an overheat stop during the test
//! ends it (the chain goes off, which the window sees). A clock or
//! voltage change during the window (a tuner's step) ends the test as
//! "pointChanged": the window would mix two operating points.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;
use warpaxe_board::selftest::{
    judge, BoardReadings, Check, FanReadings, Limits, Note, PhaseReadings, Report, Status, Window,
    CHECKS,
};

use crate::{board, config, diag, events, miner};

/// The hash window, seconds.
pub const WINDOW_S: u32 = 60;
/// The fan step, seconds at full duty.
pub const FAN_HOLD_S: u32 = 10;
/// The result stays on the device display this long.
pub const SHOW_S: u64 = 300;
/// The thread's stack, PSRAM. The log line at the end says how much of
/// it was needed.
const STACK: usize = 16 * 1024;

/// Where a run stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Running: which step, how far (0–100).
    Running {
        /// `board`, `hashrate`, `phases`, `fan`, `judge`.
        step: &'static str,
        /// Percent done.
        progress: u8,
    },
    /// Finished with a report.
    Done,
    /// Ended early: `cancelled`, `pointChanged`, `chainStopped`.
    Aborted(&'static str),
}

/// One run, kept until the next.
#[derive(Clone, Debug)]
pub struct Run {
    /// Counts runs since boot; the display tells one result from the next by it.
    pub id: u32,
    pub state: State,
    pub started_unix: u64,
    pub started_uptime: u64,
    pub finished_uptime: Option<u64>,
    pub finished_unix: Option<u64>,
    pub duration_s: u32,
    /// The clock and core voltage the test ran at.
    pub frequency_mhz: u32,
    pub core_mv: u16,
    pub samples: u16,
    pub report: Option<Report>,
    /// Per chip: average GH/s, percent of nominal, error percent, the
    /// domains' percent of their share.
    pub chips: Vec<(f32, f32, f32, Vec<f32>)>,
    pub board: Option<BoardReadings>,
    pub fan: Option<FanReadings>,
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static ABORT: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU32 = AtomicU32::new(0);

fn update(f: impl FnOnce(&mut Run)) {
    if let Some(r) = RUN.lock().unwrap().as_mut() {
        f(r);
    }
}

/// The current or last run.
pub fn run() -> Option<Run> {
    RUN.lock().unwrap().clone()
}

/// Whether a test is running.
pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// Start a run. Refused while one is running, and when the thread
/// cannot be spawned.
pub fn start() -> Result<u32, &'static str> {
    if RUNNING.swap(true, Ordering::Relaxed) {
        return Err("a self-test is already running");
    }
    ABORT.store(false, Ordering::Relaxed);
    let id = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let cfg = config::get();
    *RUN.lock().unwrap() = Some(Run {
        id,
        state: State::Running {
            step: "board",
            progress: 0,
        },
        started_unix: crate::clock::unix_now().unwrap_or(0),
        started_uptime: diag::uptime_s(),
        finished_uptime: None,
        finished_unix: None,
        duration_s: 0,
        frequency_mhz: cfg.frequency,
        core_mv: cfg.core_voltage,
        samples: 0,
        report: None,
        chips: Vec::new(),
        board: None,
        fan: None,
    });
    // Nothing here writes flash: a PSRAM stack is fine, and the internal
    // heap stays for the update writer and the pool. The large values
    // (the window, the report) live on the heap, not on this stack: the
    // first device run (2026-10-21, 8 KB) ended in a panic without a
    // core dump, which is what a stack overflow in a PSRAM task leaves.
    if diag::spawn_psram("selftest", STACK, move || {
        let t0 = Instant::now();
        log::info!(
            "selftest: run {id} started at {} MHz / {} mV",
            cfg.frequency,
            cfg.core_voltage
        );
        let outcome = perform(&cfg);
        finish(id, outcome, t0.elapsed());
        log::info!(
            "selftest: run {id} done, {} of {STACK} bytes of the thread's stack never used",
            diag::stack_unused()
        );
        RUNNING.store(false, Ordering::Relaxed);
    })
    .is_err()
    {
        RUNNING.store(false, Ordering::Relaxed);
        *RUN.lock().unwrap() = None;
        return Err("not enough memory for the self-test thread");
    }
    Ok(id)
}

/// Cancel the running test; `true` when there was one.
pub fn abort() -> bool {
    if !running() {
        return false;
    }
    ABORT.store(true, Ordering::Relaxed);
    true
}

/// Releases the fan boost however the test ends.
struct FanRelease;

impl Drop for FanRelease {
    fn drop(&mut self) {
        board::fan_boost(0, 0);
    }
}

fn readings(cfg: &config::Config) -> BoardReadings {
    let b = board::state();
    let m = miner::state();
    let profile = board::profile();
    let regulator_fault =
        b.overheat_cause == Some("power") || b.power_fault_codes.iter().any(|c| *c != "temp_warn");
    BoardReadings {
        chips_found: m.chips,
        chips_expected: board::chips_expected(),
        hashing: m.phase == "READY" && !board::chain_stopped(),
        has_work: crate::stratum::tip().is_some(),
        has_regulator: b.has_vr,
        core_set_mv: cfg.core_voltage,
        core_actual_mv: b.vout_mv.map(|v| profile.core_mv_for_vout(v / 1000.0)),
        regulator_fault,
        has_thermal: b.has_thermal,
        chip_temp_c: b.temp_c,
        chip_limit_c: profile.chip_limit_c(cfg.overheat_temp),
        vr_temp_c: b.vr_temp_c.map(|c| c as f32),
        vr_limit_c: profile.vr_stop_c,
        vin_v: b.vin_mv.map(|v| v / 1000.0),
        nominal_v: f32::from(profile.nominal_voltage_v),
        phases: PhaseReadings::NotStacked,
    }
}

/// How long the phase read waits for the board task.
const PHASE_READ_WAIT: Duration = Duration::from_secs(5);

/// The point the test started at still holds.
fn point_unchanged(cfg: &config::Config) -> bool {
    let now = config::get();
    now.frequency == cfg.frequency && now.core_voltage == cfg.core_voltage
}

/// Sleep in short steps, watching for a cancel; `false` when cancelled.
fn wait(d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if ABORT.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

type Outcome = Result<(BoardReadings, Option<Box<Window>>, Option<FanReadings>), &'static str>;

/// How long a test waits for a chain that is not up (chips that were
/// off for want of a pool, a bring-up under way, the firmware's own
/// chips off/on) before its first reading.
const POWER_UP_WAIT: Duration = Duration::from_secs(45);
/// How long a test waits for work when there is none at its start: a
/// pool session may be between two connects, which is no reason to skip
/// the hash checks.
const WORK_WAIT: Duration = Duration::from_secs(10);
/// Seconds of the hash window without work from a pool after which the
/// window says nothing about the chips: the pool went away under the
/// test, and the hash checks are skipped as they are without work from
/// the start.
const WORK_LOST_S: u32 = 5;

fn perform(cfg: &config::Config) -> Outcome {
    let profile = board::profile();
    let chip = board::chip();
    // A chain that is not up yet gets time to come up: chips off
    // because no pool answers come back on for the test (`board::no_pool`
    // steps aside while one runs), and a pool that has just come back
    // has the chain in its bring-up. The rail, the warm-up and the
    // bring-up take some seconds, and a reading before that would call
    // the core voltage wrong and the chips not hashing. A chain that is
    // off for an overheat or by hand is not waited for.
    if profile.asic_count > 0 {
        let end = Instant::now() + POWER_UP_WAIT;
        while Instant::now() < end
            && !board::overheat()
            // Off by hand is not waited for; the firmware's own off/on
            // (a domain-latch reset) is over in seconds.
            && (!board::shutdown() || board::cycling())
            && (board::chain_stopped() || miner::state().phase != "READY")
        {
            if !wait(Duration::from_millis(500)) {
                return Err("cancelled");
            }
        }
    }
    if profile.asic_count > 0 {
        let end = Instant::now() + WORK_WAIT;
        while Instant::now() < end && crate::stratum::tip().is_none() {
            if !wait(Duration::from_millis(500)) {
                return Err("cancelled");
            }
        }
    }
    let first = readings(cfg);
    // The hash window, while the chain hashes: it needs work from a
    // pool. Without any the chain is up and counts nothing, and the
    // hash checks are skipped (`Note::NoWork`), not failed at 0 %.
    let mut has_work = first.has_work;
    let mut window = None;
    if first.hashing && has_work && first.chips_found > 0 {
        let m = miner::state();
        let expected = f32::from(chip.small_cores) * m.frequency_hz as f32 / 1e9;
        let mut w = Box::new(Window::new(
            first.chips_found,
            chip.domains,
            expected,
            &Limits::DEFAULT,
        ));
        // A chip with a clock of its own (`chipFreqOffsets`) is judged
        // at that clock; without offsets these are the figure above.
        for (c, &hz) in m.chip_clocks().iter().enumerate() {
            w.set_chip_expected(c, f32::from(chip.small_cores) * hz as f32 / 1e9);
        }
        // Another clock for one chip in the window is a changed point.
        let offsets = miner::chip_offsets_request();
        update(|r| {
            r.state = State::Running {
                step: "hashrate",
                progress: 5,
            }
        });
        let mut without_work = 0;
        for i in 0..WINDOW_S {
            if !wait(Duration::from_secs(1)) {
                return Err("cancelled");
            }
            if !point_unchanged(cfg) || miner::chip_offsets_request() != offsets {
                return Err("pointChanged");
            }
            if board::chain_stopped() {
                return Err("chainStopped");
            }
            if crate::stratum::tip().is_none() {
                without_work += 1;
            }
            let m = miner::state();
            for (c, a) in m.hashrate_monitor.iter().enumerate() {
                w.sample(c, a.total, a.error_ghs, &a.domains, a.fresh);
            }
            update(|r| {
                r.samples = w.samples();
                r.state = State::Running {
                    step: "hashrate",
                    progress: (5 + 80 * (i + 1) / WINDOW_S) as u8,
                };
            });
        }
        if without_work > WORK_LOST_S {
            log::warn!(
                "selftest: the pool sent no work for {without_work} s of the hash window: hash checks skipped"
            );
            has_work = false;
        } else {
            window = Some(w);
        }
    }
    // The phases, under the load the window just measured: a phase that
    // carries nothing while the chips draw their current is the finding.
    // Without work the chips idle and a phase may well read under the
    // floor, and with the chips off every phase reads nothing; neither
    // says anything about the stack, so no window means no phase read
    // (the check is skipped, as the hash checks are).
    let mut phases = PhaseReadings::NotStacked;
    if board::checks_phases() && window.is_some() && !board::chain_stopped() {
        update(|r| {
            r.state = State::Running {
                step: "phases",
                progress: 85,
            }
        });
        phases = match board::phase_currents(PHASE_READ_WAIT) {
            Some(Ok(c)) => PhaseReadings::Read(c),
            Some(Err(e)) => {
                log::warn!("selftest: phase currents: {e}");
                PhaseReadings::NoReading
            }
            None => {
                log::warn!("selftest: phase currents: the board task did not answer in time");
                PhaseReadings::NoReading
            }
        };
        if ABORT.load(Ordering::Relaxed) {
            return Err("cancelled");
        }
    }
    // The fan step.
    let mut fan = None;
    let b = board::state();
    if profile.has_fan() && b.has_thermal {
        update(|r| {
            r.state = State::Running {
                step: "fan",
                progress: 86,
            }
        });
        let before = board::state();
        let _release = FanRelease;
        board::fan_boost(100, u64::from(FAN_HOLD_S) * 1000 + 5000);
        if !wait(Duration::from_secs(u64::from(FAN_HOLD_S))) {
            return Err("cancelled");
        }
        let after = board::state();
        fan = Some(FanReadings {
            duty_before: before.fan_pct,
            rpm_before: before.fan_rpm,
            rpm2_before: before.fan2_rpm,
            rpm_full: after.fan_rpm,
            rpm2_full: after.fan2_rpm,
        });
    }
    update(|r| {
        r.state = State::Running {
            step: "judge",
            progress: 98,
        }
    });
    // The board as it is at the end: temperatures after the window, the
    // regulator's status now.
    let mut last = readings(cfg);
    // Whether there was work is the window's matter, not the last
    // second's.
    last.has_work = has_work;
    last.phases = phases;
    Ok((last, window, fan))
}

fn finish(id: u32, outcome: Outcome, took: Duration) {
    let asic = board::profile().asic;
    let (state, report, chips, board_r, fan) = match outcome {
        Err(why) => {
            log::warn!("selftest: run {id} ended early: {why}");
            (State::Aborted(why), None, Vec::new(), None, None)
        }
        Ok((b, window, fan)) => {
            let report = judge(asic, &Limits::DEFAULT, &b, window.as_deref(), fan.as_ref());
            let chips = window
                .as_ref()
                .map(|w| {
                    (0..usize::from(w.chips()))
                        .map(|c| {
                            let expected = w.expected_chip_ghs(c);
                            let ghs = w.chip_ghs(c);
                            let doms: Vec<f32> = (0..usize::from(w.domains()))
                                .map(|d| {
                                    let share = expected / f32::from(w.domains().max(1));
                                    let (g, n) = w.domain_ghs(c, d);
                                    if n == 0 || share <= 0.0 {
                                        0.0
                                    } else {
                                        g / share * 100.0
                                    }
                                })
                                .collect();
                            let pct = if expected > 0.0 {
                                ghs / expected * 100.0
                            } else {
                                0.0
                            };
                            (ghs, pct, w.chip_error_percent(c), doms)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let summary = warpaxe_diag::events::detail::selftest(&report);
            log::info!("selftest: run {id} {}", summary.text());
            events::record("selftest", summary);
            // A phase read below the floor: the stack is not sharing the
            // current, the master phase carries it all. Chips off, as
            // the maintenance switch does; the owner turns them back on
            // once the regulator has been looked at. A failed read is
            // reported, not acted on.
            let p = report.get(Check::Phases);
            if p.status == Status::Fail && p.note == Note::None {
                log::error!(
                    "selftest: regulator phase {} carries {:.1} A (floor {:.1} A): chips off",
                    p.phase.unwrap_or(0),
                    p.value.unwrap_or(0.0),
                    p.limit.unwrap_or(0.0)
                );
                board::set_shutdown(true);
            }
            (State::Done, Some(report), chips, Some(b), fan)
        }
    };
    update(|r| {
        r.state = state;
        r.report = report;
        r.chips = chips;
        r.board = board_r;
        r.fan = fan;
        r.finished_uptime = Some(diag::uptime_s());
        r.finished_unix = crate::clock::unix_now();
        r.duration_s = took.as_secs() as u32;
    });
    crate::display::touch();
}

fn f(v: Option<f32>) -> serde_json::Value {
    match v {
        Some(x) if x.is_finite() => json!((x * 100.0).round() / 100.0),
        _ => serde_json::Value::Null,
    }
}

/// The run for the API (`GET /api/v1/selftest`).
pub fn status_json() -> serde_json::Value {
    let Some(r) = run() else {
        return json!({ "state": "idle", "windowS": WINDOW_S, "fanHoldS": FAN_HOLD_S });
    };
    let (state, step, progress, abort) = match &r.state {
        State::Running { step, progress } => ("running", *step, *progress, None),
        State::Done => ("done", "", 100, None),
        State::Aborted(why) => ("aborted", "", 100, Some(*why)),
    };
    let checks: Vec<serde_json::Value> = r
        .report
        .map(|rep| {
            rep.outcomes
                .iter()
                .map(|o| {
                    json!({
                        "id": o.check.id(), "status": o.status.id(),
                        "value": f(o.value), "limit": f(o.limit), "value2": f(o.value2), "limit2": f(o.limit2),
                        "chip": o.chip, "domain": o.domain, "phase": o.phase,
                        "note": if o.note == Note::None { serde_json::Value::Null } else { json!(o.note.id()) },
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let chips: Vec<serde_json::Value> = r
        .chips
        .iter()
        .map(|(ghs, pct, err, doms)| {
            json!({ "ghs": f(Some(*ghs)), "percent": f(Some(*pct)), "errorPercent": f(Some(*err)), "domainsPercent": doms.iter().map(|d| f(Some(*d))).collect::<Vec<_>>() })
        })
        .collect();
    json!({
        "state": state, "id": r.id, "step": step, "progress": progress, "abortReason": abort,
        "startedAt": (r.started_unix > 0).then_some(r.started_unix), "startedUptime": r.started_uptime,
        "finishedAt": r.finished_unix.filter(|t| *t > 0), "finishedUptime": r.finished_uptime, "durationS": r.duration_s,
        "passed": r.report.map(|rep| rep.passed()), "judged": r.report.map(|rep| rep.judged()), "failed": r.report.map(|rep| rep.failed()),
        "frequency": r.frequency_mhz, "coreVoltage": r.core_mv, "board": board::profile().name, "asic": board::chip().name,
        "version": crate::api::VERSION, "windowS": WINDOW_S, "fanHoldS": FAN_HOLD_S, "samples": r.samples,
        "checks": checks, "chips": chips,
        "checkOrder": CHECKS.iter().map(|c| c.id()).collect::<Vec<_>>(),
    })
}

/// A few fields for `/api/system/info`, so the web UI knows when to ask
/// for the whole thing.
pub fn brief() -> serde_json::Value {
    match run() {
        None => json!({ "state": "idle" }),
        Some(r) => {
            let state = match &r.state {
                State::Running { .. } => "running",
                State::Done => "done",
                State::Aborted(_) => "aborted",
            };
            json!({ "state": state, "id": r.id, "passed": r.report.map(|rep| rep.passed()), "finishedAt": r.finished_unix.filter(|t| *t > 0), "finishedUptime": r.finished_uptime })
        }
    }
}

/// What the device display shows: the running test, or a result for
/// [`SHOW_S`] after it finished.
#[derive(Clone, Debug)]
pub struct Shown {
    pub id: u32,
    pub running: bool,
    pub step: &'static str,
    pub progress: u8,
    pub aborted: Option<&'static str>,
    pub report: Option<Report>,
}

/// The display's view, if there is something to show.
pub fn shown() -> Option<Shown> {
    let r = run()?;
    match r.state {
        State::Running { step, progress } => Some(Shown {
            id: r.id,
            running: true,
            step,
            progress,
            aborted: None,
            report: None,
        }),
        State::Done | State::Aborted(_) => {
            let fin = r.finished_uptime?;
            if diag::uptime_s().saturating_sub(fin) >= SHOW_S {
                return None;
            }
            let aborted = match r.state {
                State::Aborted(why) => Some(why),
                _ => None,
            };
            Some(Shown {
                id: r.id,
                running: false,
                step: "",
                progress: 100,
                aborted,
                report: r.report,
            })
        }
    }
}

//! The self-test's judgement: which checks there are, what each one is
//! measured against, and the verdict. No I/O and no clock: the firmware
//! collects the readings over its test window (`firmware/warpaxe/src/
//! selftest.rs`) and hands them in here, so every limit is a host test.
//!
//! The model is ESP-Miner's factory self-test (v2.15.3
//! `main/self_test/self_test.c`), read as a yardstick, not copied: it
//! runs instead of mining, after a factory flash or with the boot key
//! held, and stops at the first failure. This one runs while the miner
//! mines, at the configured clock and voltage, and reports every check
//! with its measured value, so a support case can be read off one list.
//!
//! The limits (`Limits::DEFAULT`) and where they come from:
//!
//! | Check | Limit | Basis |
//! |---|---|---|
//! | chip count | = the configured or profile count | ESP-Miner stops at a count mismatch in `asic_initialize` |
//! | hashrate per chip | ≥ 80 % of small cores × clock, averaged over the window | ESP-Miner wants ≥ 85 % of the nominal total from nonces (`hashrate_test_percentage_target`, line 591–594, 737); the fleet's chip counters average 99–101 % over a minute (BM1370, BM1373) and 88 % on the BM1397, whose rate register is what it reports (2026-10-21) |
//! | domains | each ≥ 67 % of the chip's expected share, averaged; a sample above 3× the share is implausible and dropped | ESP-Miner `SELF_TEST_DOMAIN_HASHRATE_TOLERANCE` 0.33 (line 35, 677–678), `max_plausible_hashrate` 3× (137) |
//! | error rate | BM1370 ≤ 5 %, BM1373 ≤ 8 %, none on the BM1397 | ESP-Miner has no error-rate check; the fleet runs BM1370 chips at 0–1.3 % at stock and max points and 2.6 % at WarpTune's eco point (the Nano, 2026-10-21), the BM1373 at 3–6 % (docs/05). The rate is a tuning figure as much as a hardware one (it rises as the voltage drops), so the line sits well above any tuned point and the measured value is always reported |
//! | core voltage | measured within ±5 % of the set value | ESP-Miner ±10 % of the default voltage (`SELF_TEST_CORE_VOLTAGE_TOLERANCE`, 38, 405–419); the fleet's regulators read back within 0.5 % |
//! | regulator | no fault condition | ESP-Miner `test_vreg_faults` (442–451) |
//! | phases | every phase of a stacked regulator ≥ 1 A, read one phase at a time with PHASE paged and restored | ESP-Miner `TPS546_check_phase_currents`, `MULTIPHASE_BUCK_MIN_CURRENT_A` 1.0 (`self_test.c` 44–49, 377–384): a dead slave phase shows nothing in the summed telemetry; on the stacked boards that have not run yet (Naja Duo, Gamma Hex) |
//! | fan | ≥ 500 rpm at 100 % duty, and faster than before when the duty was below 90 % | ESP-Miner `test_fan_sense` wants more than `self_test_fan_target_rpm` (1000 by default, 500 on the Turbo, Naja Duo and Hex; 353–369) at whatever duty its PID holds |
//! | temperatures | chip and regulator read, below their stop lines | ESP-Miner fails at a missing reading (`TEMP:FAIL`, 214–223) and above `selftest_max` 70 °C (620–625); the stop lines are the board's (docs/08) |
//! | input voltage | within ±10 % of the board's nominal input | ESP-Miner `INPUT_VOLTAGE_MARGIN` 0.10 (45, 421–440), INA260 boards only |
//!
//! Not checked, on purpose: power against a per-board target (ESP-Miner
//! `test_power_consumption`, 371–403) — a tuned miner runs at any point,
//! and the regulator's own over-current limit guards the board.

use crate::profile::Asic;
use crate::tps546::PhaseCurrents;

/// Chips a report can hold (the chain driver's limit).
pub const MAX_CHIPS: usize = 16;
/// Domains per chip a report can hold.
pub const MAX_DOMAINS: usize = 8;

/// A check, in the order the report lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Chips found on the chain against the expected count.
    Chips,
    /// Every chip's hash counter against small cores × clock.
    Hashrate,
    /// Every domain counter against its share of the chip's rate.
    Domains,
    /// The chips' error counters against the chip type's limit.
    Errors,
    /// The regulator's output against the set core voltage.
    CoreVoltage,
    /// The regulator's status word.
    Regulator,
    /// Each phase of a stacked regulator carries current.
    Phases,
    /// The fan's response to full duty.
    Fan,
    /// Chip and regulator temperature, read and under their stop lines.
    Temperature,
    /// The board's input voltage against its nominal.
    InputVoltage,
}

/// Every check, in report order.
pub const CHECKS: [Check; 10] = [
    Check::Chips,
    Check::Hashrate,
    Check::Domains,
    Check::Errors,
    Check::CoreVoltage,
    Check::Regulator,
    Check::Phases,
    Check::Fan,
    Check::Temperature,
    Check::InputVoltage,
];

impl Check {
    /// The check's name in the API.
    pub fn id(self) -> &'static str {
        match self {
            Check::Chips => "chips",
            Check::Hashrate => "hashrate",
            Check::Domains => "domains",
            Check::Errors => "errors",
            Check::CoreVoltage => "coreVoltage",
            Check::Regulator => "regulator",
            Check::Phases => "phases",
            Check::Fan => "fan",
            Check::Temperature => "temperature",
            Check::InputVoltage => "inputVoltage",
        }
    }
}

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Within its limit.
    Pass,
    /// Outside its limit, or could not be measured where it should be.
    Fail,
    /// Not applicable on this board (no fan, no regulator, no counter).
    Skip,
}

impl Status {
    /// The status's name in the API.
    pub fn id(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }
}

/// What a verdict rests on beyond its numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Note {
    /// Nothing to add.
    #[default]
    None,
    /// The chain is not hashing (chips off, overheat stop, bring-up
    /// failed), so there was nothing to measure.
    NotHashing,
    /// The chain is up but no pool sends work: nothing to hash, so the
    /// checks that count hashes are skipped, not failed.
    NoWork,
    /// A reading the board should have was missing.
    NoReading,
    /// The board has no regulator the firmware reads.
    NoRegulator,
    /// The board has no fan the firmware drives.
    NoFan,
    /// The board has no sensor the firmware reads.
    NoSensor,
    /// The chip has no error counter (BM1397).
    NoCounter,
    /// The chip has no domain counters (BM1397).
    NoDomains,
    /// The chip stopped answering register reads during the window.
    NoReply,
    /// Every sample of the counter was implausible.
    NoSamples,
    /// The fan did not speed up at full duty.
    NoReaction,
    /// The regulator reports a fault condition.
    RegulatorFault,
    /// A temperature is above its stop line.
    Overheat,
    /// The second fan output reads no speed at full duty: nothing is
    /// connected there (not a failure on a one-fan board).
    SecondFanMissing,
    /// The regulator has one phase, or the firmware does not page its
    /// phases on this board: no phase check.
    SinglePhase,
}

impl Note {
    /// The note's name in the API; empty for none.
    pub fn id(self) -> &'static str {
        match self {
            Note::None => "",
            Note::NotHashing => "notHashing",
            Note::NoWork => "noWork",
            Note::NoReading => "noReading",
            Note::NoRegulator => "noRegulator",
            Note::NoFan => "noFan",
            Note::NoSensor => "noSensor",
            Note::NoCounter => "noCounter",
            Note::NoDomains => "noDomains",
            Note::NoReply => "noReply",
            Note::NoSamples => "noSamples",
            Note::NoReaction => "noReaction",
            Note::RegulatorFault => "regulatorFault",
            Note::Overheat => "overheat",
            Note::SecondFanMissing => "secondFanMissing",
            Note::SinglePhase => "singlePhase",
        }
    }
}

/// One check's verdict with the figures it rests on. What `value`,
/// `limit`, `value2` and `limit2` mean depends on the check:
///
/// | Check | value | limit | value2 | limit2 |
/// |---|---|---|---|---|
/// | chips | found | expected | | |
/// | hashrate | the worst chip's % of nominal | minimum % | | |
/// | domains | the worst domain's % of its share | minimum % | | |
/// | errors | the worst chip's error % | maximum % | | |
/// | coreVoltage | measured mV | set mV | allowed deviation mV | |
/// | regulator | | | | |
/// | phases | the weakest phase's A (`phase` names it) | minimum A | all phases together, A | |
/// | fan | rpm at full duty | minimum rpm | rpm before | second fan's rpm at full duty |
/// | temperature | chip °C | chip stop °C | regulator °C | regulator stop °C |
/// | inputVoltage | measured V | nominal V | allowed deviation V | |
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outcome {
    /// Which check.
    pub check: Check,
    /// The verdict.
    pub status: Status,
    /// The measured figure the verdict rests on.
    pub value: Option<f32>,
    /// What it was judged against.
    pub limit: Option<f32>,
    /// A second figure, per the table above.
    pub value2: Option<f32>,
    /// Its limit.
    pub limit2: Option<f32>,
    /// The chip the figure belongs to (the worst one), where per chip.
    pub chip: Option<u8>,
    /// The domain of that chip, where per domain.
    pub domain: Option<u8>,
    /// The regulator phase the figure belongs to (the weakest one).
    pub phase: Option<u8>,
    /// What the verdict rests on beyond the figures.
    pub note: Note,
}

impl Outcome {
    const fn skip(check: Check, note: Note) -> Self {
        Outcome {
            check,
            status: Status::Skip,
            value: None,
            limit: None,
            value2: None,
            limit2: None,
            chip: None,
            domain: None,
            phase: None,
            note,
        }
    }

    fn with(check: Check, status: Status) -> Self {
        Outcome {
            check,
            status,
            value: None,
            limit: None,
            value2: None,
            limit2: None,
            chip: None,
            domain: None,
            phase: None,
            note: Note::None,
        }
    }
}

/// The whole test's verdicts, one per [`CHECKS`] entry in that order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Report {
    /// One outcome per check.
    pub outcomes: [Outcome; CHECKS.len()],
}

impl Report {
    /// No check failed (skipped ones do not count).
    pub fn passed(&self) -> bool {
        self.failed() == 0
    }

    /// How many checks failed.
    pub fn failed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.status == Status::Fail)
            .count()
    }

    /// How many checks were judged (passed or failed).
    pub fn judged(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.status != Status::Skip)
            .count()
    }

    /// The first failed check, if any.
    pub fn first_failure(&self) -> Option<&Outcome> {
        self.outcomes.iter().find(|o| o.status == Status::Fail)
    }

    /// The outcome of one check.
    pub fn get(&self, check: Check) -> &Outcome {
        &self.outcomes[CHECKS.iter().position(|c| *c == check).unwrap_or(0)]
    }
}

/// The limits every check is judged against. See the module table for
/// where each one comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// A chip's average rate over the window, as a share of small cores ×
    /// clock, at least this.
    pub chip_min_ratio: f32,
    /// A domain's average rate, as a share of the chip's expected rate
    /// divided by its domains, at least this.
    pub domain_min_ratio: f32,
    /// A domain sample above this multiple of its share is implausible
    /// and dropped from the average.
    pub domain_reject_ratio: f32,
    /// A chip's error rate over the window, percent, at most this: per
    /// chip type, `None` where the chip has no error counter.
    pub error_max_percent_bm1370: f32,
    /// The same for the BM1373.
    pub error_max_percent_bm1373: f32,
    /// The measured core voltage within this share of the set value.
    pub core_tolerance: f32,
    /// The input voltage within this share of the nominal.
    pub vin_tolerance: f32,
    /// The fan at full duty at least this fast.
    pub fan_min_rpm: u16,
    /// With the duty below this before the test, the fan must speed up.
    pub fan_reaction_below_pct: u8,
    /// … by at least this factor.
    pub fan_rise_ratio: f32,
    /// Every phase of a stacked regulator carries at least this, A.
    pub phase_min_a: f32,
    /// All phases together below this, A: the chips were not loading
    /// the regulator when it was read (a chain restart, a pause without
    /// pool work), so the phases are not judged.
    pub phase_load_min_a: f32,
}

impl Limits {
    /// The limits the firmware ships with.
    pub const DEFAULT: Limits = Limits {
        chip_min_ratio: 0.80,
        domain_min_ratio: 0.67,
        domain_reject_ratio: 3.0,
        error_max_percent_bm1370: 5.0,
        error_max_percent_bm1373: 8.0,
        core_tolerance: 0.05,
        vin_tolerance: 0.10,
        fan_min_rpm: 500,
        fan_reaction_below_pct: 90,
        fan_rise_ratio: 1.10,
        phase_min_a: 1.0,
        phase_load_min_a: 5.0,
    };

    /// The error-rate limit for a chip type, percent; `None` for a chip
    /// without an error counter.
    pub fn error_max_percent(&self, asic: Asic) -> Option<f32> {
        match asic {
            Asic::Bm1370 => Some(self.error_max_percent_bm1370),
            Asic::Bm1373 => Some(self.error_max_percent_bm1373),
            Asic::Bm1397 => None,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct ChipAcc {
    /// Samples taken.
    n: u16,
    /// Of those, samples where the chip had not answered a counter read
    /// recently (its rate counted as zero).
    stale: u16,
    total_sum: f32,
    error_sum: f32,
    domain_sum: [f32; MAX_DOMAINS],
    domain_n: [u16; MAX_DOMAINS],
    domain_rejected: [u16; MAX_DOMAINS],
}

/// The hash counters over the test window: one sample per chip per
/// second, averaged at the end. Averaging is what makes the domain
/// figures usable: a chip's four or eight counters are read in turn, so a
/// single second shows 65–125 % of a domain's share (fleet, 2026-10-21)
/// while a minute shows 97–104 %.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    chips: u8,
    domains: u8,
    /// What each chip is expected to hash, GH/s.
    expected_ghs: [f32; MAX_CHIPS],
    /// A domain sample above this many times its share is dropped.
    reject_ratio: f32,
    acc: [ChipAcc; MAX_CHIPS],
}

impl Window {
    /// A window for `chips` chips with `domains` counters each, expected
    /// to hash `expected_chip_ghs` (small cores × clock) apiece.
    pub fn new(chips: u8, domains: u8, expected_chip_ghs: f32, limits: &Limits) -> Self {
        Window {
            chips: chips.min(MAX_CHIPS as u8),
            domains: domains.min(MAX_DOMAINS as u8),
            expected_ghs: [expected_chip_ghs; MAX_CHIPS],
            reject_ratio: limits.domain_reject_ratio,
            acc: [ChipAcc::default(); MAX_CHIPS],
        }
    }

    /// Chip `chip` runs at a clock of its own (an offset from the
    /// chain's): what it is expected to hash at that clock, GH/s. Its
    /// total and its domains are judged against this, not against the
    /// chain's figure.
    pub fn set_chip_expected(&mut self, chip: usize, ghs: f32) {
        if let Some(e) = self.expected_ghs.get_mut(chip) {
            *e = ghs;
        }
    }

    /// A domain's share of chip `chip`'s expected rate, GH/s.
    fn share(&self, chip: usize) -> f32 {
        if self.domains > 0 {
            self.expected_ghs[chip] / f32::from(self.domains)
        } else {
            0.0
        }
    }

    /// Chips the window was opened for.
    pub fn chips(&self) -> u8 {
        self.chips
    }

    /// Domains per chip the window was opened for.
    pub fn domains(&self) -> u8 {
        self.domains
    }

    /// What chip `chip` was expected to hash, GH/s.
    pub fn expected_chip_ghs(&self, chip: usize) -> f32 {
        self.expected_ghs.get(chip).copied().unwrap_or(0.0)
    }

    /// One reading of chip `chip`: its total rate, its error rate and its
    /// domain rates in GH/s as the monitor reports them. `fresh` is false
    /// when the chip has not answered a counter read recently; its rates
    /// then count as zero, whatever the last reading said.
    pub fn sample(
        &mut self,
        chip: usize,
        total_ghs: f32,
        error_ghs: f32,
        domains: &[f32],
        fresh: bool,
    ) {
        if chip >= usize::from(self.chips) {
            return;
        }
        let reject_above_ghs = self.share(chip) * self.reject_ratio;
        let a = &mut self.acc[chip];
        a.n = a.n.saturating_add(1);
        if !fresh {
            a.stale = a.stale.saturating_add(1);
            return;
        }
        a.total_sum += total_ghs.max(0.0);
        a.error_sum += error_ghs.max(0.0);
        for (d, &ghs) in domains.iter().enumerate().take(usize::from(self.domains)) {
            if ghs.is_nan() || ghs > reject_above_ghs {
                a.domain_rejected[d] = a.domain_rejected[d].saturating_add(1);
            } else {
                a.domain_sum[d] += ghs.max(0.0);
                a.domain_n[d] = a.domain_n[d].saturating_add(1);
            }
        }
    }

    /// Samples taken for chip 0 (every chip is sampled each round).
    pub fn samples(&self) -> u16 {
        self.acc[0].n
    }

    /// A chip's average total rate over the window, GH/s.
    pub fn chip_ghs(&self, chip: usize) -> f32 {
        let a = &self.acc[chip];
        if a.n == 0 {
            0.0
        } else {
            a.total_sum / f32::from(a.n)
        }
    }

    /// A chip's average error rate over the window, percent of its total.
    pub fn chip_error_percent(&self, chip: usize) -> f32 {
        let a = &self.acc[chip];
        if a.total_sum > 0.0 {
            a.error_sum / a.total_sum * 100.0
        } else {
            0.0
        }
    }

    /// A domain's average rate over its accepted samples, GH/s, and how
    /// many samples were accepted.
    pub fn domain_ghs(&self, chip: usize, domain: usize) -> (f32, u16) {
        let a = &self.acc[chip];
        let n = a.domain_n[domain];
        if n == 0 {
            (0.0, 0)
        } else {
            (a.domain_sum[domain] / f32::from(n), n)
        }
    }
}

/// What the board reports at the time of the test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardReadings {
    /// Chips the chain found.
    pub chips_found: u8,
    /// Chips there should be (the config's count, else the profile's;
    /// 0 = a board without a chain, the check is skipped).
    pub chips_expected: u8,
    /// The chain is up and taking work.
    pub hashing: bool,
    /// A pool has work for it. Without work a chain that is up hashes
    /// nothing through no fault of the board's: the hash checks are
    /// skipped then ([`Note::NoWork`]), the rest is judged as ever.
    pub has_work: bool,
    /// The board has a regulator the firmware reads.
    pub has_regulator: bool,
    /// The configured core voltage, mV.
    pub core_set_mv: u16,
    /// The regulator's output as a core voltage, mV.
    pub core_actual_mv: Option<u16>,
    /// The regulator reports a fault (AxeOS `power_fault` as a fault, or
    /// a stop with cause "power").
    pub regulator_fault: bool,
    /// The board has a thermal sensor the firmware reads.
    pub has_thermal: bool,
    /// The chip temperature, °C.
    pub chip_temp_c: Option<f32>,
    /// The chip stop line in force, °C.
    pub chip_limit_c: f32,
    /// The regulator temperature, °C.
    pub vr_temp_c: Option<f32>,
    /// The regulator stop line, °C.
    pub vr_limit_c: f32,
    /// The input voltage, V.
    pub vin_v: Option<f32>,
    /// The board's nominal input, V.
    pub nominal_v: f32,
    /// The regulator's phases, each read on its own.
    pub phases: PhaseReadings,
}

/// What the test read from a stacked regulator's phases
/// (`Tps546::phase_currents`), taken under load after the hash window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhaseReadings {
    /// One phase, or a board whose phases the firmware does not page
    /// (`tps546::Config::phase_check`): nothing to check.
    NotStacked,
    /// The read failed on the bus (the page was restored).
    NoReading,
    /// One current per phase.
    Read(PhaseCurrents),
}

/// The fan test's readings: the speeds before, and at full duty.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanReadings {
    /// The chip fan's duty before the test, %.
    pub duty_before: u8,
    /// The chip fan's speed before, rpm.
    pub rpm_before: u16,
    /// The second output's speed before, where there is one.
    pub rpm2_before: Option<u16>,
    /// The chip fan's speed at full duty, rpm.
    pub rpm_full: u16,
    /// The second output's speed at full duty.
    pub rpm2_full: Option<u16>,
}

fn abs(x: f32) -> f32 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// Judge everything. `window` is `None` when the chain was not hashing
/// or had no work (nothing could be sampled); `fan` is `None` on a
/// board without a fan the firmware drives.
pub fn judge(
    asic: Asic,
    limits: &Limits,
    board: &BoardReadings,
    window: Option<&Window>,
    fan: Option<&FanReadings>,
) -> Report {
    Report {
        outcomes: [
            judge_chips(board),
            judge_hashrate(limits, board, window),
            judge_domains(limits, board, window),
            judge_errors(asic, limits, board, window),
            judge_core_voltage(limits, board),
            judge_regulator(board),
            judge_phases(limits, board),
            judge_fan(limits, fan),
            judge_temperature(board),
            judge_input_voltage(limits, board),
        ],
    }
}

fn judge_chips(b: &BoardReadings) -> Outcome {
    if b.chips_expected == 0 {
        return Outcome::skip(Check::Chips, Note::NotHashing);
    }
    let ok = b.hashing && b.chips_found == b.chips_expected;
    let mut o = Outcome::with(Check::Chips, if ok { Status::Pass } else { Status::Fail });
    o.value = Some(f32::from(b.chips_found));
    o.limit = Some(f32::from(b.chips_expected));
    if !b.hashing {
        o.note = Note::NotHashing;
    }
    o
}

/// A chain that is up but had no work and so no window: the checks
/// that count hashes have nothing to judge. A chain that is not up
/// (chips off, an overheat stop, a failed bring-up) is another matter
/// and stays a failure.
fn no_work(b: &BoardReadings, w: Option<&Window>) -> bool {
    b.hashing && !b.has_work && w.is_none()
}

/// The worst chip of the window against `min_ratio`: (ratio, chip,
/// mostly stale).
fn worst_chip(w: &Window) -> (f32, u8, bool) {
    let mut worst = (f32::INFINITY, 0u8, false);
    for i in 0..usize::from(w.chips()) {
        let ratio = if w.expected_ghs[i] > 0.0 {
            w.chip_ghs(i) / w.expected_ghs[i]
        } else {
            0.0
        };
        if ratio < worst.0 {
            let a = &w.acc[i];
            worst = (ratio, i as u8, a.n > 0 && a.stale * 2 > a.n);
        }
    }
    if worst.0.is_infinite() {
        worst.0 = 0.0;
    }
    worst
}

fn judge_hashrate(limits: &Limits, b: &BoardReadings, w: Option<&Window>) -> Outcome {
    if b.chips_expected == 0 {
        return Outcome::skip(Check::Hashrate, Note::NotHashing);
    }
    if no_work(b, w) {
        return Outcome::skip(Check::Hashrate, Note::NoWork);
    }
    let mut o = Outcome::with(Check::Hashrate, Status::Fail);
    o.limit = Some(limits.chip_min_ratio * 100.0);
    let Some(w) = w.filter(|w| w.chips() > 0 && w.samples() > 0) else {
        o.note = Note::NotHashing;
        return o;
    };
    let (ratio, chip, stale) = worst_chip(w);
    o.value = Some(ratio * 100.0);
    o.chip = Some(chip);
    if ratio >= limits.chip_min_ratio {
        o.status = Status::Pass;
    } else if stale {
        o.note = Note::NoReply;
    }
    o
}

fn judge_domains(limits: &Limits, b: &BoardReadings, w: Option<&Window>) -> Outcome {
    if b.chips_expected == 0 {
        return Outcome::skip(Check::Domains, Note::NotHashing);
    }
    if no_work(b, w) {
        return Outcome::skip(Check::Domains, Note::NoWork);
    }
    let mut o = Outcome::with(Check::Domains, Status::Fail);
    o.limit = Some(limits.domain_min_ratio * 100.0);
    let Some(w) = w.filter(|w| w.chips() > 0 && w.samples() > 0) else {
        o.note = Note::NotHashing;
        return o;
    };
    if w.domains == 0 {
        return Outcome::skip(Check::Domains, Note::NoDomains);
    }
    let mut worst = (f32::INFINITY, 0u8, 0u8, Note::None);
    for i in 0..usize::from(w.chips()) {
        let share = w.share(i);
        for d in 0..usize::from(w.domains) {
            let (ghs, n) = w.domain_ghs(i, d);
            let (ratio, note) = if n == 0 {
                (0.0, Note::NoSamples)
            } else if share > 0.0 {
                (ghs / share, Note::None)
            } else {
                (0.0, Note::None)
            };
            if ratio < worst.0 {
                worst = (ratio, i as u8, d as u8, note);
            }
        }
    }
    o.value = Some(worst.0 * 100.0);
    o.chip = Some(worst.1);
    o.domain = Some(worst.2);
    if worst.0 >= limits.domain_min_ratio {
        o.status = Status::Pass;
    } else {
        o.note = worst.3;
    }
    o
}

fn judge_errors(asic: Asic, limits: &Limits, b: &BoardReadings, w: Option<&Window>) -> Outcome {
    if b.chips_expected == 0 {
        return Outcome::skip(Check::Errors, Note::NotHashing);
    }
    let Some(max) = limits.error_max_percent(asic) else {
        return Outcome::skip(Check::Errors, Note::NoCounter);
    };
    if no_work(b, w) {
        return Outcome::skip(Check::Errors, Note::NoWork);
    }
    let mut o = Outcome::with(Check::Errors, Status::Fail);
    o.limit = Some(max);
    let Some(w) = w.filter(|w| w.chips() > 0 && w.samples() > 0) else {
        o.note = Note::NotHashing;
        return o;
    };
    let mut worst = (-1.0f32, 0u8);
    for i in 0..usize::from(w.chips()) {
        let pct = w.chip_error_percent(i);
        if pct > worst.0 {
            worst = (pct, i as u8);
        }
    }
    o.value = Some(worst.0.max(0.0));
    o.chip = Some(worst.1);
    if worst.0 <= max {
        o.status = Status::Pass;
    }
    o
}

fn judge_core_voltage(limits: &Limits, b: &BoardReadings) -> Outcome {
    if !b.has_regulator {
        return Outcome::skip(Check::CoreVoltage, Note::NoRegulator);
    }
    let mut o = Outcome::with(Check::CoreVoltage, Status::Fail);
    let set = f32::from(b.core_set_mv);
    let allowed = set * limits.core_tolerance;
    o.limit = Some(set);
    o.value2 = Some(allowed);
    let Some(actual) = b.core_actual_mv else {
        o.note = Note::NoReading;
        return o;
    };
    let actual = f32::from(actual);
    o.value = Some(actual);
    if abs(actual - set) <= allowed {
        o.status = Status::Pass;
    }
    o
}

fn judge_regulator(b: &BoardReadings) -> Outcome {
    if !b.has_regulator {
        return Outcome::skip(Check::Regulator, Note::NoRegulator);
    }
    let mut o = Outcome::with(Check::Regulator, Status::Pass);
    if b.regulator_fault {
        o.status = Status::Fail;
        o.note = Note::RegulatorFault;
    }
    o
}

/// Every phase of a stacked regulator above the floor: the weakest one
/// is the figure, with the phase it is; the sum is `value2`, so a
/// reading taken at a light load reads as what it is.
fn judge_phases(limits: &Limits, b: &BoardReadings) -> Outcome {
    let currents = match b.phases {
        PhaseReadings::NotStacked => return Outcome::skip(Check::Phases, Note::SinglePhase),
        PhaseReadings::NoReading => {
            let mut o = Outcome::with(Check::Phases, Status::Fail);
            o.limit = Some(limits.phase_min_a);
            o.note = Note::NoReading;
            return o;
        }
        PhaseReadings::Read(c) => c,
    };
    // No load in the sum: a dead slave phase leaves the master carrying
    // everything, so the sum stays high; a sum near zero is a read taken
    // while the chips did not draw (a chain restart, no pool work in the
    // last seconds), nothing to judge a phase by.
    if currents.total() < limits.phase_load_min_a {
        let mut o = Outcome::skip(Check::Phases, Note::NoWork);
        o.value2 = Some(currents.total());
        return o;
    }
    let mut o = Outcome::with(Check::Phases, Status::Fail);
    o.limit = Some(limits.phase_min_a);
    o.value2 = Some(currents.total());
    let Some((phase, amps)) = currents.weakest() else {
        o.note = Note::NoReading;
        return o;
    };
    o.value = Some(amps);
    o.phase = Some(phase);
    if amps >= limits.phase_min_a {
        o.status = Status::Pass;
    }
    o
}

fn judge_fan(limits: &Limits, fan: Option<&FanReadings>) -> Outcome {
    let Some(f) = fan else {
        return Outcome::skip(Check::Fan, Note::NoFan);
    };
    let mut o = Outcome::with(Check::Fan, Status::Pass);
    o.value = Some(f32::from(f.rpm_full));
    o.limit = Some(f32::from(limits.fan_min_rpm));
    o.value2 = Some(f32::from(f.rpm_before));
    if f.rpm_full < limits.fan_min_rpm {
        o.status = Status::Fail;
    } else if f.duty_before < limits.fan_reaction_below_pct
        && f32::from(f.rpm_full) < f32::from(f.rpm_before) * limits.fan_rise_ratio
    {
        o.status = Status::Fail;
        o.note = Note::NoReaction;
    }
    // The second output: judged only where something spins on it.
    if let Some(rpm2) = f.rpm2_full {
        o.limit2 = Some(f32::from(rpm2));
        let ever = rpm2 > 0 || f.rpm2_before.unwrap_or(0) > 0;
        if !ever {
            if o.note == Note::None {
                o.note = Note::SecondFanMissing;
            }
        } else if rpm2 < limits.fan_min_rpm {
            o.status = Status::Fail;
        }
    }
    o
}

fn judge_temperature(b: &BoardReadings) -> Outcome {
    if !b.has_thermal {
        return Outcome::skip(Check::Temperature, Note::NoSensor);
    }
    let mut o = Outcome::with(Check::Temperature, Status::Pass);
    o.limit = Some(b.chip_limit_c);
    o.value = b.chip_temp_c;
    match b.chip_temp_c {
        None => {
            o.status = Status::Fail;
            o.note = Note::NoReading;
        }
        Some(t) if t > b.chip_limit_c => {
            o.status = Status::Fail;
            o.note = Note::Overheat;
        }
        _ => {}
    }
    if b.has_regulator {
        o.limit2 = Some(b.vr_limit_c);
        o.value2 = b.vr_temp_c;
        match b.vr_temp_c {
            None => {
                o.status = Status::Fail;
                if o.note == Note::None {
                    o.note = Note::NoReading;
                }
            }
            Some(t) if t > b.vr_limit_c => {
                o.status = Status::Fail;
                if o.note == Note::None {
                    o.note = Note::Overheat;
                }
            }
            _ => {}
        }
    }
    o
}

fn judge_input_voltage(limits: &Limits, b: &BoardReadings) -> Outcome {
    let Some(vin) = b.vin_v else {
        return Outcome::skip(Check::InputVoltage, Note::NoReading);
    };
    let mut o = Outcome::with(Check::InputVoltage, Status::Fail);
    let allowed = b.nominal_v * limits.vin_tolerance;
    o.value = Some(vin);
    o.limit = Some(b.nominal_v);
    o.value2 = Some(allowed);
    if abs(vin - b.nominal_v) <= allowed {
        o.status = Status::Pass;
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Bitaxe Gamma at 525 MHz / 1020 mV as the fleet's .42 reports it.
    fn gamma() -> BoardReadings {
        BoardReadings {
            chips_found: 1,
            chips_expected: 1,
            hashing: true,
            has_work: true,
            has_regulator: true,
            core_set_mv: 1020,
            core_actual_mv: Some(1017),
            regulator_fault: false,
            has_thermal: true,
            chip_temp_c: Some(65.1),
            chip_limit_c: 75.0,
            vr_temp_c: Some(57.0),
            vr_limit_c: 105.0,
            vin_v: Some(5.23),
            nominal_v: 5.0,
            phases: PhaseReadings::NotStacked,
        }
    }

    /// Two phases as a Naja Duo at 327 MHz / 1000 mV shares its 24 A.
    fn two_phases(a0: f32, a1: f32) -> PhaseReadings {
        let mut amps = [0.0; crate::tps546::MAX_PHASES];
        amps[0] = a0;
        amps[1] = a1;
        PhaseReadings::Read(PhaseCurrents { count: 2, amps })
    }

    fn fan_ok() -> FanReadings {
        FanReadings {
            duty_before: 36,
            rpm_before: 2106,
            rpm2_before: None,
            rpm_full: 4800,
            rpm2_full: None,
        }
    }

    /// A window of `chips` BM1370 chips hashing at `ratio` of nominal
    /// with `err_pct` errors, 60 samples, domains round-robin.
    fn window(chips: u8, ratio: f32, err_pct: f32) -> Window {
        let expected = 1071.0;
        let mut w = Window::new(chips, 4, expected, &Limits::DEFAULT);
        for _ in 0..60 {
            for c in 0..usize::from(chips) {
                let ghs = expected * ratio;
                let d = [ghs / 4.0; 4];
                w.sample(c, ghs, ghs * err_pct / 100.0, &d, true);
            }
        }
        w
    }

    /// A one-phase board skips the phase check; a Naja Duo with both
    /// phases carrying current passes it; a dead slave phase (U7 off
    /// the stack) fails it naming the phase, with the sum beside it; a
    /// bus failure fails it as no reading.
    #[test]
    fn the_phase_check_wants_every_phase_of_a_stack_above_an_amp() {
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &gamma(), None, None);
        let p = r.get(Check::Phases);
        assert_eq!((p.status, p.note), (Status::Skip, Note::SinglePhase));

        let naja = BoardReadings {
            phases: two_phases(12.25, 12.0),
            ..gamma()
        };
        let r = judge(
            Asic::Bm1373,
            &Limits::DEFAULT,
            &naja,
            Some(&window(2, 0.98, 1.0)),
            Some(&fan_ok()),
        );
        let p = r.get(Check::Phases);
        assert_eq!(p.status, Status::Pass);
        assert_eq!(
            (p.value, p.limit, p.phase),
            (Some(12.0), Some(1.0), Some(1))
        );
        assert_eq!(p.value2, Some(24.25));
        assert!(r.passed(), "{r:?}");
        assert_eq!(r.judged(), 10);

        let dead = BoardReadings {
            phases: two_phases(23.9, 0.3),
            ..gamma()
        };
        let r = judge(
            Asic::Bm1373,
            &Limits::DEFAULT,
            &dead,
            Some(&window(2, 0.98, 1.0)),
            Some(&fan_ok()),
        );
        let p = r.get(Check::Phases);
        assert_eq!(
            (p.status, p.phase, p.value),
            (Status::Fail, Some(1), Some(0.3))
        );
        assert_eq!(p.value2.map(|v| (v * 10.0).round() / 10.0), Some(24.2));
        assert_eq!(r.failed(), 1);
        assert_eq!(r.first_failure().map(|o| o.check), Some(Check::Phases));

        let lost = BoardReadings {
            phases: PhaseReadings::NoReading,
            ..gamma()
        };
        let r = judge(Asic::Bm1373, &Limits::DEFAULT, &lost, None, None);
        let p = r.get(Check::Phases);
        assert_eq!(
            (p.status, p.note, p.value),
            (Status::Fail, Note::NoReading, None)
        );
        // No load in the sum (a chain restart, no pool work): skipped, not
        // failed, so the chips are not switched off.
        let idle = BoardReadings {
            phases: two_phases(0.3, 0.2),
            ..gamma()
        };
        let p = *judge(Asic::Bm1373, &Limits::DEFAULT, &idle, None, None).get(Check::Phases);
        assert_eq!((p.status, p.note), (Status::Skip, Note::NoWork));
        // Exactly at the floor passes; the Hex's four phases are judged alike.
        let mut amps = [1.5; crate::tps546::MAX_PHASES];
        amps[2] = 0.99;
        let hex = BoardReadings {
            phases: PhaseReadings::Read(PhaseCurrents { count: 4, amps }),
            ..gamma()
        };
        let p = *judge(Asic::Bm1370, &Limits::DEFAULT, &hex, None, None).get(Check::Phases);
        assert_eq!((p.status, p.phase), (Status::Fail, Some(2)));
        amps[2] = 1.0;
        let hex = BoardReadings {
            phases: PhaseReadings::Read(PhaseCurrents { count: 4, amps }),
            ..gamma()
        };
        let p = *judge(Asic::Bm1370, &Limits::DEFAULT, &hex, None, None).get(Check::Phases);
        assert_eq!((p.status, p.value2), (Status::Pass, Some(5.5)));
    }

    #[test]
    fn a_healthy_gamma_passes_every_applicable_check() {
        let w = window(1, 0.96, 0.2);
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&w),
            Some(&fan_ok()),
        );
        assert!(r.passed(), "{r:?}");
        assert_eq!(r.judged(), 9);
        assert_eq!(r.get(Check::Hashrate).value.map(|v| v.round()), Some(96.0));
        assert_eq!(r.get(Check::Domains).value.map(|v| v.round()), Some(96.0));
        assert_eq!(
            r.get(Check::Errors).value.map(|v| (v * 10.0).round()),
            Some(2.0)
        );
        assert_eq!(r.get(Check::CoreVoltage).value, Some(1017.0));
        assert_eq!(r.get(Check::Fan).value, Some(4800.0));
        assert_eq!(r.get(Check::Temperature).value2, Some(57.0));
        assert_eq!(r.get(Check::InputVoltage).value, Some(5.23));
    }

    /// A chip with a clock of its own (an offset from the chain's) is
    /// judged against what it hashes at that clock: 450 of the chain's
    /// 600 MHz is 75 % of the others' rate and would fail the 80 % line
    /// and, by a hair less, pass the domains' 67 % against the chain's
    /// figure; against its own it is at 100 %.
    #[test]
    fn a_chip_with_a_clock_of_its_own_is_judged_against_that_clock() {
        let expected = 1224.0; // 2040 small cores at 600 MHz
        let own = expected * 0.75;
        let fill = |w: &mut Window| {
            for _ in 0..60 {
                for c in 0..4 {
                    let ghs = if c == 1 { own } else { expected };
                    w.sample(c, ghs, 0.0, &[ghs / 4.0; 4], true);
                }
            }
        };
        let b = BoardReadings {
            chips_found: 4,
            chips_expected: 4,
            ..gamma()
        };
        // Against the chain's figure: chip 1 fails.
        let mut w = Window::new(4, 4, expected, &Limits::DEFAULT);
        fill(&mut w);
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&w),
            Some(&fan_ok()),
        );
        let h = r.get(Check::Hashrate);
        assert_eq!((h.status, h.chip), (Status::Fail, Some(1)));
        // Against its own: every check passes, at 100 %.
        let mut w = Window::new(4, 4, expected, &Limits::DEFAULT);
        w.set_chip_expected(1, own);
        w.set_chip_expected(99, 1.0); // no such chip: ignored
        fill(&mut w);
        assert_eq!(w.expected_chip_ghs(0), expected);
        assert_eq!(w.expected_chip_ghs(1), own);
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&w),
            Some(&fan_ok()),
        );
        assert!(r.passed(), "{r:?}");
        let h = r.get(Check::Hashrate);
        assert!((h.value.unwrap() - 100.0).abs() < 0.01, "{h:?}");
        let d = r.get(Check::Domains);
        assert!((d.value.unwrap() - 100.0).abs() < 0.01, "{d:?}");
        // The drop line of a domain sample follows the chip's own share:
        // 3× the slow chip's share is dropped, on a fast chip it counts.
        let mut w = Window::new(2, 4, expected, &Limits::DEFAULT);
        w.set_chip_expected(1, own);
        let spike = own / 4.0 * 3.5;
        w.sample(0, expected, 0.0, &[spike; 4], true);
        w.sample(1, own, 0.0, &[spike; 4], true);
        assert_eq!(w.domain_ghs(0, 0).1, 1);
        assert_eq!(w.domain_ghs(1, 0).1, 0);
    }

    #[test]
    fn a_dead_chip_fails_the_hashrate_check_and_names_it() {
        let expected = 1632.0;
        let mut w = Window::new(4, 4, expected, &Limits::DEFAULT);
        for _ in 0..60 {
            for c in 0..4 {
                let ghs = if c == 2 { 0.0 } else { expected };
                w.sample(c, ghs, 0.0, &[ghs / 4.0; 4], c != 2);
            }
        }
        let b = BoardReadings {
            chips_found: 4,
            chips_expected: 4,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&w),
            Some(&fan_ok()),
        );
        assert!(!r.passed());
        let h = r.get(Check::Hashrate);
        assert_eq!(
            (h.status, h.chip, h.note),
            (Status::Fail, Some(2), Note::NoReply)
        );
        assert_eq!(h.value, Some(0.0));
        // The domains of that chip are empty too; the check names the first one.
        let d = r.get(Check::Domains);
        assert_eq!(
            (d.status, d.chip, d.domain),
            (Status::Fail, Some(2), Some(0))
        );
        assert_eq!(r.failed(), 2);
    }

    #[test]
    fn a_weak_domain_fails_the_domain_check_only() {
        let expected = 1071.0;
        let mut w = Window::new(1, 4, expected, &Limits::DEFAULT);
        for _ in 0..60 {
            // Domain 3 at half its share: the chip's total still at 88 %.
            let d = [
                expected / 4.0,
                expected / 4.0,
                expected / 4.0,
                expected / 8.0,
            ];
            w.sample(0, expected * 0.875, 0.0, &d, true);
        }
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&w),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Hashrate).status, Status::Pass);
        let d = r.get(Check::Domains);
        assert_eq!(
            (d.status, d.chip, d.domain),
            (Status::Fail, Some(0), Some(3))
        );
        assert_eq!(d.value.map(|v| v.round()), Some(50.0));
        assert_eq!(r.failed(), 1);
    }

    #[test]
    fn implausible_domain_samples_are_dropped_not_averaged() {
        let expected = 1071.0;
        let mut w = Window::new(1, 4, expected, &Limits::DEFAULT);
        let share = expected / 4.0;
        for i in 0..60 {
            // Every other sample of domain 0 is garbage (10× its share).
            let d0 = if i % 2 == 0 { share * 10.0 } else { share };
            w.sample(0, expected, 0.0, &[d0, share, share, share], true);
        }
        let (ghs, n) = w.domain_ghs(0, 0);
        assert_eq!(n, 30);
        assert!((ghs - share).abs() < 0.01);
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&w),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Domains).status, Status::Pass);
        // All samples garbage: no average, a failure that says so.
        let mut w = Window::new(1, 4, expected, &Limits::DEFAULT);
        for _ in 0..60 {
            w.sample(0, expected, 0.0, &[share * 10.0, share, share, share], true);
        }
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&w),
            Some(&fan_ok()),
        );
        let d = r.get(Check::Domains);
        assert_eq!(
            (d.status, d.note, d.domain),
            (Status::Fail, Note::NoSamples, Some(0))
        );
    }

    #[test]
    fn the_error_limit_is_per_chip_type_and_the_bm1397_has_none() {
        // 6 % errors: too many for a BM1370, fine for a BM1373; the
        // Nano's 2.6 % at WarpTune's eco point passes.
        let w = window(1, 1.0, 6.0);
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&w),
            Some(&fan_ok()),
        );
        let e = r.get(Check::Errors);
        assert_eq!((e.status, e.limit), (Status::Fail, Some(5.0)));
        assert_eq!(e.value.map(|v| v.round()), Some(6.0));
        let eco = window(1, 1.0, 2.6);
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&eco),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Errors).status, Status::Pass);
        let r = judge(
            Asic::Bm1373,
            &Limits::DEFAULT,
            &gamma(),
            Some(&w),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Errors).status, Status::Pass);
        // The BM1397 reports a rate, no counters: no domains, no errors.
        let mut w = Window::new(1, 0, 201.6, &Limits::DEFAULT);
        for _ in 0..60 {
            w.sample(0, 175.0, 0.0, &[], true);
        }
        let r = judge(
            Asic::Bm1397,
            &Limits::DEFAULT,
            &gamma(),
            Some(&w),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Errors).status, Status::Skip);
        assert_eq!(r.get(Check::Errors).note, Note::NoCounter);
        assert_eq!(r.get(Check::Domains).status, Status::Skip);
        assert_eq!(r.get(Check::Domains).note, Note::NoDomains);
        assert_eq!(r.get(Check::Hashrate).status, Status::Pass);
        assert_eq!(r.get(Check::Hashrate).value.map(|v| v.round()), Some(87.0));
    }

    #[test]
    fn a_board_without_regulator_or_fan_skips_those_checks() {
        // The NerdNOS: thermistor, no regulator, no fan, no input reading.
        let b = BoardReadings {
            has_regulator: false,
            core_actual_mv: Some(1396),
            vr_temp_c: None,
            vin_v: None,
            chip_temp_c: Some(72.1),
            chip_limit_c: 80.0,
            ..gamma()
        };
        let mut w = Window::new(1, 0, 201.6, &Limits::DEFAULT);
        for _ in 0..60 {
            w.sample(0, 177.0, 0.0, &[], true);
        }
        let r = judge(Asic::Bm1397, &Limits::DEFAULT, &b, Some(&w), None);
        assert!(r.passed(), "{r:?}");
        assert_eq!(r.get(Check::CoreVoltage).note, Note::NoRegulator);
        assert_eq!(r.get(Check::Regulator).status, Status::Skip);
        assert_eq!(r.get(Check::Fan).note, Note::NoFan);
        assert_eq!(r.get(Check::InputVoltage).status, Status::Skip);
        assert_eq!(r.get(Check::Temperature).status, Status::Pass);
        assert_eq!(r.get(Check::Temperature).value2, None);
        assert_eq!(r.judged(), 3);
    }

    #[test]
    fn chips_off_is_a_failure_of_every_chip_check_not_a_skip() {
        let b = BoardReadings {
            hashing: false,
            chips_found: 0,
            ..gamma()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &b, None, Some(&fan_ok()));
        for c in [Check::Chips, Check::Hashrate, Check::Domains, Check::Errors] {
            let o = r.get(c);
            assert_eq!((c, o.status, o.note), (c, Status::Fail, Note::NotHashing));
        }
        assert_eq!(r.failed(), 4);
        // A chain with one chip short: found against expected.
        let b = BoardReadings {
            chips_found: 3,
            chips_expected: 4,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&window(3, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        let c = r.get(Check::Chips);
        assert_eq!(
            (c.status, c.value, c.limit),
            (Status::Fail, Some(3.0), Some(4.0))
        );
        // Chips off with no work either (no pool is why they are off, or
        // an overheat during an outage): still a failure, not a skip.
        let b = BoardReadings {
            hashing: false,
            has_work: false,
            chips_found: 0,
            ..gamma()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &b, None, Some(&fan_ok()));
        for c in [Check::Chips, Check::Hashrate, Check::Domains, Check::Errors] {
            let o = r.get(c);
            assert_eq!((c, o.status, o.note), (c, Status::Fail, Note::NotHashing));
        }
    }

    #[test]
    fn a_chain_that_is_up_without_work_skips_the_hash_checks_and_passes_on_the_rest() {
        // No pool answers: the chain is ready, every chip was found, but
        // there is nothing to hash and so no window.
        let b = BoardReadings {
            has_work: false,
            ..gamma()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &b, None, Some(&fan_ok()));
        for c in [Check::Hashrate, Check::Domains, Check::Errors] {
            let o = r.get(c);
            assert_eq!((c, o.status, o.note), (c, Status::Skip, Note::NoWork));
            assert_eq!((o.value, o.chip), (None, None));
        }
        // The chip count comes from the enumeration, not from hashes.
        let c = r.get(Check::Chips);
        assert_eq!((c.status, c.note), (Status::Pass, Note::None));
        // Voltages, regulator, fan and temperatures as with work.
        for c in [
            Check::CoreVoltage,
            Check::Regulator,
            Check::Fan,
            Check::Temperature,
            Check::InputVoltage,
        ] {
            assert_eq!(r.get(c).status, Status::Pass, "{c:?}");
        }
        assert!(r.passed(), "{r:?}");
        assert_eq!((r.failed(), r.judged()), (0, 6));
        // A fault elsewhere still fails the run.
        let hot = BoardReadings {
            has_work: false,
            chip_temp_c: Some(80.0),
            ..gamma()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &hot, None, Some(&fan_ok()));
        assert!(!r.passed());
        assert_eq!(r.get(Check::Temperature).status, Status::Fail);
        assert_eq!(r.get(Check::Hashrate).status, Status::Skip);
        // A chip short is found without work too.
        let short = BoardReadings {
            has_work: false,
            chips_found: 3,
            chips_expected: 4,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &short,
            None,
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Chips).status, Status::Fail);
        // With a window the window decides, work at the end or not (the
        // pool went away after the sampling).
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&window(1, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Hashrate).status, Status::Pass);
        // The BM1397 has no error counter: that skip keeps its own note.
        let r = judge(Asic::Bm1397, &Limits::DEFAULT, &b, None, Some(&fan_ok()));
        assert_eq!(r.get(Check::Errors).note, Note::NoCounter);
        assert_eq!(r.get(Check::Hashrate).note, Note::NoWork);
        // Its missing domain counters show only in a window: without
        // work the domains are skipped for want of work.
        assert_eq!(
            (r.get(Check::Domains).status, r.get(Check::Domains).note),
            (Status::Skip, Note::NoWork)
        );
    }

    #[test]
    fn core_voltage_within_five_percent_input_within_ten() {
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&window(1, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        let c = r.get(Check::CoreVoltage);
        assert_eq!(
            (c.status, c.limit, c.value2),
            (Status::Pass, Some(1020.0), Some(51.0))
        );
        let b = BoardReadings {
            core_actual_mv: Some(960),
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&window(1, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::CoreVoltage).status, Status::Fail);
        let b = BoardReadings {
            core_actual_mv: None,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&window(1, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::CoreVoltage).note, Note::NoReading);
        // A 12 V board at 10.7 V: sagging supply.
        let b = BoardReadings {
            vin_v: Some(10.7),
            nominal_v: 12.0,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&window(1, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        let v = r.get(Check::InputVoltage);
        assert_eq!((v.status, v.limit), (Status::Fail, Some(12.0)));
        assert!((v.value2.unwrap() - 1.2).abs() < 0.001);
    }

    #[test]
    fn the_fan_must_reach_the_floor_and_react_unless_it_already_ran_full() {
        let w = window(1, 1.0, 0.0);
        // Spins, but no faster at 100 % than at 36 %: the duty does nothing.
        let f = FanReadings {
            rpm_full: 2150,
            ..fan_ok()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &gamma(), Some(&w), Some(&f));
        let o = r.get(Check::Fan);
        assert_eq!((o.status, o.note), (Status::Fail, Note::NoReaction));
        // Already at 100 % before (warm-up): only the floor counts.
        let f = FanReadings {
            duty_before: 100,
            rpm_before: 4800,
            rpm_full: 4800,
            ..fan_ok()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &gamma(), Some(&w), Some(&f));
        assert_eq!(r.get(Check::Fan).status, Status::Pass);
        // Stalled.
        let f = FanReadings {
            rpm_before: 0,
            rpm_full: 0,
            ..fan_ok()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &gamma(), Some(&w), Some(&f));
        let o = r.get(Check::Fan);
        assert_eq!(
            (o.status, o.value, o.limit),
            (Status::Fail, Some(0.0), Some(500.0))
        );
    }

    #[test]
    fn an_empty_second_fan_header_is_a_note_a_slow_second_fan_a_failure() {
        let w = window(1, 1.0, 0.0);
        let f = FanReadings {
            rpm2_before: Some(0),
            rpm2_full: Some(0),
            ..fan_ok()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &gamma(), Some(&w), Some(&f));
        let o = r.get(Check::Fan);
        assert_eq!(
            (o.status, o.note, o.limit2),
            (Status::Pass, Note::SecondFanMissing, Some(0.0))
        );
        let f = FanReadings {
            rpm2_before: Some(300),
            rpm2_full: Some(320),
            ..fan_ok()
        };
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &gamma(), Some(&w), Some(&f));
        assert_eq!(r.get(Check::Fan).status, Status::Fail);
    }

    #[test]
    fn temperatures_must_be_read_and_under_their_lines() {
        let w = window(1, 1.0, 0.0);
        let b = BoardReadings {
            chip_temp_c: None,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&w),
            Some(&fan_ok()),
        );
        let o = r.get(Check::Temperature);
        assert_eq!((o.status, o.note), (Status::Fail, Note::NoReading));
        let b = BoardReadings {
            vr_temp_c: Some(106.0),
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&w),
            Some(&fan_ok()),
        );
        let o = r.get(Check::Temperature);
        assert_eq!(
            (o.status, o.note, o.value2, o.limit2),
            (Status::Fail, Note::Overheat, Some(106.0), Some(105.0))
        );
        let b = BoardReadings {
            has_thermal: false,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&w),
            Some(&fan_ok()),
        );
        assert_eq!(r.get(Check::Temperature).note, Note::NoSensor);
    }

    #[test]
    fn a_regulator_fault_fails_the_regulator_check() {
        let b = BoardReadings {
            regulator_fault: true,
            ..gamma()
        };
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &b,
            Some(&window(1, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        let o = r.get(Check::Regulator);
        assert_eq!((o.status, o.note), (Status::Fail, Note::RegulatorFault));
        assert_eq!(r.first_failure().map(|o| o.check), Some(Check::Regulator));
    }

    #[test]
    fn the_report_order_and_ids_are_stable() {
        let r = judge(
            Asic::Bm1370,
            &Limits::DEFAULT,
            &gamma(),
            Some(&window(1, 1.0, 0.0)),
            Some(&fan_ok()),
        );
        let ids: Vec<&str> = r.outcomes.iter().map(|o| o.check.id()).collect();
        assert_eq!(
            ids,
            [
                "chips",
                "hashrate",
                "domains",
                "errors",
                "coreVoltage",
                "regulator",
                "phases",
                "fan",
                "temperature",
                "inputVoltage"
            ]
        );
        assert_eq!(Status::Skip.id(), "skip");
        assert_eq!(Note::SecondFanMissing.id(), "secondFanMissing");
        assert_eq!(Note::SinglePhase.id(), "singlePhase");
    }
}

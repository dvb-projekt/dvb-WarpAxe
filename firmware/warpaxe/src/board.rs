//! The board layer on ESP-IDF: the profile from the config, the I²C
//! bus, regulator and thermal parts from `warpaxe-board`, the fan
//! regulators, and the sample the API reports. On a board without
//! regulator and sensor (the devkit) this only owns the reset pin, if
//! any, and reports nothing.
//!
//! Two families: the Bitaxe-style single-chip boards (TPS546 + EMC2101)
//! and the Nerd multi-chip boards (TPS53647/TPS53667 with enable and LDO
//! pins, TMP1075 sensors, EMC2302 with two fan outputs, on the Q13xx
//! carriers everything behind an FXL6408 expander). The Nerd power-up
//! follows NerdOS `NerdQaxePlus::initAsics`.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver, I2C0};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::{self, EspError};
use serde::Serialize;
use warpaxe_board::chip_temps::ChipTemps;
use warpaxe_board::choice::{self, FirstStart};
use warpaxe_board::core_hold::{self, CoreHold, Step};
use warpaxe_board::detect::{self, Detection};
use warpaxe_board::emc2101::Emc2101;
use warpaxe_board::emc2103::Emc2103;
use warpaxe_board::emc2302::{Emc2302, Fan};
use warpaxe_board::fan::{FanPid, Warmup, SAMPLE_MS};
use warpaxe_board::fxl6408::{q13xx, Fxl6408};
use warpaxe_board::ina260::Ina260;
use warpaxe_board::overheat::{Cause, Guard, Reading, SensorWatch};
use warpaxe_board::pca9544::Muxed;
use warpaxe_board::pmbus;
use warpaxe_board::power_fault::{self, Condition, Monitor, Verdict};
use warpaxe_board::tmp1075::{self, Addrs};
use warpaxe_board::tps53647::{self, Tps53647};
use warpaxe_board::tps546::{PhaseCurrents, Tps546};
use warpaxe_board::warm_boot;
use warpaxe_board::{Asic, I2c, Profile, Thermal as ThermalKind, Vr};
use warpaxe_core::chip::{
    bm1370::{BM1370, BM1370_NERD},
    bm1373::{BM1373, BM1373_ESP, BM1373_ESP_1M},
    bm1397::BM1397,
    ChipProfile,
};

use crate::config;

/// The regulator-temperature target of the second fan channel on the
/// Nerd boards (NerdOS `m_pidSettings[1].targetTemp`).
pub const VR_FAN_TARGET_C: f32 = 65.0;

/// The regulator temperature the fans hold on this board, °C: 65 on the
/// Nerd boards' second fan (NerdOS), 80 on the NerdAxe boards, whose one
/// fan also cools the chip — their regulator runs at 70-75 °C in normal
/// mining and only shuts down at 105 °C, so 65 held the fan at 100 %.
pub fn vr_fan_target_c(profile: &Profile) -> f32 {
    if profile.name.starts_with("nerdaxe") {
        80.0
    } else {
        VR_FAN_TARGET_C
    }
}
/// Seconds without a chip temperature reading before the core voltage
/// goes off: a chain hashing blind is treated like one overheating.
/// NerdOS has no such guard (a failed sensor reads 0 °C there).
const TEMP_LOST_S: u32 = 60;

/// What the board reports; `None` where the board has no sensor.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BoardState {
    pub board: String,
    pub has_vr: bool,
    pub has_thermal: bool,
    /// The chip temperature the fan and the stop line act on: on a
    /// board with a sensor per chip the hottest (`chip_temps`).
    pub temp_c: Option<f32>,
    /// Each chip's own temperature on a board with two chip diodes (the
    /// Naja Duo, the Gamma Turbo), chip 1 first; `None` for a sensor
    /// without a reading. Empty on every other board.
    pub chip_temps: Vec<Option<f32>>,
    pub vr_temp_c: Option<i32>,
    pub vin_mv: Option<f32>,
    pub vout_mv: Option<f32>,
    /// Output current on the Bitaxe boards, input current on the Nerd
    /// boards — what AxeOS and NerdOS each report as `current`.
    pub iout_ma: Option<f32>,
    pub power_w: Option<f32>,
    /// `power_w` is the profile's estimate, not a measurement (the
    /// NerdNOS: nothing on the board reads power).
    pub power_estimated: bool,
    /// The regulator's output current (IOUT), A, on every board.
    pub core_current_a: Option<f32>,
    /// Its over-current warning limit as written at init, A.
    pub core_current_warn_a: Option<f32>,
    pub fan_pct: u8,
    pub fan_rpm: u16,
    /// The second fan output (Nerd boards: M1, on the regulator PID).
    pub fan2_pct: Option<u8>,
    pub fan2_rpm: Option<u16>,
    pub fan_auto: bool,
    pub vr_status: u16,
    /// Regulator population found at boot (Nerd boards).
    pub vr_part: String,
    /// Mining stopped and the regulator switched off; starts again on its
    /// own once cooled (see `warpaxe_board::overheat`).
    pub overheat: bool,
    /// Too many overheat stops in a row: stays off until cleared by hand.
    pub overheat_latched: bool,
    /// Overheat stops within the last hour (the third latches).
    pub overheat_trips: u8,
    /// Why the running stop happened: "hot" or "sensor"; `None` when
    /// running.
    pub overheat_cause: Option<&'static str>,
    /// Chips switched off by hand.
    pub shutdown: bool,
    /// Fans held at full while the chips warm up after the core rail
    /// came on (see `warpaxe_board::fan::Warmup`).
    pub fan_warmup: bool,
    /// The regulator's fault or warning text (AxeOS `power_fault`, its
    /// wording); `None` when the status is clear.
    pub power_fault: Option<String>,
    /// The conditions behind it as codes: off, vout_ov, iout_oc, vin_uv,
    /// temp, temp_warn.
    pub power_fault_codes: Vec<&'static str>,
    /// A requested core voltage the regulator refused, mV (outside its
    /// window, or the bus failed); `None` once a request went through.
    /// The board task keeps trying it.
    pub core_mv_refused: Option<u16>,
    pub last_error: String,
    /// What the regulator answered to each probe at boot.
    pub vr_probe: String,
}

static STATE: Mutex<Option<BoardState>> = Mutex::new(None);
static OVERHEAT: AtomicBool = AtomicBool::new(false);
/// The BM1373's inits as a ladder, best first: ESP-Miner's at 3 MBaud
/// (rung 0, the Naja Duo's maker), the same registers at NerdOS's late
/// 1 MBaud switch (rung 1, for a Naja Duo whose level shifters do not
/// carry 3 MBaud), NerdOS's (rung 2, the Gaia's maker). A board starts
/// at its own rung ([`select_chip_init`]: the Naja Duo at 0, every
/// other BM1373 board at 2) unless the `chipInit` setting names one; a
/// chain that does not come up moves one rung down until the next boot
/// ([`chip_init_fall_back`]). The Naja Duo's copies carry its maker's
/// small-core count (`Profile::small_cores`).
const RUNG_ESP: u8 = 0;
const RUNG_ESP_1M: u8 = 1;
const RUNG_NERDOS: u8 = 2;
static BM1373_INITS: [ChipProfile; 3] = [BM1373_ESP, BM1373_ESP_1M, BM1373];
static NAJA_DUO_INITS: [ChipProfile; 3] = [
    BM1373_ESP.with_small_cores(warpaxe_board::profile::BITAXE_NAJA_DUO.small_cores),
    BM1373_ESP_1M.with_small_cores(warpaxe_board::profile::BITAXE_NAJA_DUO.small_cores),
    BM1373.with_small_cores(warpaxe_board::profile::BITAXE_NAJA_DUO.small_cores),
];
/// The rung the setting or the board starts at; `u8::MAX` until the
/// first chain start read the setting (the board's rung meanwhile).
static CHIP_INIT_START: AtomicU8 = AtomicU8::new(u8::MAX);
/// The rung the fallbacks have reached this boot; 0 = none.
static CHIP_INIT_FALLEN: AtomicU8 = AtomicU8::new(0);
/// The self-test's phase read ([`phase_currents`]): asked for by the
/// self-test thread, done by the board task, whose regulator it is.
static PHASE_READ: Mutex<PhaseRead> = Mutex::new(PhaseRead::Idle);

/// Where a phase read stands.
enum PhaseRead {
    Idle,
    Wanted,
    Done(Result<PhaseCurrents, String>),
}
/// When the chips may run again after an overheat stop.
static GUARD: Mutex<Guard> = Mutex::new(Guard::new());
/// Chips switched off by hand (web UI / API), not by a limit.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// The core rail is up; the miner starts the chain only then.
static POWERED: AtomicBool = AtomicBool::new(true);
/// Counts the chips' starts: the rail back on, the reset pulse's two
/// edges and the chain's answer ([`chips_started`]). The board loop
/// starts the second diode's settle window over at each (`chip_temps`).
static CHIP_STARTS: AtomicU32 = AtomicU32::new(0);
/// The chain has answered since the chips last came out of reset or
/// the rail last came on. Until then a two-chip board's diodes are no
/// reading (`chips_answered`).
static CHIPS_ANSWERED: AtomicBool = AtomicBool::new(false);
/// Set by an overheat reset: the board loop forgets its sensor-loss count.
static CLEAR_LOST: AtomicBool = AtomicBool::new(false);
/// Set by [`prepare_reset`]: the fans at full until the reset.
static FANS_FULL: AtomicBool = AtomicBool::new(false);
/// The board thread runs, so it is the one switching the rail.
static BOARD_THREAD: AtomicBool = AtomicBool::new(false);
/// The lowest clock the API takes and a stored config may hold, MHz:
/// the bring-up ramp starts at 56.25 MHz, and nothing is to be gained
/// below it.
pub const MIN_FREQUENCY_MHZ: u32 = 50;

/// No pool answers (`stratum.rs`, `warpaxe_stratum::outage`): the chips
/// are off until one does. Its own flag, not [`SHUTDOWN`]: `shutdown` in
/// the API stays the switch a person set, and the two do not undo each
/// other.
static NO_POOL: AtomicBool = AtomicBool::new(false);
/// Set by [`cycle_chips`]: the shutdown in force is the board's own, to
/// be lifted once the rails have been down for [`warm_boot::WARM_OFF_MS`].
/// A shutdown asked for over the API meanwhile clears it and stands.
static CYCLE: AtomicBool = AtomicBool::new(false);
/// The self-test's fan step: both outputs at least this duty until the
/// deadline, then the PID again. Only ever raises the duty.
static FAN_BOOST: Mutex<Option<(u8, Instant)>> = Mutex::new(None);
/// Core voltage the API asked for, mV; 0 = nothing pending.
static WANTED_MV: AtomicU16 = AtomicU16::new(0);
static RESET: Mutex<Option<Pin>> = Mutex::new(None);
/// When the Nerd carriers' enable and LDO lines were first driven low
/// this boot (`rail_off` at boot, or `start_nerd` for pins behind the
/// expander): the bring-up counts its pause from here
/// (`warm_boot::settle_ms`).
static RAILS_LOW_SINCE: Mutex<Option<Instant>> = Mutex::new(None);
/// The chip power-enable line of the Bitaxe Color boards, with the level
/// that enables (ESP-Miner `asic_enable`).
static ASIC_ENABLE: Mutex<Option<(Pin, bool)>> = Mutex::new(None);

/// Drive the chip power-enable line, if the board has one; enabling
/// waits the 20 ms ESP-Miner gives it before the regulator goes on.
fn asic_power(on: bool) {
    if let Some((pin, active_high)) = ASIC_ENABLE.lock().unwrap().as_ref() {
        pin.set(if on { *active_high } else { !*active_high });
        if on {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
static PROFILE: Mutex<Option<&'static Profile>> = Mutex::new(None);

pub fn state() -> BoardState {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

fn update(f: impl FnOnce(&mut BoardState)) {
    let mut g = STATE.lock().unwrap();
    f(g.get_or_insert_with(BoardState::default));
}

/// The board is a T-Display-S3 without chips (profile `t-display-s3`):
/// the ESP32-S3 hashes by itself (`cpuminer`). Everything that treats a
/// board differently without a chain keys on this, never on a feature.
pub fn devkit() -> bool {
    profile().asic_count == 0 && profile().name != "undetected"
}

/// The devkit has no I²C bus, so [`start`] returns before the sensor
/// loop: the CPU miner sets the ESP's own die temperature as the chip
/// temperature for the display and the API.
pub(crate) fn set_devkit_temp(temp_c: Option<f32>) {
    update(|s| s.temp_c = temp_c);
}

/// The devkit's own overheat guard (the CPU miner stops at the profile's
/// `chip_stop_c`, resumes 10 °C below): the same flag the sensor loop
/// raises on a board with chips, so the API and the display say so.
pub(crate) fn set_devkit_overheat(on: bool) {
    OVERHEAT.store(on, Ordering::Relaxed);
}

pub fn profile() -> &'static Profile {
    PROFILE
        .lock()
        .unwrap()
        .unwrap_or(&warpaxe_board::T_DISPLAY_S3_DEVKIT)
}

/// The chip profile the board's chain is built from. A BM1370 on a Nerd
/// carrier (the TPS5364x boards and the Rev 7 NerdQAxe++) gets NerdOS's
/// init, the firmware those boards ship with; a Bitaxe board (and the
/// NerdAxe Gamma, which ESP-Miner's TPS546 profile covers) gets
/// ESP-Miner's. See `warpaxe_core::chip::bm1370`.
pub fn chip() -> &'static ChipProfile {
    match profile().asic {
        Asic::Bm1370 => match profile().vr {
            Vr::Tps5364x { .. } | Vr::Tps546Nerd { .. } => &BM1370_NERD,
            _ => &BM1370,
        },
        // The rung of the BM1373 ladder the board, the setting and the
        // fallbacks have arrived at (`BM1373_INITS`).
        Asic::Bm1373 => &bm1373_ladder()[usize::from(chip_init_rung())],
        Asic::Bm1397 => &BM1397,
    }
}

/// The BM1373 inits this board may start with, best first.
fn bm1373_ladder() -> &'static [ChipProfile; 3] {
    if is_naja_duo() {
        &NAJA_DUO_INITS
    } else {
        &BM1373_INITS
    }
}

fn is_naja_duo() -> bool {
    profile().name == warpaxe_board::profile::BITAXE_NAJA_DUO.name
}

/// The rung this board starts at with no setting: its maker's init.
fn board_init_rung() -> u8 {
    if is_naja_duo() {
        RUNG_ESP
    } else {
        RUNG_NERDOS
    }
}

/// The rung the setting (else the board) starts at.
fn chip_init_start() -> u8 {
    match CHIP_INIT_START.load(Ordering::Relaxed) {
        u8::MAX => board_init_rung(),
        r => r,
    }
}

/// The rung in force: the start, or lower where a fallback moved it.
fn chip_init_rung() -> u8 {
    chip_init_start()
        .max(CHIP_INIT_FALLEN.load(Ordering::Relaxed))
        .min(RUNG_NERDOS)
}

/// Read the `chipInit` setting at a chain start: `espminer` (3 MBaud),
/// `espminer-1m`, `nerdos`, or anything else (the default, empty) for
/// the board's own init. A setting that names another rung than before
/// forgets the fallbacks (as a changed setting is a fresh try); the
/// same one keeps them until the next boot. BM1373 only.
pub fn select_chip_init(setting: &str) {
    if profile().asic != Asic::Bm1373 {
        return;
    }
    let rung = match setting {
        "espminer" => RUNG_ESP,
        "espminer-1m" => RUNG_ESP_1M,
        "nerdos" => RUNG_NERDOS,
        "" | "auto" | "board" => board_init_rung(),
        other => {
            log::warn!("board: chipInit {other:?} is not a known init, using the board's");
            board_init_rung()
        }
    };
    let before = CHIP_INIT_START.swap(rung, Ordering::Relaxed);
    if before != u8::MAX && before != rung {
        CHIP_INIT_FALLEN.store(0, Ordering::Relaxed);
    }
    if is_naja_duo() && chip().small_cores != profile().small_cores {
        log::warn!(
            "board: the chip profile counts {} small cores, the board {}: the board's copy is missing",
            chip().small_cores,
            profile().small_cores
        );
    }
}

/// The init the next chain start will use, for the API and the log:
/// `NerdOS`, `ESP-Miner`, `ESP-Miner 1 MBaud`, with `(fallback from …)`
/// where a failed start moved it down the ladder.
pub fn chip_init_name() -> String {
    let c = chip();
    if profile().asic != Asic::Bm1373 {
        return c.init_source.to_string();
    }
    let start = chip_init_start();
    if chip_init_rung() > start {
        let from = &bm1373_ladder()[usize::from(start)];
        format!("{} (fallback from {})", c.init_source, from.init_source)
    } else {
        c.init_source.to_string()
    }
}

/// The chain did not come up with the init in force: one rung down the
/// ladder from the next start on, until the next boot. The init left
/// and the one coming, or `None` when there is no rung below (NerdOS's
/// init, or a chip without a ladder).
pub fn chip_init_fall_back() -> Option<(&'static str, &'static str)> {
    if profile().asic != Asic::Bm1373 {
        return None;
    }
    let now = chip_init_rung();
    if now >= RUNG_NERDOS {
        return None;
    }
    CHIP_INIT_FALLEN.store(now + 1, Ordering::Relaxed);
    let ladder = bm1373_ladder();
    Some((
        ladder[usize::from(now)].init_source,
        ladder[usize::from(now + 1)].init_source,
    ))
}

/// The self-test's phase check applies to this board: a TPS546 run as
/// a stack whose config asks for it (`tps546::Config::phase_check`,
/// Naja Duo and Gamma Hex).
pub fn checks_phases() -> bool {
    match profile().vr {
        Vr::Tps546(cfg) => {
            cfg.phase_check && cfg.write_phase_config && (cfg.stack_config & 0x07) > 0
        }
        _ => false,
    }
}

/// Each phase's current from the board task's regulator
/// (`Tps546::phase_currents`): asked for here, read on the board task's
/// next pass, waited for up to `timeout`. `None` when the board task did
/// not get to it in time; `Some(Err(_))` with the bus error and the
/// PMBus command that failed.
pub fn phase_currents(timeout: Duration) -> Option<Result<PhaseCurrents, String>> {
    *PHASE_READ.lock().unwrap() = PhaseRead::Wanted;
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
        let mut g = PHASE_READ.lock().unwrap();
        if let PhaseRead::Done(_) = &*g {
            if let PhaseRead::Done(r) = std::mem::replace(&mut *g, PhaseRead::Idle) {
                return Some(r);
            }
        }
    }
    *PHASE_READ.lock().unwrap() = PhaseRead::Idle;
    None
}

fn phase_read_wanted() -> bool {
    matches!(*PHASE_READ.lock().unwrap(), PhaseRead::Wanted)
}

/// True once a temperature limit was crossed, until the board has cooled
/// down (or the stop is cleared by hand).
/// Hold both fan outputs at least at `pct` for `for_ms` (0 = release).
/// The board task applies it on its next pass and drops it by itself
/// when the time is up, so a caller that dies cannot leave it on.
pub fn fan_boost(pct: u8, for_ms: u64) {
    *FAN_BOOST.lock().unwrap() =
        (pct > 0).then(|| (pct.min(100), Instant::now() + Duration::from_millis(for_ms)));
}

/// The fan boost in force right now, if any.
fn fan_boost_now() -> Option<u8> {
    let mut g = FAN_BOOST.lock().unwrap();
    match *g {
        Some((p, until)) if Instant::now() < until => Some(p),
        Some(_) => {
            *g = None;
            None
        }
        None => None,
    }
}

/// Chips there should be: the config's count, else the profile's (0 on
/// a board without a chain).
pub fn chips_expected() -> u8 {
    let n = config::get().asic_count;
    if n > 0 {
        n
    } else {
        profile().asic_count
    }
}

pub fn overheat() -> bool {
    OVERHEAT.load(Ordering::Relaxed)
}

/// Chips switched off by hand, or held off because the chosen board was
/// refused ([`choice_blocked`]: nothing but a restart or another choice
/// lets them on again).
pub fn shutdown() -> bool {
    SHUTDOWN.load(Ordering::Relaxed) || choice_blocked()
}

/// Where the board choice stands. `None` while no board is chosen (the
/// first start's choice state, an unsupported board).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Choice {
    /// Its chips have been counted on this board.
    Verified,
    /// Fits the bus; the chain comes up at the cautious voltage and the
    /// enumeration decides.
    Pending,
    /// Does not fit what answered on the bus: nothing was switched on.
    Rejected,
    /// The chips did not answer as the profile says: switched off again.
    Mismatch,
}

/// `choice()` as a number (read in the miner's loop and under the state
/// lock, so no mutex): 0 none, then the variants in order.
static CHOICE: AtomicU8 = AtomicU8::new(0);
/// The core voltage of a pending first start, mV (0 = the profile's own).
static CAUTIOUS_MV: AtomicU16 = AtomicU16::new(0);

pub fn choice() -> Option<Choice> {
    match CHOICE.load(Ordering::Relaxed) {
        1 => Some(Choice::Verified),
        2 => Some(Choice::Pending),
        3 => Some(Choice::Rejected),
        4 => Some(Choice::Mismatch),
        _ => None,
    }
}

fn set_choice(c: Option<Choice>) {
    CHOICE.store(
        match c {
            None => 0,
            Some(Choice::Verified) => 1,
            Some(Choice::Pending) => 2,
            Some(Choice::Rejected) => 3,
            Some(Choice::Mismatch) => 4,
        },
        Ordering::Relaxed,
    );
    let mut d = DETECT.lock().unwrap();
    if let Some(d) = d.as_mut() {
        d.choice = c;
    }
}

/// The chip counts of the other boards that fit what the probe found:
/// a first start that counts exactly one of these is the other board.
pub fn sibling_counts() -> Vec<u8> {
    let found = FOUND.lock().unwrap().clone().unwrap_or_default();
    let me = profile().name;
    choice::candidates(&found.evidence())
        .filter(|c| c.name != me)
        .map(|c| c.asic_count)
        .collect()
}

/// The chain is on its first start under this board: counted, not yet
/// verified.
pub fn choice_pending() -> bool {
    choice() == Some(Choice::Pending)
}

/// The chosen board was refused: the chips stay off whatever
/// `/api/v1/chips/on` says, until a restart or another choice.
pub fn choice_blocked() -> bool {
    matches!(choice(), Some(Choice::Rejected | Choice::Mismatch))
}

/// The core voltage the chain is brought up with: the profile's bring-up
/// voltage (or the configured point, if higher); on a first start the
/// cautious one from `warpaxe_board::choice::cautious_core_mv`, which is
/// never above the bring-up voltage of a sibling the bus cannot tell this
/// board from.
pub fn bring_up_mv(profile: &Profile, configured_mv: u16) -> u16 {
    let normal = profile.init_core_mv.max(configured_mv);
    match (choice(), CAUTIOUS_MV.load(Ordering::Relaxed)) {
        (Some(Choice::Pending), mv) if mv > 0 => mv.min(normal),
        _ => normal,
    }
}

/// The enumeration matched the profile (or fell short by a count that is
/// no sibling's: chips missing, not another board): the board is
/// verified, kept in NVS so later boots start as usual. The core voltage
/// goes from the cautious value to the configured point once the chain
/// init is over (`chain_ready`, as NerdOS sets its final voltage after
/// the init). Called from the miner thread (internal stack: the NVS
/// write is allowed there).
pub fn verify_choice(chips: u8, expected: u8) {
    let p = profile();
    if let Err(e) = config::set_verified_board(p.name) {
        log::warn!("board: saving the verified board failed: {e}");
    }
    set_choice(Some(Choice::Verified));
    if chips < expected {
        log::warn!(
            "board: {} verified with {chips} of {expected} chips: no other board has that count; check the chain",
            p.name
        );
        crate::events::record(
            "board",
            warpaxe_diag::events::detail::verified(p.name, chips, expected),
        );
    } else {
        log::info!(
            "board: {} verified: {chips} chip(s) answered as the profile says",
            p.name
        );
        crate::events::record(
            "board",
            warpaxe_diag::events::detail::verified(p.name, chips, expected),
        );
    }
}

/// The enumeration did not match the profile: the chips go off again
/// (the board loop switches the rail off and holds the reset) and the
/// web UI says what answered, with the boards that fit. Nothing is
/// saved; the next restart tries the same choice once more, another
/// choice goes through the bus check first.
pub fn reject_choice(verdict: FirstStart) {
    let p = profile();
    let (kind, text, event, found, expected, chip_id) = match verdict {
        FirstStart::Verified { .. } | FirstStart::ShortCount { .. } => return,
        FirstStart::WrongCount { found, expected } => (
            "count",
            format!("{found} chip(s) answered, a {} has {expected}", p.model),
            warpaxe_diag::events::detail::refused_count(p.name, found, expected),
            Some(found),
            Some(expected),
            None,
        ),
        FirstStart::WrongChip { chip_id } => (
            "chip",
            format!("a chip answered as {chip_id:#06x}, not a {}", chip().name),
            warpaxe_diag::events::detail::refused_chip(p.name, chip_id, chip().name),
            None,
            None,
            Some(chip_id),
        ),
        FirstStart::NoChips => (
            "none",
            "no chip answered in three attempts".to_string(),
            warpaxe_diag::events::detail::refused_none(p.name),
            Some(0),
            Some(chips_expected()),
            None,
        ),
    };
    set_choice(Some(Choice::Mismatch));
    log::error!(
        "board: {} refused at its first start: {text}; chips off until another board is chosen or the miner restarts",
        p.name
    );
    crate::events::record("board", event);
    let found_bus = FOUND.lock().unwrap().clone().unwrap_or_default();
    let candidates: Vec<&'static str> = choice::candidates(&found_bus.evidence())
        .map(|c| c.name)
        .collect();
    let mut d = DETECT.lock().unwrap();
    let info = d.get_or_insert_with(|| DetectInfo {
        status: "mismatch",
        why: "",
        candidates: Vec::new(),
        imported: Vec::new(),
        model: None,
        chosen: Some(p.name),
        choice: Some(Choice::Mismatch),
        reason: None,
        detail: None,
        cautious_mv: 0,
        bus: found_bus.json(),
    });
    info.status = "mismatch";
    info.why = "the chips did not answer as the chosen board's profile says";
    info.chosen = Some(p.name);
    info.choice = Some(Choice::Mismatch);
    info.candidates = candidates;
    info.detail = Some(serde_json::json!({
        "kind": kind, "found": found, "expected": expected,
        "chipId": chip_id.map(|c| format!("{c:#06x}")),
        "chip": chip().name,
    }));
}

/// The chain must not run: overheat, a manual shutdown, or the core rail
/// not (yet) back up.
pub fn chain_stopped() -> bool {
    overheat() || shutdown() || no_pool() || !POWERED.load(Ordering::Relaxed)
}

/// The chips are (to be) off because no pool answers. Not during a
/// pending first start: the chain has to come up once for its
/// enumeration to judge the chosen board, pool or no pool. Not while a
/// self-test runs or the one after a verified first start is due: the
/// chips stay on for it (and come on for one started while they were
/// off; without work its hash checks are skipped and the board's
/// run). Not on a board without chips (the T-Display-S3, an undetected
/// board): nothing is off there.
pub fn no_pool() -> bool {
    NO_POOL.load(Ordering::Relaxed)
        && !choice_pending()
        && profile().asic_count > 0
        && !crate::selftest::running()
        && !crate::miner::selftest_due()
}

/// No pool answers (`true`: core rail down, chain in reset, fans low) or
/// one does again (`false`: the board loop brings the rail back at the
/// voltage it had, the miner starts the chain over at the configured
/// clock). Atomics only: callable from any thread.
pub fn set_no_pool(off: bool) {
    NO_POOL.store(off, Ordering::Relaxed);
}

/// Boot, before anything slow: the chain in reset and the core rail off.
/// An ESP reset leaves the board as it was: the TPS546 keeps its
/// OPERATION through a software reset and in ROM download mode, the reset
/// line floats, and the chips sit powered at their last clock with
/// nothing watching them until [`start`] runs, seconds after USB and
/// Wi-Fi (BitForge Nano 2026-10-03: 82 °C chip on the 0x24 diode scale
/// of the time, 95 °C regulator, 35 W at uptime 5 s after three minutes
/// of download mode). ESP-Miner holds the
/// reset low first thing in app_main "to minimize ASIC power
/// consumption"; NerdOS's initBoard drives enable, LDO and reset low.
pub fn quiesce() {
    let profile = profile();
    if let Some(p) = profile.asic_reset {
        let pin = Pin::Esp(p);
        pin.output(false);
        *RESET.lock().unwrap() = Some(pin);
    }
    if let Some(en) = profile.asic_enable {
        Pin::Esp(en.pin).output(!en.active_high);
    }
    rail_off("boot");
}

/// The core rail off by the shortest path, without the board thread: the
/// TPS546 by OPERATION (as the first-start probe does for a rail the stock
/// firmware left on), the Nerd regulators by their enable line. Pins
/// behind the FXL6408 expander are left to [`start`], which resets the
/// expander first; the Gaia's LDO stays up (cutting it breaks the chip's
/// enumeration, see `start_tps546_nerd`).
fn rail_off(why: &str) {
    let profile = profile();
    match profile.vr {
        Vr::Tps5364x {
            enable,
            ldo_enable,
            via_expander,
            ..
        } => {
            if !via_expander {
                Pin::Esp(enable).output(false);
                if let Some(l) = ldo_enable {
                    Pin::Esp(l).output(false);
                }
                rails_low_now();
            }
        }
        Vr::Tps546Nerd {
            enable: Some(en), ..
        } => {
            Pin::Esp(en).output(false);
        }
        _ => {}
    }
    let Some(cfg) = profile.tps546() else {
        return;
    };
    let Some(bus) = i2c() else {
        return;
    };
    let mut vr = Tps546::new(bus, cfg);
    let was_on = vr.is_on();
    match (was_on, vr.off()) {
        (Ok(true), Ok(())) => log::warn!("board: {why}: the core rail was on, switched off"),
        (_, Ok(())) => log::info!("board: {why}: core rail off"),
        (_, Err(e)) => log::error!("board: {why}: switching the core rail off failed: {e:?}"),
    }
}

/// Before an intentional reset (restart, update, download mode): the
/// chain stopped and held in reset, the core rail off, the fans at full
/// for whatever follows (ROM download mode runs nothing, and the fan
/// controllers keep their last duty). NerdOS does the same in
/// PowerManagementTask::restart ("shutdown asics and LDOs before reset":
/// core off, 500 ms, LDO off, 500 ms, `esp_restart`); ESP-Miner restarts
/// with the rail on. Returns once the board thread reports the rail off
/// (on the Nerd carriers that includes the LDO, `Regulator::off`), or
/// after about two seconds, and NerdOS's half second after that.
pub fn prepare_reset(why: &str) {
    log::warn!("board: {why}: chips off before the reset");
    // A chips off/on cycle under way must not lift the shutdown while
    // the reset is being prepared (and leave the chips powered in ROM
    // download mode, the Nano's 82 °C case of 2026-10-03).
    CYCLE.store(false, Ordering::Relaxed);
    FANS_FULL.store(true, Ordering::Relaxed);
    SHUTDOWN.store(true, Ordering::Relaxed);
    if BOARD_THREAD.load(Ordering::Relaxed) {
        // Switched off on the thread's next pass; the fans follow a pass
        // later.
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            if !POWERED.load(Ordering::Relaxed) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2 * u64::from(SAMPLE_MS)));
    }
    if let Some(pin) = RESET.lock().unwrap().as_ref() {
        pin.set(false);
    }
    asic_power(false);
    rail_off(why);
    std::thread::sleep(Duration::from_millis(u64::from(
        warm_boot::LDO_OFF_BEFORE_RESET_MS,
    )));
}

/// The chips' I/O rails go away when they are switched off (the Nerd
/// carriers: `Regulator::off` cuts the LDO), so the miner parks the
/// chain's TX low meanwhile (`ChipUart::park`).
pub fn parks_uart() -> bool {
    matches!(
        profile().vr,
        Vr::Tps5364x {
            ldo_enable: Some(_),
            ..
        }
    )
}

/// Enable and LDO are low from now, if they were not already.
fn rails_low_now() {
    RAILS_LOW_SINCE
        .lock()
        .unwrap()
        .get_or_insert_with(Instant::now);
}

/// How long enable and LDO have been low, ms; 0 if never driven.
fn rails_low_for_ms() -> u32 {
    RAILS_LOW_SINCE.lock().unwrap().map_or(0, |t| {
        t.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
    })
}

/// Switch the chips off (core rail down) or back on; the board loop does
/// the switching, the miner restarts the chain once the rail is up.
pub fn set_shutdown(off: bool) {
    CYCLE.store(false, Ordering::Relaxed);
    SHUTDOWN.store(off, Ordering::Relaxed);
    log::warn!(
        "board: chips switched {} by request",
        if off { "off" } else { "on" }
    );
}

/// A [`cycle_chips`] is under way: the chips are off for a few seconds
/// and come back on their own.
pub fn cycling() -> bool {
    CYCLE.load(Ordering::Relaxed)
}

/// Chips off and back on, the way `/api/v1/chips/off` and `/on` do it,
/// in one go: the board loop takes the rail down on its next pass
/// (`Regulator::off`: chain into reset, core off, 500 ms, LDO off on the
/// TPS5364x Nerd carriers; the Gaia's LDO stays up, see
/// `start_tps546_nerd`), holds it down for [`warm_boot::WARM_OFF_MS`]
/// (the five seconds the warm-reboot work settled on, docs/04), and
/// lifts the shutdown; the miner then pulses the reset and brings the
/// chain up afresh, as after a chain restart. For a hash domain stuck
/// mid-run (`miner.rs`, domain latch) this is the agreed intervention
/// (concept 2026-11-04): the strongest re-init the firmware has short
/// of a new clock or voltage, a superset of the chain restart the
/// silent-chip case uses (reset pulse and init with the rails up), at
/// about six seconds' cost. Whether the rails matter for a mid-run
/// latch is not known: docs/04 found the warm-reboot latch was the
/// clock-delay trim, and a clock dip brought a dead domain back without
/// a power cycle. In the field the reset has taken twice on a Bitaxe
/// Gamma (v0.1.41, a domain at 0 % from the chain's start). Clock and voltage
/// stay as they are; nothing is written to NVS. Returns `false` without
/// the board thread (nothing would switch the rail back on), or while
/// the chips are held off for another reason.
pub fn cycle_chips(why: &str) -> bool {
    if !BOARD_THREAD.load(Ordering::Relaxed) || chain_stopped() {
        return false;
    }
    log::warn!("board: {why}: chips off for {} ms, then back on", warm_boot::WARM_OFF_MS);
    CYCLE.store(true, Ordering::Relaxed);
    SHUTDOWN.store(true, Ordering::Relaxed);
    true
}

/// Clear an overheat without a reboot. If the limit is still crossed, the
/// next sample trips it again.
pub fn reset_overheat() {
    CLEAR_LOST.store(true, Ordering::Relaxed);
    GUARD.lock().unwrap().clear();
    OVERHEAT.store(false, Ordering::Relaxed);
    update(|s| {
        if s.last_error.starts_with("temp:") {
            s.last_error.clear();
        }
    });
    log::warn!("board: overheat cleared by request");
}

/// A new core voltage from `PATCH /api/system`, applied by the board task.
pub fn request_core_mv(mv: u16) {
    // During a pending first start nothing goes above the cautious
    // voltage (a PATCH meanwhile is applied once the choice is verified:
    // chain_ready requests the configured point).
    let cap = CAUTIOUS_MV.load(Ordering::Relaxed);
    let mv = if choice_pending() && cap > 0 {
        mv.min(cap)
    } else {
        mv
    };
    // Released after the clock that came with it (`api.rs` asks for
    // the clock first), acquired by the board task before it reads the
    // clock: it never sees the new voltage with the old clock request.
    WANTED_MV.store(mv, Ordering::Release);
}

/// The chain init is over, answered or not: drop from the bring-up
/// voltage to the configured one (NerdOS sets the final voltage after the
/// chain init). Called on a failed init too — a chain that never answered
/// must not sit at the bring-up voltage (1000 mV on a Gaia, above its
/// 970 mV cap) while the miner retries.
pub fn chain_ready() {
    // On a first start the cautious voltage holds until the enumeration
    // is judged; once verified, the configured point applies from here.
    if choice_pending() {
        return;
    }
    let core = config::get().core_voltage;
    if profile().init_core_mv > core || CAUTIOUS_MV.swap(0, Ordering::Relaxed) > 0 {
        request_core_mv(core);
    }
}

/// The chips have just come out of reset or answered on the chain: a
/// second chip's diode reading settles from here (`chip_temps`: the
/// sensor's average still holds what it read while the chip was off).
pub fn chips_started() {
    CHIP_STARTS.fetch_add(1, Ordering::Relaxed);
}

/// The chain answered (enumerated): from here a two-chip board's diodes
/// read their chips. Before it, with the rail off or the chips in reset,
/// the EMC2103 reads them as nonsense: a Gamma Turbo's first diode read
/// 72.75 °C eleven seconds after a power-on and stopped the start over
/// a 70 °C line, then 33.6 °C once its chip ran (John11, 2026-11-12);
/// the second reads the top of the scale (`chip_temps`).
pub fn chips_answered() {
    CHIPS_ANSWERED.store(true, Ordering::Relaxed);
}

/// Pull the chain's reset low, then release it, with the given timing.
/// Nothing happens on a board without a reset line. On a board with a
/// `stock_reset_pulse` line (the NerdNOS's GPIO12) that line goes high
/// for `low_ms` and back low once the reset is released, as the stock
/// firmware does it (WantClue: PEN high, then `_reset`: RST high 100 ms,
/// low 100 ms, then the init).
pub fn reset_chain(low_ms: u32, release_ms: u32) {
    // Before the pulse and at its release: a sample that falls inside
    // the pulse is in the window too.
    CHIPS_ANSWERED.store(false, Ordering::Relaxed);
    chips_started();
    if let Some(pin) = RESET.lock().unwrap().as_ref() {
        pin.set(false);
        std::thread::sleep(Duration::from_millis(u64::from(low_ms)));
        pin.set(true);
        chips_started();
        if let Some(p) = profile().stock_reset_pulse {
            let aux = Pin::Esp(p);
            aux.output(true);
            std::thread::sleep(Duration::from_millis(u64::from(low_ms)));
            aux.set(false);
        }
        std::thread::sleep(Duration::from_millis(u64::from(release_ms)));
    }
}

/// The bus shared by regulator, sensors, expander and display.
#[derive(Clone)]
pub struct SharedI2c(Arc<Mutex<I2cDriver<'static>>>);

static BUS: Mutex<Option<SharedI2c>> = Mutex::new(None);

/// The board's I²C bus, once [`init_bus`] created it.
pub fn i2c() -> Option<SharedI2c> {
    BUS.lock().unwrap().clone()
}

/// ESP-Miner's policy: three tries, 10 ms apart.
fn retry(mut f: impl FnMut() -> Result<(), EspError>) -> Result<(), EspError> {
    let mut last = Ok(());
    for i in 0..3 {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
        last = f();
        if last.is_ok() {
            return last;
        }
    }
    last
}

impl I2c for SharedI2c {
    type Error = EspError;
    fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), EspError> {
        retry(|| self.0.lock().unwrap().write(addr, bytes, BLOCK))
    }
    fn write_read(&mut self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), EspError> {
        retry(|| self.0.lock().unwrap().write_read(addr, wr, rd, BLOCK))
    }
    fn read(&mut self, addr: u8, rd: &mut [u8]) -> Result<(), EspError> {
        retry(|| self.0.lock().unwrap().read(addr, rd, BLOCK))
    }
}

// ESP GPIOs by number, chosen by the profile at run time (the display
// does the same). The level is set before the direction, so an enable
// line never drives high for an instant.
fn esp_output(pin: u8, level: bool) {
    let pin = i32::from(pin);
    unsafe {
        sys::gpio_set_level(pin, u32::from(level));
        sys::gpio_set_direction(pin, sys::gpio_mode_t_GPIO_MODE_OUTPUT);
    }
}

fn esp_set(pin: u8, level: bool) {
    unsafe {
        sys::gpio_set_level(i32::from(pin), u32::from(level));
    }
}

/// Read a strap with the pull-down on (unfitted reads low).
fn esp_read_pulldown(pin: u8) -> bool {
    let pin = i32::from(pin);
    unsafe {
        sys::gpio_set_direction(pin, sys::gpio_mode_t_GPIO_MODE_INPUT);
        sys::gpio_set_pull_mode(pin, sys::gpio_pull_mode_t_GPIO_PULLDOWN_ONLY);
    }
    std::thread::sleep(Duration::from_millis(1));
    unsafe { sys::gpio_get_level(pin) != 0 }
}

type Expander = Arc<Mutex<Fxl6408<SharedI2c>>>;

/// A control line: an ESP GPIO, or a pin on the FXL6408 expander.
#[derive(Clone)]
enum Pin {
    Esp(u8),
    Exp(Expander, u8),
}

impl Pin {
    /// Configure as output driving `level`.
    fn output(&self, level: bool) -> bool {
        match self {
            Pin::Esp(p) => {
                esp_output(*p, level);
                true
            }
            Pin::Exp(x, p) => x.lock().unwrap().set_output(*p, level).is_ok(),
        }
    }
    /// Change the level of a configured output.
    fn set(&self, level: bool) -> bool {
        match self {
            Pin::Esp(p) => {
                esp_set(*p, level);
                true
            }
            Pin::Exp(x, p) => x.lock().unwrap().write_pin(*p, level).is_ok(),
        }
    }
}

/// Which board a TPS546 sits on: the numbers the stock firmware reports
/// differ.
enum Flavor {
    /// Bitaxe / NerdAxe: `current` = IOUT, power = VOUT × IOUT + offset.
    Bitaxe,
    /// BitForge Nano: an INA260 on the input rail gives voltage, current
    /// and power (ForgeOS reports those).
    Ina260(Ina260<SharedI2c>),
    /// Nerd carriers with a TPS546 (NerdQAxe++ Rev 7, NerdAxe Gaia):
    /// NerdOS reports the input current, POUT / VIN.
    Nerd,
}

/// The core regulator, whichever the board carries.
enum Regulator {
    Tps546(Tps546<SharedI2c>, Flavor),
    Tps5364x {
        vr: Tps53647<SharedI2c>,
        enable: Pin,
        /// The chips' I/O rails (NerdOS `LDO_EN_PIN`): off with the core
        /// rail, on before it (`off`, `on`).
        ldo: Option<Pin>,
        /// For the regulator-side TMP1075.
        bus: SharedI2c,
    },
}

impl Regulator {
    fn temperature_c(&mut self) -> Option<i32> {
        match self {
            Regulator::Tps546(vr, _) => vr.temperature_c().ok(),
            // NerdOS reads the TMP1075 behind the regulator, with the
            // board's correction; the part's own sensor is the fallback.
            Regulator::Tps5364x { vr, bus, .. } => {
                let offset = f32::from(vr.config().vr_temp_offset_c);
                match tmp1075::read_celsius(bus, tmp1075::VR_ADDR) {
                    Ok(c) => Some((c + offset).round() as i32),
                    Err(_) => vr.temperature_c().ok(),
                }
            }
        }
    }
    fn vin(&mut self) -> Option<f32> {
        match self {
            Regulator::Tps546(_, Flavor::Ina260(ina)) => {
                ina.bus_voltage_mv().ok().map(|mv| mv / 1000.0)
            }
            Regulator::Tps546(vr, _) => vr.vin().ok(),
            Regulator::Tps5364x { vr, .. } => vr.vin().ok(),
        }
    }
    fn vout(&mut self) -> Option<f32> {
        match self {
            Regulator::Tps546(vr, _) => vr.vout().ok(),
            Regulator::Tps5364x { vr, .. } => vr.vout().ok(),
        }
    }
    /// Output current, A: what the chips draw from the core rail.
    fn core_current_a(&mut self) -> Option<f32> {
        match self {
            Regulator::Tps546(vr, _) => vr.iout().ok(),
            Regulator::Tps5364x { vr, .. } => vr.iout().ok(),
        }
    }
    /// The output over-current warning limit written at init, A (the
    /// TPS5364x get one limit for warning and fault).
    fn core_current_warn_a(&self) -> f32 {
        match self {
            Regulator::Tps546(vr, _) => vr.config().iout_oc_warn,
            Regulator::Tps5364x { vr, .. } => f32::from(vr.config().ifault),
        }
    }
    /// The current the stock firmware reports: IOUT on the TPS546,
    /// IIN on the Nerd regulators.
    fn current_a(&mut self) -> Option<f32> {
        match self {
            Regulator::Tps546(vr, Flavor::Bitaxe) => vr.iout().ok(),
            Regulator::Tps546(_, Flavor::Ina260(ina)) => {
                ina.current_ma().ok().map(|ma| ma / 1000.0)
            }
            Regulator::Tps546(vr, Flavor::Nerd) => {
                let vin = vr.vin().ok().filter(|v| *v > 0.0)?;
                Some(vr.vout().ok()? * vr.iout().ok()? / vin)
            }
            Regulator::Tps5364x { vr, .. } => vr.iin().ok(),
        }
    }
    /// Board power: VOUT × IOUT + offset on the TPS546 boards (ESP-Miner),
    /// the regulator's input power on the Nerd boards (NerdOS).
    fn power_w(
        &mut self,
        vin: Option<f32>,
        vout: Option<f32>,
        current: Option<f32>,
        offset: f32,
    ) -> Option<f32> {
        match self {
            Regulator::Tps546(_, Flavor::Bitaxe) => match (vout, current) {
                (Some(v), Some(i)) => Some(v * i + offset),
                _ => None,
            },
            Regulator::Tps546(_, Flavor::Ina260(ina)) => ina.power_mw().ok().map(|mw| mw / 1000.0),
            // `current` is the input current here: VIN × IIN, what the
            // board draws. BitMaker's VOUT × IOUT + 5 W read ~6 W high on
            // the Gaia (32 W against 26 W at 350 MHz), and VOUT × IIN read
            // 7 W.
            // With the core off IIN reads a hair below zero: that is 0 W,
            // not a missing reading (tuners count a missing one as a failed
            // probe).
            Regulator::Tps546(_, Flavor::Nerd) => match (vin, current) {
                (Some(v), Some(i)) => Some((v * i).max(0.0)),
                _ => None,
            },
            Regulator::Tps5364x { vr, .. } => vr.pin().ok(),
        }
    }
    /// STATUS_WORD, with STATUS_TEMPERATURE when its TEMP bit is set (a
    /// warning and a fault share the bit).
    fn status(&mut self) -> Option<(u16, Option<u8>)> {
        let word = match self {
            Regulator::Tps546(vr, _) => vr.status_word().ok()?,
            Regulator::Tps5364x { vr, .. } => vr.status_word().ok()?,
        };
        let temp = if word & power_fault::status::TEMP != 0 {
            self.status_register(pmbus::STATUS_TEMPERATURE)
        } else {
            None
        };
        Some((word, temp))
    }
    fn status_register(&mut self, cmd: u8) -> Option<u8> {
        match self {
            Regulator::Tps546(vr, _) => vr.status_register(cmd).ok(),
            Regulator::Tps5364x { vr, .. } => vr.status_register(cmd).ok(),
        }
    }
    /// The STATUS_* detail registers on one line, logged once per fault
    /// (AxeOS `TPS546_log_snapshot`).
    fn status_snapshot(&mut self) -> String {
        let mut s = String::new();
        for (name, cmd) in [
            ("VOUT", pmbus::STATUS_VOUT),
            ("IOUT", pmbus::STATUS_IOUT),
            ("INPUT", pmbus::STATUS_INPUT),
            ("TEMP", pmbus::STATUS_TEMPERATURE),
            ("CML", pmbus::STATUS_CML),
            ("MFR", pmbus::STATUS_MFR_SPECIFIC),
        ] {
            match self.status_register(cmd) {
                Some(v) => s.push_str(&format!(" {name}={v:#04x}")),
                None => s.push_str(&format!(" {name}=?")),
            }
        }
        s
    }
    /// Clear latched faults before the output goes back on.
    fn clear_faults(&mut self) {
        let ok = match self {
            Regulator::Tps546(vr, _) => vr.clear_faults().is_ok(),
            Regulator::Tps5364x { vr, .. } => vr.clear_faults().is_ok(),
        };
        if !ok {
            log::warn!("board: CLEAR_FAULTS failed");
        }
    }
    fn set_core(&mut self, volts: f32) -> Result<(), String> {
        match self {
            Regulator::Tps546(vr, _) => {
                let r = vr.set_vout(volts).map_err(|e| format!("{e:?}"));
                // The output is set either way; the window register the
                // part refused keeps its last value (the limits are
                // percentages of VOUT_COMMAND, a fixed value can fall
                // outside the window at a low output).
                if let Some(cmd) = vr.limit_fault() {
                    log::warn!(
                        "board: TPS546 limit register {cmd:#04x} refused at {volts} V, output set"
                    );
                }
                r
            }
            Regulator::Tps5364x { vr, .. } => {
                vr.set_vout(volts).map(|_| ()).map_err(|e| format!("{e:?}"))
            }
        }
    }
    /// Core voltage back on at `volts` after [`off`](Self::off). On the
    /// Nerd carriers in NerdOS's `initAsics` order: the LDO first, 100 ms,
    /// then the regulator's enable and the voltage; the reset line stays
    /// low until the miner's `reset_chain`.
    fn on(&mut self, volts: f32) -> Result<(), String> {
        match self {
            Regulator::Tps546(vr, _) => vr.set_vout(volts).map_err(|e| format!("{e:?}")),
            Regulator::Tps5364x {
                vr, enable, ldo, ..
            } => {
                if let Some(l) = ldo {
                    l.set(true);
                    std::thread::sleep(Duration::from_millis(u64::from(
                        warm_boot::LDO_TO_CORE_ON_MS,
                    )));
                }
                enable.set(true);
                vr.set_vout(volts).map(|_| ()).map_err(|e| format!("{e:?}"))
            }
        }
    }
    /// Core voltage off: the TPS546 by command, the Nerd regulators by
    /// their enable line (NerdOS `setVoltage(0)`). On the Nerd carriers
    /// the chips' I/O rails go too, as in NerdOS's `shutdown`: the chain
    /// into reset first (a reset line held high into a chip without I/O
    /// power would feed it), the core off, 500 ms, the LDO off. Until
    /// v0.1.23 the LDO stayed up here, and on the NerdOctaxe-γ a dead
    /// hash domain survived every such off and on (2026-10-09).
    ///
    /// The 500 ms block the board loop (and `on` 100 ms): the loop
    /// samples once a second and counts its sensor-loss seconds per
    /// sample, the power-fault monitor goes `disabled()` right after
    /// this and judges by uptime seconds, and the board thread is on no
    /// task watchdog, so a sample half a second late changes nothing.
    /// `prepare_reset` waits up to 2 s for the rail to report off.
    /// Locks: `RESET` only, held for the one `set`; nothing else is
    /// locked here and nothing calls in while it is held (`prepare_reset`
    /// takes `RESET` from the HTTP or update task, never nested with
    /// anything the board thread holds).
    fn off(&mut self) {
        match self {
            Regulator::Tps546(vr, _) => {
                let _ = vr.set_vout(0.0);
            }
            Regulator::Tps5364x { enable, ldo, .. } => {
                if ldo.is_some() {
                    if let Some(pin) = RESET.lock().unwrap().as_ref() {
                        pin.set(false);
                    }
                }
                enable.set(false);
                if let Some(l) = ldo {
                    std::thread::sleep(Duration::from_millis(u64::from(
                        warm_boot::CORE_TO_LDO_OFF_MS,
                    )));
                    l.set(false);
                    log::info!(
                        "board: chips in reset, LDO off {} ms after the core (NerdOS shutdown order)",
                        warm_boot::CORE_TO_LDO_OFF_MS
                    );
                }
            }
        }
    }
}

/// Chip temperature and fans, whichever the board carries.
enum Thermal {
    Emc2101(Emc2101<SharedI2c>),
    /// Gamma Turbo, Naja Duo, Gamma Hex: two diodes, one fan; the driver
    /// knows which diode is the chip, and on the Turbo and the Naja the
    /// other one is the second chip's.
    Emc2103(Emc2103<SharedI2c>),
    /// BitForge Nano: one EMC2101 per chip behind the PCA9544, each with
    /// its own fan; the chip temperature is the hotter of the two.
    Nano([Emc2101<Muxed<SharedI2c>>; 2]),
    Nerd {
        fans: Emc2302<SharedI2c>,
        sensors: Addrs,
        bus: SharedI2c,
        offset: f32,
    },
    /// NerdNOS: a thermistor beside the chip on the ADC, no fan, and the
    /// core rail on a second ADC pin where the board senses it.
    Ntc {
        temp: crate::ntc::Reader,
        vcore: Option<crate::ntc::Reader>,
    },
}

impl Thermal {
    fn chip_temp_c(&mut self) -> Result<f32, String> {
        match self {
            Thermal::Ntc { temp, .. } => {
                temp.celsius().ok_or_else(|| "thermistor at a rail".into())
            }
            Thermal::Emc2101(t) => t.external_temp_c().map_err(|e| format!("{e:?}")),
            Thermal::Emc2103(t) => t.chip_temp_c().map_err(|e| format!("{e:?}")),
            Thermal::Nano(sensors) => {
                let mut best: Option<f32> = None;
                let mut err = String::new();
                for s in sensors.iter_mut() {
                    match s.external_temp_c() {
                        Ok(c) => best = Some(best.map_or(c, |b| b.max(c))),
                        Err(e) => err = format!("{e:?}"),
                    }
                }
                best.ok_or(err)
            }
            Thermal::Nerd {
                sensors,
                bus,
                offset,
                ..
            } => tmp1075::max_celsius(bus, sensors.as_slice())
                .map(|c| c + *offset)
                .ok_or_else(|| "no TMP1075 answered".into()),
        }
    }
    /// The second chip's own diode, on the boards that have one (the
    /// EMC2103's other channel on the Naja Duo and the Gamma Turbo).
    /// `None` on every other board.
    fn second_chip_temp_c(&mut self) -> Option<Result<f32, String>> {
        match self {
            Thermal::Emc2103(t) if t.second_chip() => {
                Some(t.second_temp_c().map_err(|e| format!("{e:?}")))
            }
            _ => None,
        }
    }
    /// RPM of the chip fan, and of the second output where there is one.
    fn rpm(&mut self) -> (u16, Option<u16>) {
        match self {
            Thermal::Ntc { .. } => (0, None),
            Thermal::Emc2101(t) => (t.fan_rpm().unwrap_or(0), None),
            Thermal::Emc2103(t) => (t.fan_rpm().unwrap_or(0), None),
            Thermal::Nano([a, b]) => (a.fan_rpm().unwrap_or(0), Some(b.fan_rpm().unwrap_or(0))),
            Thermal::Nerd { fans, .. } => (
                fans.rpm(Fan::Two).unwrap_or(0),
                Some(fans.rpm(Fan::One).unwrap_or(0)),
            ),
        }
    }
    /// PWM polarity of the Nerd fan outputs; nothing on the EMC2101.
    fn set_polarity(&mut self, invert: bool) -> bool {
        match self {
            Thermal::Emc2101(_) | Thermal::Emc2103(_) | Thermal::Nano(_) | Thermal::Ntc { .. } => {
                true
            }
            Thermal::Nerd { fans, .. } => fans.set_polarity(invert).is_ok(),
        }
    }
    /// Drive the chip fan and, on the Nerd boards, the regulator fan.
    fn set_fans(&mut self, chip: u8, vr: u8) -> bool {
        match self {
            // No fan to drive; the duty is reported as 0 (see `run`).
            Thermal::Ntc { .. } => true,
            Thermal::Emc2101(t) => t.set_fan_percent(chip).is_ok(),
            Thermal::Emc2103(t) => t.set_fan_percent(chip).is_ok(),
            // Both fans follow the chip loop (ForgeOS drives one speed).
            Thermal::Nano(sensors) => sensors
                .iter_mut()
                .fold(true, |ok, s| s.set_fan_percent(chip).is_ok() && ok),
            // NerdOS channel 0 (M2) is the chip's FAN2, channel 1 (M1) FAN1.
            Thermal::Nerd { fans, .. } => {
                let a = fans.set_fan_percent(Fan::Two, chip).is_ok();
                let b = fans.set_fan_percent(Fan::One, vr).is_ok();
                a && b
            }
        }
    }
}

/// Pick the profile from the config and create its I²C bus on the
/// profile's pins, before anything (the display first) needs it.
pub fn init_bus(mut i2c0: I2C0) {
    let cfg = config::get();
    let verified = config::verified_board();
    let profile = match Profile::by_name(&cfg.board_model) {
        Some(p) => match choice::start_mode(verified.as_deref(), p.name) {
            choice::Start::Verified => {
                set_choice(Some(Choice::Verified));
                p
            }
            // A boardModel saved by a firmware before v0.1.35 (no `boardok`
            // entry at all): it has run on this board, taken as verified.
            // Only a choice made under this firmware (the entry present but
            // not this name: the detection and PATCH write BOARD_PENDING)
            // goes through the check.
            choice::Start::Migrate => {
                log::info!(
                    "board: {} from an earlier version's settings: taken as verified",
                    p.name
                );
                if let Err(e) = config::set_verified_board(p.name) {
                    log::warn!("board: saving the verified board failed: {e}");
                }
                set_choice(Some(Choice::Verified));
                p
            }
            // Named in the settings (chosen in the web UI or set by PATCH,
            // or the verified board itself with `reverify`) but not verified
            // here: the bus must agree before any of its pins is driven,
            // and its first chain start counts the chips (choice_pending).
            choice::Start::Check => {
                log::warn!(
                "board: {} from the settings is not verified on this board yet: reading the bus first",
                p.name
            );
                let found = gather(&mut i2c0);
                gate(
                    p,
                    &found,
                    "chosen",
                    "named in the settings, not verified on this board yet",
                    Vec::new(),
                )
            }
        },
        None => {
            if !cfg.board_model.is_empty() {
                log::warn!("board: unknown boardModel {:?}, detecting", cfg.board_model);
            }
            first_start(&mut i2c0)
        }
    };
    *PROFILE.lock().unwrap() = Some(profile);
    // A clock above the board's own ceiling never reaches the chips: the
    // config's generic default (525 MHz, the Gamma's) ran 7 s on the
    // NerdNOS's fanless 1.4 V chip after its first start (2026-10-08)
    // before the API set 300. Clamped to the profile's default and saved,
    // so the first chain start is already at the board's clock.
    // The same for a clock the chips cannot be set to at all (below
    // [`MIN_FREQUENCY_MHZ`], or without a PLL setting): the API refuses
    // one since v0.1.42, but a config written before may hold it. The
    // chain would start at the chip's stock clock with that clock still
    // asked for, and every voltage drop would wait for it for ever.
    // The config as it is now: a first start above may have taken the
    // stock firmware's clock over since `cfg` was read.
    let stored_mhz = config::get().frequency;
    let above = profile.abs_max_frequency_mhz > 0 && stored_mhz > profile.abs_max_frequency_mhz;
    let unsettable = profile.asic_count > 0
        && profile.default_frequency_mhz >= MIN_FREQUENCY_MHZ
        && (stored_mhz < MIN_FREQUENCY_MHZ
            || stored_mhz
                .checked_mul(1_000_000)
                .and_then(|hz| warpaxe_core::pll::solve(&chip().pll, hz))
                .is_none());
    if above || unsettable {
        let mut fixed = config::get();
        let why = if above {
            format!("above its ceiling of {} MHz", profile.abs_max_frequency_mhz)
        } else if stored_mhz < MIN_FREQUENCY_MHZ {
            format!("below {MIN_FREQUENCY_MHZ} MHz")
        } else {
            "without a PLL setting".into()
        };
        log::warn!(
            "board: clock {} MHz is {why} on {}: starting at its default {} MHz",
            fixed.frequency,
            profile.name,
            profile.default_frequency_mhz
        );
        fixed.frequency = profile.default_frequency_mhz;
        if let Err(e) = config::save(&fixed) {
            log::warn!("board: saving the clock failed: {e}");
        }
    }
    // Stored settings that an update made stale (v0.1.36: the Nano's fan
    // target on the old diode scale), moved once.
    config::migrate(profile);
    // A core voltage from another board's settings (a Rev 7's 1300 mV
    // before the Nano was chosen) must not become the point the verified
    // first start goes to: outside the profile's own window it is reset
    // to the factory point, as the clock is.
    if choice_pending() {
        let cfg = config::get();
        let floor = profile.regulator_min_core_mv();
        let ceiling = profile.abs_max_core_mv;
        let outside =
            (floor > 0 && cfg.core_voltage < floor) || (ceiling > 0 && cfg.core_voltage > ceiling);
        if outside && cfg.core_voltage != profile.default_core_mv {
            let mut fixed = cfg;
            log::warn!(
                "board: core voltage {} mV is outside {}'s {floor}–{ceiling} mV: starting at its default {} mV",
                fixed.core_voltage,
                profile.name,
                profile.default_core_mv
            );
            fixed.core_voltage = profile.default_core_mv;
            if let Err(e) = config::save(&fixed) {
                log::warn!("board: saving the core voltage failed: {e}");
            }
        }
    }
    update(|s| {
        s.board = profile.name.into();
        s.has_vr = profile.tps546().is_some() || profile.tps53647().is_some();
        s.has_thermal = profile.thermal != ThermalKind::None;
        s.fan_auto = cfg.autofanspeed;
    });
    log::info!("board: profile {}", profile.name);
    let Some(pins) = profile.i2c else {
        return;
    };
    // SAFETY: the profile's I²C pins are used by nothing else on that
    // board (the devkit profile, whose LCD sits on 47/48, has no bus).
    let (sda, scl) = unsafe {
        (
            AnyIOPin::new(i32::from(pins.sda)),
            AnyIOPin::new(i32::from(pins.scl)),
        )
    };
    match I2cDriver::new(i2c0, sda, scl, &I2cConfig::new().baudrate(Hertz(pins.hz))) {
        Ok(b) => *BUS.lock().unwrap() = Some(SharedI2c(Arc::new(Mutex::new(b)))),
        Err(e) => {
            log::error!("board: i2c failed: {e}");
            update(|s| s.last_error = format!("i2c: {e}"));
        }
    }
}

/// Bring the board up: reset line, regulator at the bring-up voltage,
/// sensors and fans, then the sampling and fan task. Returns once the
/// chain may be started; on a failure the chain stays unpowered.
pub fn start() {
    let profile = profile();
    if let Some(p) = profile.asic_reset {
        let pin = Pin::Esp(p);
        pin.output(false); // held in reset until the regulator is up
        *RESET.lock().unwrap() = Some(pin);
    }
    if let Some(en) = profile.asic_enable {
        // ESP-Miner `configure_asic_power_enable`: enabled before the
        // regulator is set up, 20 ms to settle.
        let pin = Pin::Esp(en.pin);
        pin.output(en.active_high);
        *ASIC_ENABLE.lock().unwrap() = Some((pin, en.active_high));
        std::thread::sleep(Duration::from_millis(20));
    }
    if let ThermalKind::Ntc(cfg) = profile.thermal {
        start_nerdnos(profile, cfg);
        return;
    }
    let Some(bus) = i2c() else {
        return;
    };
    match profile.vr {
        Vr::Tps5364x { .. } => start_nerd(profile, bus),
        Vr::Tps546Nerd { .. } => start_tps546_nerd(profile, bus),
        _ => start_bitaxe(profile, bus),
    }
}

fn start_bitaxe(profile: &'static Profile, bus: SharedI2c) {
    let cfg = config::get();
    let mut vr = profile.tps546().map(|c| Tps546::new(bus.clone(), c));
    if let Some(vr) = vr.as_mut() {
        // ESP-Miner's prelude: clear a pending SMBus alert, give the
        // part 15 ms, and retry the id check a few times.
        match vr.alert_response() {
            Ok(a) => log::info!("board: SMBus alert response {a:#04x}"),
            Err(_) => log::info!("board: no SMBus alert pending"),
        }
        std::thread::sleep(Duration::from_millis(15));
        // Probe the part step by step so a failure says where; the
        // window it holds before init shows what the straps / NVM set.
        let probe = format!(
            "vout_mode={:?} status={:?} id={:?} {:?}",
            vr.vout_mode(),
            vr.status_word(),
            vr.device_id(),
            vr.window()
        );
        log::info!("board: TPS546 probe {probe}");
        update(|s| s.vr_probe = probe);
        let mut result = vr.init();
        for attempt in 1..5 {
            if result.is_ok() {
                break;
            }
            log::warn!("board: TPS546 init attempt {attempt} failed: {result:?}");
            std::thread::sleep(Duration::from_millis(3));
            result = vr.init();
        }
        if let Err(e) = &result {
            let cmd = vr.last_command();
            log::error!("board: TPS546 init failed at command {cmd:#04x}: {e:?}");
            // Busy or refused? Read it back, write it again after a pause,
            // and try the next register in the list.
            std::thread::sleep(Duration::from_millis(100));
            let rd = vr.read_register(cmd);
            let wr_again = vr.write_register(cmd, 0x0280);
            let wr_next = vr.write_register(0x42, 0x02C7);
            let st = vr.status_word();
            let post = format!(
                " init=Err({e:?}) at cmd {cmd:#04x}; after 100 ms: read={rd:?} write_again={wr_again:?} write_0x42={wr_next:?} status={st:?}"
            );
            update(|s| s.vr_probe.push_str(&post));
        }
        match result {
            Ok(status) => {
                log::info!("board: TPS546 up, status {status:#06x}");
                update(|s| {
                    s.vr_status = status;
                    s.vr_part = "TPS546".into();
                });
                let mv = bring_up_mv(profile, cfg.core_voltage);
                let volts = profile.vout_for_core_mv(mv);
                match vr.set_vout(volts) {
                    Ok(()) => log::info!("board: core {mv} mV"),
                    Err(e) => log::error!("board: set_vout {volts} V failed: {e:?}"),
                }
            }
            Err(e) => {
                log::error!("board: TPS546 init failed: {e:?}");
                update(|s| {
                    s.has_vr = false;
                    s.last_error = format!("tps546: {e:?}");
                });
            }
        }
    }
    // The Nano's input-side monitor; without it the TPS546 numbers stand.
    let flavor = match profile.vr {
        Vr::Tps546Ina260 { ina260, .. } => {
            let mut ina = Ina260::new(bus.clone(), ina260);
            match ina.init() {
                Ok(()) => {
                    log::info!("board: INA260 up");
                    Flavor::Ina260(ina)
                }
                Err(e) => {
                    log::error!("board: INA260 init failed: {e:?}");
                    update(|s| s.last_error = format!("ina260: {e:?}"));
                    Flavor::Bitaxe
                }
            }
        }
        _ => Flavor::Bitaxe,
    };
    let vr = vr
        .filter(|_| state().has_vr)
        .map(|vr| Regulator::Tps546(vr, flavor));

    let thermal = match profile.thermal {
        ThermalKind::DualEmc2101Mux {
            config,
            mux,
            channels,
        } => nano_thermal(config, mux, channels, &bus),
        ThermalKind::Emc2103(config) => emc2103_thermal(config, &bus),
        _ => emc2101_thermal(profile, &bus),
    };

    // Regulator up: release the chain from reset with the init timing.
    std::thread::sleep(Duration::from_millis(u64::from(profile.reset_low_ms)));
    reset_chain(profile.reset_low_ms, profile.reset_release_ms);
    spawn_run(profile, vr, thermal);
}

/// The NerdNOS: no regulator and no bus, a thermistor on the ADC, the
/// chip's run/stop line as the reset (held low since `quiesce`; the
/// miner releases it with the stock pulse in `reset_chain`). The board
/// loop runs for the temperature alone: an overheat or a lost sensor
/// holds the chip in reset, as the stock firmware drops its PEN.
fn start_nerdnos(profile: &'static Profile, cfg: warpaxe_board::ntc::Config) {
    if let Some(p) = profile.stock_reset_pulse {
        // Idle low until the first init, like the stock firmware's RST.
        Pin::Esp(p).output(false);
    }
    let thermal = match crate::ntc::Reader::new(cfg) {
        Some(r) => {
            match r.millivolts() {
                Some(mv) => log::info!(
                    "board: thermistor on GPIO{} reads {mv} mV ({:?} °C)",
                    cfg.gpio,
                    cfg.celsius(mv)
                ),
                None => log::warn!("board: thermistor on GPIO{} gives no reading", cfg.gpio),
            }
            // The core rail, sensed directly (WantClue `nerdnos_get_vcore`).
            let vcore = cfg.vcore_gpio.and_then(crate::ntc::Reader::new_pin);
            if let Some(v) = vcore.as_ref() {
                // Read here the rail is still off: the chip sits in reset
                // until the miner's first init. Say so, not just "0 mV".
                match v.millivolts() {
                    Some(mv) if mv >= 100 => {
                        log::info!("board: core rail on GPIO{:?} reads {mv} mV", cfg.vcore_gpio)
                    }
                    Some(mv) => log::info!(
                        "board: core rail on GPIO{:?} reads {mv} mV: off until the chip starts",
                        cfg.vcore_gpio
                    ),
                    None => log::warn!(
                        "board: core rail on GPIO{:?} gives no reading",
                        cfg.vcore_gpio
                    ),
                }
            }
            Some(Thermal::Ntc { temp: r, vcore })
        }
        None => {
            update(|s| {
                s.has_thermal = false;
                s.last_error = "ntc: adc failed".into();
            });
            None
        }
    };
    spawn_run(profile, None, thermal);
}

/// The Nerd power-up, NerdOS `initAsics`: everything off and the chain
/// in reset; 250 ms; LDO on; 100 ms; regulator init; enable at the
/// bring-up voltage; 500 ms; release reset; 250 ms.
fn start_nerd(profile: &'static Profile, bus: SharedI2c) {
    let Vr::Tps5364x {
        regulator,
        enable,
        ldo_enable,
        via_expander,
        alt,
    } = profile.vr
    else {
        return;
    };
    let cfg = config::get();
    let fail = |what: String| {
        log::error!("board: {what}; the chain stays unpowered");
        update(|s| {
            s.has_vr = false;
            s.last_error = what;
        });
    };

    // The Q13xx expander: out of its own reset first, or it never answers.
    let expander: Option<Expander> = if via_expander {
        esp_output(q13xx::EXPANDER_RESET_GPIO, false);
        std::thread::sleep(Duration::from_millis(10));
        let mut x = Fxl6408::new(bus.clone());
        match x.init() {
            Ok(id) => log::info!("board: FXL6408 up, id {id:#04x}"),
            Err(e) => return fail(format!("fxl6408: {e:?}")),
        }
        std::thread::sleep(Duration::from_millis(100));
        Some(Arc::new(Mutex::new(x)))
    } else {
        None
    };
    let pin = |n: u8| match &expander {
        Some(x) => Pin::Exp(x.clone(), n),
        None => Pin::Esp(n),
    };

    // Everything off, chain in reset (NerdOS order: regulator, LDO, reset).
    let en = pin(enable);
    let ldo = ldo_enable.map(pin);
    if !en.output(false) {
        return fail("regulator enable pin".into());
    }
    if let Some(l) = &ldo {
        l.output(false);
    }
    rails_low_now();
    if let Some(x) = &expander {
        let reset = Pin::Exp(x.clone(), q13xx::ASIC_RESET);
        reset.output(false);
        *RESET.lock().unwrap() = Some(reset);
        let _ = x.lock().unwrap().set_input_pull_up(q13xx::CAN_SLAVE);
    }

    // Which regulator is fitted: the strap says; on the Q1373 the device
    // code then decides the part and the strap only the phase count.
    let (regulator, other_by_code) = match alt {
        Some(a) => {
            let level = match &expander {
                Some(x) => x.lock().unwrap().read_pin(a.pin).unwrap_or(!a.level),
                None => esp_read_pulldown(a.pin),
            };
            let (by_strap, other) = if level == a.level {
                (a.regulator, regulator)
            } else {
                (regulator, a.regulator)
            };
            (by_strap, a.part_by_code.then_some(other))
        }
        None => (regulator, None),
    };
    log::info!(
        "board: regulator by strap {:?} {} phases",
        regulator.variant,
        regulator.phases
    );

    // NerdOS's 250 ms with everything off; after a warm reboot the
    // seconds a power cycle would have given the rails, counted from the
    // boot's rail_off (warm_boot).
    let power_on = crate::diag::reset_was_power_on();
    let low_for = rails_low_for_ms();
    let settle = warm_boot::settle_ms(power_on, low_for);
    if settle > warm_boot::NERD_SETTLE_MS {
        log::warn!(
            "board: warm boot (reset reason {}): rails low for {low_for} ms, {settle} ms more before the LDO comes on",
            crate::diag::reset_reason()
        );
    }
    std::thread::sleep(Duration::from_millis(u64::from(settle)));
    if let Some(l) = &ldo {
        l.set(true);
    }
    std::thread::sleep(Duration::from_millis(u64::from(
        warm_boot::LDO_TO_CORE_ON_MS,
    )));

    let mut vr = Tps53647::new(bus.clone(), regulator);
    let code = vr.device_code();
    let probe = format!("device_code={code:?}");
    log::info!("board: TPS5364x probe {probe}");
    update(|s| s.vr_probe = probe);
    let regulator = match (other_by_code, code) {
        (Some(other), Ok(code)) => {
            let (resolved, note) = tps53647::resolve_by_code(code, regulator, other);
            if let Some(note) = note {
                log::warn!("board: regulator detect: {note} (device code {code:#06x})");
            }
            if resolved != regulator {
                vr = Tps53647::new(vr.release(), resolved);
            }
            resolved
        }
        _ => regulator,
    };
    let part = format!("{:?} {} phases", regulator.variant, regulator.phases);
    log::info!("board: regulator {part}");
    update(|s| s.vr_part = part);
    if let Err(e) = vr.init() {
        let cmd = vr.last_command();
        return fail(format!("tps5364x init at {cmd:#04x}: {e:?}"));
    }
    let mv = bring_up_mv(profile, cfg.core_voltage);
    en.set(true);
    if let Err(e) = vr.set_vout(profile.vout_for_core_mv(mv)) {
        en.set(false);
        return fail(format!("tps5364x set_vout {mv} mV: {e:?}"));
    }
    log::info!("board: core {mv} mV");
    std::thread::sleep(Duration::from_millis(500));
    reset_chain(0, 250);

    let thermal = nerd_thermal(profile, &bus, cfg.invertfanpolarity);
    let regulator = Regulator::Tps5364x {
        vr,
        enable: en,
        ldo,
        bus,
    };
    spawn_run(profile, Some(regulator), Some(thermal));
}

/// The EMC2101 on the chip diode, fan at full speed until the first
/// reading; `None` when the profile has none or it does not answer.
fn emc2101_thermal(profile: &'static Profile, bus: &SharedI2c) -> Option<Thermal> {
    let mut thermal = profile.emc2101().map(|c| Emc2101::new(bus.clone(), c));
    if let Some(t) = thermal.as_mut() {
        match t.init().and_then(|()| t.set_fan_percent(100)) {
            Ok(()) => log::info!("board: EMC2101 up, fan 100 %"),
            Err(e) => {
                log::error!("board: EMC2101 init failed: {e:?}");
                update(|s| {
                    s.has_thermal = false;
                    s.last_error = format!("emc2101: {e:?}");
                });
            }
        }
    }
    thermal
        .filter(|_| state().has_thermal)
        .map(Thermal::Emc2101)
}

/// The EMC2103 of the Gamma Turbo / Naja Duo / Gamma Hex, fan at full
/// speed until the first reading; `None` when it does not answer.
fn emc2103_thermal(config: warpaxe_board::emc2103::Config, bus: &SharedI2c) -> Option<Thermal> {
    let mut t = Emc2103::new(bus.clone(), config);
    match t.init().and_then(|()| t.set_fan_percent(100)) {
        Ok(()) => {
            log::info!("board: EMC2103 up, fan 100 %");
            Some(Thermal::Emc2103(t))
        }
        Err(e) => {
            log::error!("board: EMC2103 init failed: {e:?}");
            update(|s| {
                s.has_thermal = false;
                s.last_error = format!("emc2103: {e:?}");
            });
            None
        }
    }
}

/// The Nano's two EMC2101 behind the PCA9544, fans at full speed until
/// the first reading; `None` unless both answer.
fn nano_thermal(
    config: warpaxe_board::emc2101::Config,
    mux: u8,
    channels: [u8; 2],
    bus: &SharedI2c,
) -> Option<Thermal> {
    let mut sensors = channels.map(|ch| Emc2101::new(Muxed::new(bus.clone(), mux, ch), config));
    for (i, s) in sensors.iter_mut().enumerate() {
        match s.init().and_then(|()| s.set_fan_percent(100)) {
            Ok(()) => log::info!(
                "board: EMC2101 #{} up (mux channel {}), fan 100 %",
                i + 1,
                channels[i]
            ),
            Err(e) => {
                log::error!(
                    "board: EMC2101 #{} (mux channel {}) init failed: {e:?}",
                    i + 1,
                    channels[i]
                );
                update(|s| {
                    s.has_thermal = false;
                    s.last_error = format!("emc2101 #{}: {e:?}", i + 1);
                });
                return None;
            }
        }
    }
    Some(Thermal::Nano(sensors))
}

/// The Nerd carrier's sensors and fans: TMP1075 detection and the
/// EMC2302 at full speed until the first reading.
fn nerd_thermal(profile: &'static Profile, bus: &SharedI2c, invert_polarity: bool) -> Thermal {
    let mut sensor_bus = bus.clone();
    let sensors = tmp1075::detect(&mut sensor_bus);
    log::info!("board: TMP1075 at {:02x?}", sensors.as_slice());
    let mut fans = Emc2302::new(bus.clone());
    if let Ok((product, mfr)) = fans.identify() {
        log::info!("board: EMC2302 product {product:#04x} manufacturer {mfr:#04x}");
    }
    let fans_ok = fans.init(invert_polarity).is_ok()
        && fans.set_fan_percent(Fan::One, 100).is_ok()
        && fans.set_fan_percent(Fan::Two, 100).is_ok();
    if !fans_ok {
        log::error!("board: EMC2302 init failed");
        update(|s| s.last_error = "emc2302 init failed".into());
    }
    Thermal::Nerd {
        fans,
        sensors,
        bus: bus.clone(),
        offset: profile.temp_offset_c,
    }
}

/// A TPS546 on a Nerd carrier. With an enable line, the NerdQAxe++ Rev 7
/// (NerdOS `NerdQaxePlus2::initAsics`: enable and LDO off, chain in
/// reset; 250 ms; LDO on; 100 ms; enable on; 50 ms; init; the bring-up
/// voltage; 500 ms; release reset; 250 ms). Without one, the NerdAxe
/// Gaia (BitMaker `nerdaxegaia.cpp` initBoard + initAsics): the LDO
/// comes up and is never cut ("the chip runs on the LDO alone during
/// the self-test; cutting it left the chip unpowered and it failed to
/// enumerate"), 100 ms, init with the output off, 500 ms, reset held
/// low 350 ms more, the bring-up voltage, 500 ms, reset released,
/// 250 ms. An enable line stays high afterwards; OPERATION switches the
/// output. First Gaia run (2026-09-29) with the Rev 7 order ended in
/// NoChips; this order is the reference firmware's.
fn start_tps546_nerd(profile: &'static Profile, bus: SharedI2c) {
    let Vr::Tps546Nerd {
        regulator,
        enable,
        ldo_enable,
    } = profile.vr
    else {
        return;
    };
    let cfg = config::get();
    let fail = |what: String| {
        log::error!("board: {what}; the chain stays unpowered");
        update(|s| {
            s.has_vr = false;
            s.last_error = what;
        });
    };
    let en = enable.map(Pin::Esp);
    let ldo = ldo_enable.map(Pin::Esp);
    match &en {
        Some(e) => {
            if !e.output(false) {
                return fail("regulator enable pin".into());
            }
            if let Some(l) = &ldo {
                l.output(false);
            }
            std::thread::sleep(Duration::from_millis(250));
            if let Some(l) = &ldo {
                l.set(true);
            }
            std::thread::sleep(Duration::from_millis(100));
            e.set(true);
            std::thread::sleep(Duration::from_millis(50));
        }
        None => {
            // The LDO straight up, never pulsed low.
            if let Some(l) = &ldo {
                if !l.output(true) {
                    return fail("ldo enable pin".into());
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    let mut vr = Tps546::new(bus.clone(), regulator);
    // The window before init: after a power cycle it is what the VSEL /
    // MSEL1 straps or the NVM programmed (the divider the board's
    // compensation was chosen for); after a reset, what the last init
    // wrote.
    let probe = format!("device_id={:?} {:?}", vr.device_id(), vr.window());
    log::info!("board: TPS546 (Nerd carrier) probe {probe}");
    update(|s| s.vr_probe = probe);
    match vr.init() {
        Ok(status) => {
            log::info!("board: TPS546 up, status {status:#06x}");
            update(|s| {
                s.vr_status = status;
                s.vr_part = format!("TPS546, {} voltage domain(s)", profile.voltage_domains);
            });
        }
        Err(e) => {
            let cmd = vr.last_command();
            if let Some(e) = &en {
                e.set(false);
            }
            return fail(format!("tps546 init at {cmd:#04x}: {e:?}"));
        }
    }
    let mut release_ms = 250;
    if en.is_none() {
        // BitMaker initAsics: output off (init left it so), 500 ms, then
        // the reset held low for the profile's time before the rail.
        std::thread::sleep(Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(u64::from(profile.reset_low_ms)));
        release_ms = profile.reset_release_ms;
    }
    let mv = bring_up_mv(profile, cfg.core_voltage);
    if let Err(e) = vr.set_vout(profile.vout_for_core_mv(mv)) {
        if let Some(en) = &en {
            en.set(false);
        }
        return fail(format!("tps546 set_vout {mv} mV: {e:?}"));
    }
    log::info!("board: core {mv} mV");
    std::thread::sleep(Duration::from_millis(500));
    reset_chain(0, release_ms);

    let thermal = match profile.thermal {
        ThermalKind::Tmp1075Emc2302 => Some(nerd_thermal(profile, &bus, cfg.invertfanpolarity)),
        ThermalKind::Emc2101(_) => emc2101_thermal(profile, &bus),
        ThermalKind::Emc2103(config) => emc2103_thermal(config, &bus),
        ThermalKind::DualEmc2101Mux {
            config,
            mux,
            channels,
        } => nano_thermal(config, mux, channels, &bus),
        // A thermistor board never comes through here (`start_nerdnos`).
        ThermalKind::None | ThermalKind::Ntc(_) => None,
    };
    spawn_run(profile, Some(Regulator::Tps546(vr, Flavor::Nerd)), thermal);
}

fn spawn_run(profile: &'static Profile, vr: Option<Regulator>, thermal: Option<Thermal>) {
    if vr.is_none() && thermal.is_none() {
        return;
    }
    BOARD_THREAD.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("board".into())
        // 10 KB: 6 KB overflowed on a NerdQAxe++ about a minute after
        // boot once the event details grew (v0.1.35 candidate,
        // 2026-10-22); at 8 KB the low-water mark was 2 KB on the common
        // path alone, before any power fault or overheat event. run()
        // logs its low-water mark.
        .stack_size(10 * 1024)
        .spawn(move || run(profile, vr, thermal))
        .expect("board thread");
}

fn run(profile: &'static Profile, mut vr: Option<Regulator>, mut thermal: Option<Thermal>) {
    let mut cfg = config::get();
    let mut pid = FanPid::new(f32::from(cfg.min_fan_speed));
    pid.reset(100.0, profile.fan_target_c);
    let mut pid_vr = FanPid::new(f32::from(cfg.min_fan_speed));
    pid_vr.reset(100.0, vr_fan_target_c(profile));
    let mut applied_mv = bring_up_mv(profile, cfg.core_voltage);
    // A core voltage the regulator refused is tried again once a second,
    // and logged once per value.
    let mut core_retry_at = Instant::now();
    let mut core_refused_logged: Option<u16> = None;
    // A voltage drop waiting for the clock to come down (core_hold).
    let mut core_hold = CoreHold::new();
    let mut core_held_logged = false;
    let mut fans: (u8, u8) = (100, 100);
    let mut last_sample = Instant::now() - Duration::from_secs(1);
    let mut temp: Option<f32> = None;
    let mut vr_temp: Option<i32> = None;
    let mut polarity = cfg.invertfanpolarity;
    // The lost-sensor clock: it runs only while the core rail is on
    // (`SensorWatch`: chips that are off on purpose never go blind).
    let mut sensor = SensorWatch::new();
    // Two chip diodes: which have read since boot (`chip_temps`).
    let mut two_chips = ChipTemps::<2>::new();
    // The chips' starts seen so far (`CHIP_STARTS`); the second diode's
    // reading is held back in the settle window, and that was said for
    // this start.
    let mut chip_starts_seen = 0u32;
    let mut settle_held = false;
    let mut settle_logged = false;
    // A chip diode that has never read since boot while the other one
    // reads: whether that was said, and that the fans run at full for
    // it (the chip has no stop line of its own).
    let mut never_read_warned = [false; 2];
    let mut diode_never_read = false;
    // A chip temperature has been read since boot: losing it then stops
    // the chain (Cause::Blind). A board whose sensor never answered runs
    // without the chip limit, fans at full, as NerdOS does with no
    // ASIC-side TMP1075 (it shows 0 °C). Seen 2026-10-01 on a NerdOctaxe
    // v2.2 under both Nerd profiles: "overheated" after exactly 60 s.
    let mut no_sensor_warned = false;
    let t0 = Instant::now();
    // The least stack this thread has had left, logged at each new low by
    // 256 bytes (spawn_run).
    let mut stack_low = u32::MAX;
    // Fans at full until the chips have warmed up (fan::Warmup); the PID
    // takes over from there.
    let mut warmup: Option<Warmup> = Some(Warmup::start(0));
    update(|s| s.fan_warmup = true);
    // The regulator has reported a temperature since boot: a missing one
    // then counts as unknown, not as cool.
    let mut vr_temp_seen = false;
    // The regulator's own verdict; the output was enabled at start.
    let mut pf = Monitor::new();
    if POWERED.load(Ordering::Relaxed) {
        pf.enabled(0);
    }
    // A `cycle_chips` in progress: when the rail went down for it.
    let mut cycle_off_at: Option<Instant> = None;
    // The reset line was pulled low for a no-pool stop with the rail off.
    let mut pool_reset_held = false;
    let mut pool_reset_failed = false;
    loop {
        // Once a second: sensors, config, the sample.
        if last_sample.elapsed() >= Duration::from_secs(1) {
            last_sample = Instant::now();
            let unused = crate::diag::stack_unused();
            if unused.saturating_add(256) <= stack_low {
                stack_low = unused;
                log::info!("board: {unused} bytes of the thread's stack never used so far");
            }
            cfg = config::get();
            pid.set_min_percent(f32::from(cfg.min_fan_speed));
            pid_vr.set_min_percent(f32::from(cfg.min_fan_speed));
            let (kp, ki, kd) = (cfg.pid_p as f32, cfg.pid_i as f32, cfg.pid_d as f32);
            pid.set_gains(kp, ki, kd);
            pid_vr.set_gains(kp, ki, kd);
            if cfg.invertfanpolarity != polarity {
                if let Some(t) = thermal.as_mut() {
                    if t.set_polarity(cfg.invertfanpolarity) {
                        polarity = cfg.invertfanpolarity;
                        log::info!("board: fan polarity inverted {polarity}");
                    }
                }
            }
            let mut vin = None;
            let mut vout = None;
            let mut current = None;
            let mut power = None;
            let mut core_current = None;
            let mut core_current_warn = None;
            let mut status = None;
            vr_temp = None;
            if let Some(vr) = vr.as_mut() {
                // A regulator that is still coming up (right after power-on)
                // can answer with garbage words (0xFF01 read as -127.5 V):
                // those count as no reading, never as a value.
                vr_temp = plausible(
                    vr.temperature_c().map(|t| t as f32),
                    -40.0,
                    150.0,
                    "vr temp",
                )
                .map(|t| t as i32);
                vin = plausible(vr.vin(), 0.0, 30.0, "vin");
                vout = plausible(vr.vout(), 0.0, 2.0, "vout");
                current = plausible(vr.current_a(), -1.0, 150.0, "iout");
                core_current =
                    plausible(vr.core_current_a(), -1.0, 300.0, "core current").map(|a| a.max(0.0));
                core_current_warn = Some(vr.core_current_warn_a());
                // Never negative: with the core off a current reads a hair
                // below zero, and a tuner counts a missing power as a
                // failed probe.
                power = plausible(
                    vr.power_w(vin, vout, current, profile.power_offset_w)
                        .map(|p| p.max(0.0)),
                    0.0,
                    500.0,
                    "power",
                );
                status = vr.status();
            }
            // The self-test's phase read, on this thread because the
            // regulator is this thread's: PHASE paged per phase and back.
            if phase_read_wanted() {
                let r = match vr.as_mut() {
                    Some(Regulator::Tps546(vr, _)) => vr.phase_currents().map_err(|e| {
                        format!("{e:?} at PMBus command 0x{:02X}", vr.last_command())
                    }),
                    _ => Err("no TPS546 on this board".to_string()),
                };
                match &r {
                    Ok(c) => log::info!("board: phase currents {:?} A", c.amps()),
                    Err(e) => log::warn!("board: phase currents: {e}"),
                }
                *PHASE_READ.lock().unwrap() = PhaseRead::Done(r);
            }
            // A board that measures no power: the profile's estimate from
            // the chip clock, flagged as such for the API and the display.
            let mut power_estimated = false;
            if power.is_none() {
                if let Some(e) = profile.power_estimate {
                    power = Some(e.watts(crate::miner::state().frequency_hz));
                    power_estimated = true;
                }
            }
            // The core rail on a board that senses it on the ADC instead
            // of reporting it from a regulator.
            if let Some(Thermal::Ntc {
                vcore: Some(sense), ..
            }) = thermal.as_mut()
            {
                vout = plausible(
                    sense.millivolts().map(|mv| f32::from(mv) / 1000.0),
                    0.0,
                    2.0,
                    "vcore",
                );
            }
            let mut rpm = (0, None);
            let chip_limit = profile.chip_limit_c(cfg.overheat_temp);
            // The rail as this sample finds it: a reading that fails with
            // the rail off is not a lost sensor (a Gamma's chip diode
            // gives nothing without the chip's power).
            let rail_on = POWERED.load(Ordering::Relaxed);
            // A two-chip board whose chips are not running yet: its
            // diodes give no reading, which counts as with the rail off.
            let mut diodes_idle = false;
            if let Some(t) = thermal.as_mut() {
                let checked = |r: Result<f32, String>| {
                    r.and_then(|c| {
                        plausible(Some(c), -40.0, 150.0, "chip temp")
                            .ok_or_else(|| "implausible reading".to_string())
                    })
                };
                let first = checked(t.chip_temp_c());
                // A second chip with its own diode: fan and stop line
                // act on the hotter of the two, and a diode that read
                // before and fails now makes the temperature unknown,
                // never cool (ESP-Miner stops on either diode over
                // 75 °C and runs the fan on the hotter).
                let acting = match t.second_chip_temp_c().map(checked) {
                    None => first,
                    // The chips not running yet (rail off, in reset, the
                    // chain not answered): neither diode reads its chip.
                    // No reading, and none of a lost sensor's either.
                    Some(_) if !CHIPS_ANSWERED.load(Ordering::Relaxed) => {
                        diodes_idle = true;
                        update(|s| s.chip_temps = vec![None, None]);
                        Err("the chips are not running".to_string())
                    }
                    Some(second) => {
                        let chips = [first.as_ref().ok().copied(), second.as_ref().ok().copied()];
                        update(|s| s.chip_temps = chips.to_vec());
                        // The chips started since the last sample (the
                        // rail back on, the reset pulse, the chain's
                        // answer): the second diode's settle window
                        // starts over. Its reading over the stop line is
                        // left out for those samples and the fans run
                        // at full; after them it stops as ever.
                        let starts = CHIP_STARTS.load(Ordering::Relaxed);
                        if starts != chip_starts_seen {
                            chip_starts_seen = starts;
                            two_chips.chips_started();
                            settle_logged = false;
                        }
                        let acting = two_chips.sample(chips, chip_limit);
                        settle_held = two_chips.held(1);
                        if settle_held && !settle_logged {
                            settle_logged = true;
                            log::warn!(
                                "board: chip 2 reads {:?} °C right after the chips' start (limit {chip_limit}): the sensor's average still holds readings from before, settling for {} s more, fans at full, no stop",
                                chips[1],
                                two_chips.settle_left()
                            );
                        } else if !settle_held && settle_logged && two_chips.settle_left() == 0 {
                            settle_logged = false;
                            log::info!("board: chip 2's reading has settled, chip temperatures {chips:?}");
                        }
                        // A diode that has never read while the other
                        // does: left out of the temperature, and after
                        // the time a lost sensor is given the fans go to
                        // full (its chip has no stop line of its own; a
                        // reboot must not take the protection a failed
                        // diode had before it), with a line in the log
                        // and the board's standing notice (`boardError`).
                        // The chips keep running. One failed read at
                        // boot is not it; when the diode reads after
                        // all, everything is as normal.
                        diode_never_read =
                            (0..2).any(|i| two_chips.never_read_s(i) >= TEMP_LOST_S);
                        for i in 0..2 {
                            if two_chips.never_read_s(i) >= TEMP_LOST_S {
                                let text = format!(
                                    "chip {}'s temperature diode has not read since boot: fans at full, the stop line follows chip {} alone",
                                    i + 1,
                                    2 - i
                                );
                                if !never_read_warned[i] {
                                    never_read_warned[i] = true;
                                    log::warn!("board: {text}");
                                }
                                // Standing: put back when another text
                                // took the place and was cleared.
                                update(|s| {
                                    if s.last_error.is_empty() {
                                        s.last_error = text;
                                    }
                                });
                            }
                        }
                        // A diode that reads after all: the notice goes.
                        for i in 0..2 {
                            if never_read_warned[i] && two_chips.seen(i) {
                                never_read_warned[i] = false;
                                log::info!("board: chip {}'s temperature diode reads now", i + 1);
                                update(|s| {
                                    if s.last_error.contains("temperature diode has not read since boot") {
                                        s.last_error.clear();
                                    }
                                });
                            }
                        }
                        acting.ok_or_else(|| {
                            let why = |r: &Result<f32, String>| r.as_ref().err().cloned();
                            match (why(&first), why(&second)) {
                                (Some(e), _) => format!("chip 1: {e}"),
                                (None, Some(e)) => format!("chip 2: {e}"),
                                (None, None) => "no reading".into(),
                            }
                        })
                    }
                };
                match acting {
                    Ok(c) => {
                        temp = Some(c);
                        sensor.sample(true, rail_on);
                        update(|s| {
                            if s.last_error.starts_with("temp:") {
                                s.last_error.clear();
                            }
                        });
                    }
                    Err(e) => {
                        temp = None;
                        let counted = rail_on && !diodes_idle;
                        sensor.sample(false, counted);
                        // With the rail off (or the chips not running)
                        // this is no fault to report: no notice, and one
                        // from before goes.
                        update(|s| {
                            if counted {
                                s.last_error = format!("temp: {e}");
                            } else if s.last_error.starts_with("temp:") {
                                s.last_error.clear();
                            }
                        });
                    }
                }
                rpm = t.rpm();
            }
            let hot_chip = temp.is_some_and(|c| c > chip_limit);
            let hot_vr = vr_temp.is_some_and(|c| c as f32 > profile.vr_stop_c);
            let blind = sensor.blind(TEMP_LOST_S);
            let temp_lost_s = sensor.lost_s();
            if sensor.never(TEMP_LOST_S) && !no_sensor_warned {
                no_sensor_warned = true;
                log::warn!(
                    "board: no chip temperature since boot ({temp_lost_s} s): running without the chip limit, fans at full"
                );
            }
            vr_temp_seen |= vr_temp.is_some();
            let now_s = t0.elapsed().as_secs() as u32;
            // The regulator's own verdict (AxeOS `power_fault`), judged only
            // while the output is meant to be on. A fault takes the overheat
            // path; a warning is only reported.
            let mut power_fault = false;
            if let (Some((word, tstat)), Some(reg)) = (status, vr.as_mut()) {
                if POWERED.load(Ordering::Relaxed) && !overheat() && !shutdown() {
                    let (verdict, changed) = pf.observe(now_s, word, tstat);
                    match verdict {
                        Verdict::Fault(f) | Verdict::Warning(f) => {
                            let what = f
                                .iter()
                                .map(Condition::describe)
                                .collect::<Vec<_>>()
                                .join("; ");
                            power_fault = matches!(verdict, Verdict::Fault(_));
                            if changed {
                                let snap = reg.status_snapshot();
                                if power_fault {
                                    log::error!(
                                        "board: REGULATOR FAULT status {word:#06x}:{snap}: {what}"
                                    );
                                } else {
                                    log::warn!("board: regulator warning status {word:#06x}:{snap}: {what}");
                                }
                            }
                            update(|s| {
                                s.power_fault = Some(what);
                                s.power_fault_codes = f.iter().map(Condition::code).collect();
                            });
                        }
                        Verdict::Clear => {
                            if changed {
                                log::info!("board: regulator status clear again");
                            }
                            update(|s| {
                                s.power_fault = None;
                                s.power_fault_codes.clear();
                            });
                        }
                        Verdict::Busy | Verdict::Disabled => {}
                    }
                }
            }
            if (hot_chip || hot_vr || blind || power_fault) && !overheat() {
                OVERHEAT.store(true, Ordering::Relaxed);
                let cause = if blind {
                    Cause::Blind
                } else if hot_chip || hot_vr {
                    Cause::Hot
                } else {
                    Cause::PowerFault
                };
                let mut g = GUARD.lock().unwrap();
                g.trip(now_s, cause);
                if g.latched() {
                    log::error!("board: third overheat stop within an hour: stays off until cleared by hand");
                }
                drop(g);
                if blind {
                    log::error!("board: no chip temperature for {temp_lost_s} s: core voltage off");
                    update(|s| s.last_error = "temp: sensor lost, core voltage off".into());
                    crate::events::record(
                        "overheat",
                        warpaxe_diag::events::detail::blind(temp_lost_s),
                    );
                } else if cause == Cause::PowerFault {
                    log::error!(
                        "board: regulator fault: core voltage off, retry after {} s",
                        warpaxe_board::overheat::MIN_OFF_S
                    );
                    let codes = state().power_fault_codes;
                    crate::events::record("power", warpaxe_diag::events::detail::power(&codes));
                } else {
                    log::error!(
                        "board: OVERHEAT chip {temp:?} °C (limit {chip_limit}), regulator {vr_temp:?} °C (limit {}): core voltage off",
                        profile.vr_stop_c
                    );
                    let chips = state().chip_temps;
                    if !chips.is_empty() {
                        log::error!("board: chip temperatures {chips:?}");
                    }
                    crate::events::record(
                        "overheat",
                        warpaxe_diag::events::detail::hot(
                            temp,
                            chip_limit,
                            vr_temp.map(i32::from),
                            profile.vr_stop_c,
                        ),
                    );
                }
                if let Some(vr) = vr.as_mut() {
                    vr.off();
                }
                asic_power(false);
                hold_reset_without_regulator(profile, vr.is_none());
                pf.disabled();
                POWERED.store(false, Ordering::Relaxed);
                CHIPS_ANSWERED.store(false, Ordering::Relaxed);
                if let Some(t) = thermal.as_mut() {
                    t.set_fans(100, 100);
                }
            } else if overheat() {
                let mut g = GUARD.lock().unwrap();
                let cooled = g.may_resume(
                    now_s,
                    &Reading {
                        chip_c: temp,
                        chip_limit_c: chip_limit,
                        vr_c: vr_temp.map(|c| c as f32),
                        vr_limit_c: profile.vr_stop_c,
                        vr_expected: vr_temp_seen,
                        chip_expected: sensor.seen(),
                        // Off, and the sensor that read while the chips
                        // ran gives nothing now.
                        chip_dark_off: !rail_on && temp.is_none() && sensor.seen(),
                    },
                );
                if cooled {
                    let was_hot = matches!(g.stopped(), Some(Cause::Hot));
                    g.resumed();
                    drop(g);
                    sensor.clear();
                    OVERHEAT.store(false, Ordering::Relaxed);
                    log::warn!(
                        "board: cooled down (chip {temp:?} °C, regulator {vr_temp:?} °C): core voltage back on"
                    );
                    // A board without a fan to drive cannot cool itself at
                    // the same clock: it comes back one option lower (the
                    // stock NerdNOS firmware drops to 210 MHz for good).
                    if was_hot && !profile.has_fan() {
                        if let Some((from, to)) = crate::miner::step_down_after_overheat() {
                            log::warn!(
                                "board: no fan to drive: the chip comes back at {to} MHz instead of {from}; a restart begins at the configured clock again"
                            );
                        }
                    }
                    update(|s| {
                        if s.last_error.starts_with("temp:") {
                            s.last_error.clear();
                        }
                    });
                }
            }
            // Queued events go to flash from here: this thread's stack is
            // internal.
            crate::events::flush();
            let (latched, trips, cause) = {
                let g = GUARD.lock().unwrap();
                let cause = g.stopped().map(|c| match c {
                    Cause::Hot => "hot",
                    Cause::Blind => "sensor",
                    Cause::PowerFault => "power",
                });
                (g.latched(), g.recent_trips(now_s) as u8, cause)
            };
            update(|s| {
                s.temp_c = temp;
                s.vr_temp_c = vr_temp;
                s.vin_mv = vin.map(|v| v * 1000.0);
                s.vout_mv = vout.map(|v| v * 1000.0);
                s.iout_ma = current.map(|a| a * 1000.0);
                s.power_w = power;
                s.power_estimated = power_estimated;
                s.core_current_a = core_current;
                s.core_current_warn_a = core_current_warn;
                s.fan_rpm = rpm.0;
                s.fan2_rpm = rpm.1;
                s.fan_auto = cfg.autofanspeed;
                s.overheat = overheat();
                s.overheat_latched = latched;
                s.overheat_trips = trips;
                s.overheat_cause = cause;
                s.shutdown = shutdown();
                if let Some((st, _)) = status {
                    s.vr_status = st;
                }
            });
        }

        // Core rail down on an overheat or a manual shutdown, back up once
        // both are cleared; the miner restarts the chain after that.
        if CLEAR_LOST.swap(false, Ordering::Relaxed) {
            sensor.clear();
        }
        let halt = overheat() || shutdown() || no_pool();
        if halt && POWERED.load(Ordering::Relaxed) {
            if let Some(vr) = vr.as_mut() {
                vr.off();
            }
            asic_power(false);
            hold_reset_without_regulator(profile, vr.is_none());
            pf.disabled();
            POWERED.store(false, Ordering::Relaxed);
            CHIPS_ANSWERED.store(false, Ordering::Relaxed);
            log::warn!("board: core voltage off");
            // Off for want of a pool: a warm-up still under way is over
            // (its end would hand the fans to the PID from full duty,
            // with nothing left to cool); the rail's return starts one.
            if no_pool() && warmup.take().is_some() {
                update(|s| s.fan_warmup = false);
            }
        } else if !halt && !POWERED.load(Ordering::Relaxed) {
            // The chip power-enable line first (ESP-Miner's order); on a
            // board without a regulator the chain comes back when the
            // miner pulses the reset.
            asic_power(true);
            let ok = match vr.as_mut() {
                Some(vr) => {
                    // Latched fault bits stay until cleared; a fault that
                    // is still there trips the guard again after the grace.
                    vr.clear_faults();
                    match vr.on(profile.vout_for_core_mv(applied_mv)) {
                        Ok(()) => true,
                        Err(e) => {
                            log::error!("board: core voltage back on failed: {e}");
                            false
                        }
                    }
                }
                None => true,
            };
            if ok {
                // Let the rail settle before the miner resets the chain.
                std::thread::sleep(Duration::from_millis(500));
                if overheat() || shutdown() || no_pool() {
                    // A stop asked for while the rail came up (a
                    // `prepare_reset` right after a chips-on or at the
                    // end of an off/on cycle: it sees the rail still
                    // reported off and runs its own `rail_off` at once):
                    // off again, instead of reporting the rail up under
                    // it, which could leave it on at the reset.
                    if let Some(vr) = vr.as_mut() {
                        vr.off();
                    }
                    asic_power(false);
                    hold_reset_without_regulator(profile, vr.is_none());
                    log::warn!("board: core voltage off again, stopped while it came up");
                } else {
                    pf.enabled(t0.elapsed().as_secs() as u32);
                    POWERED.store(true, Ordering::Relaxed);
                    // The lost-sensor count starts anew with the rail.
                    sensor.clear();
                    // And the second diode's settle window (the miner's
                    // reset pulse starts it over).
                    chips_started();
                    // Start the fans from full again, as at boot: the PID
                    // ran down to its minimum over cold chips while the
                    // core was off, and a Gaia restarted at 420 MHz then
                    // heated its regulator to 76 °C in 45 s at 25 % fan
                    // and tripped its over-current limit.
                    pid.reset(100.0, profile.fan_target_c);
                    pid_vr.reset(100.0, vr_fan_target_c(profile));
                    warmup = Some(Warmup::start(t0.elapsed().as_secs() as u32));
                    update(|s| s.fan_warmup = true);
                    log::warn!("board: core voltage back on, {applied_mv} mV");
                }
            }
        }

        // Off for want of a pool, for as long as that takes: the chain
        // in reset on every board, as `quiesce` leaves it at boot (the
        // TPS546 boards otherwise keep the line released with the core
        // gone). By state, not at the switch-off: the rail may have
        // gone down for another stop first. The miner's reset pulse
        // releases the line again before the next init.
        if !POWERED.load(Ordering::Relaxed) {
            if no_pool() && !pool_reset_held {
                // Tried again next pass if the pin did not take it (an
                // expander pin's write can fail on the bus).
                pool_reset_held = RESET
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_none_or(|pin| pin.set(false));
                if !pool_reset_held && !pool_reset_failed {
                    pool_reset_failed = true;
                    log::warn!("board: the reset line did not take its level, trying again");
                }
            }
        } else {
            pool_reset_held = false;
            pool_reset_failed = false;
        }

        // A `cycle_chips` and the rail down (this pass or, when another
        // stop took it down first, an earlier one): its off time starts.
        if CYCLE.load(Ordering::Relaxed) && !POWERED.load(Ordering::Relaxed) && cycle_off_at.is_none() {
            cycle_off_at = Some(Instant::now());
        }
        // The chips' off time of a `cycle_chips` is over: lift its
        // shutdown (an API request meanwhile has cleared CYCLE and
        // stands; the loop above switches the rail back on next pass).
        if let Some(t) = cycle_off_at {
            if !CYCLE.load(Ordering::Relaxed) {
                cycle_off_at = None;
            } else if t.elapsed() >= Duration::from_millis(u64::from(warm_boot::WARM_OFF_MS)) {
                cycle_off_at = None;
                // The shutdown first: `cycling()` stays true until it is
                // lifted, for whoever waits on the two (the self-test).
                SHUTDOWN.store(false, Ordering::Relaxed);
                CYCLE.store(false, Ordering::Relaxed);
                log::warn!("board: chips back on after the off/on cycle");
            }
        }

        // A new core voltage from the API or the chain coming up. A value
        // the regulator refuses does not count as applied: the rail
        // stays where it is, the API shows the refusal, and the request
        // is tried again once a second (a bus hiccup passes, a value
        // outside the window keeps being refused until a new one comes).
        // A drop that came with a lower clock waits until the mining
        // loop has ramped the chips down to it (625 to 425 MHz takes
        // 3.2 s, the voltage one pass): a raise, a drop on its own and
        // the bring-up voltage's drop after the init go at once.
        let want = WANTED_MV.load(Ordering::Acquire);
        let held = want != 0
            && match core_hold.decide(
                t0.elapsed().as_millis() as u64,
                applied_mv,
                want,
                crate::miner::clock(),
            ) {
                Step::Apply => {
                    if core_held_logged {
                        core_held_logged = false;
                        if POWERED.load(Ordering::Relaxed) {
                            log::info!("board: the clock is down, core {want} mV goes now");
                        }
                    }
                    false
                }
                Step::Hold => {
                    if !core_held_logged {
                        core_held_logged = true;
                        log::info!(
                            "board: core {want} mV waits for the clock to come down (now {applied_mv} mV)"
                        );
                    }
                    true
                }
                Step::ApplyLate => {
                    core_held_logged = false;
                    log::warn!(
                        "board: the clock's ramp has not ended in {} s, core {want} mV goes now",
                        core_hold::MAX_HOLD_MS / 1000
                    );
                    false
                }
            };
        if want != 0
            && want != applied_mv
            && !held
            && POWERED.load(Ordering::Relaxed)
            && Instant::now() >= core_retry_at
        {
            let result = match vr.as_mut() {
                Some(vr) => vr.set_core(profile.vout_for_core_mv(want)),
                None => Ok(()),
            };
            match result {
                Ok(()) => {
                    log::info!("board: core {want} mV");
                    applied_mv = want;
                    core_refused_logged = None;
                    update(|s| {
                        s.core_mv_refused = None;
                        if s.last_error.starts_with("core:") {
                            s.last_error.clear();
                        }
                    });
                }
                Err(e) => {
                    if core_refused_logged != Some(want) {
                        log::warn!(
                            "board: core {want} mV refused: {e}, staying at {applied_mv} mV"
                        );
                        core_refused_logged = Some(want);
                    }
                    core_retry_at = Instant::now() + Duration::from_secs(1);
                    update(|s| {
                        s.core_mv_refused = Some(want);
                        s.last_error = format!("core: {want} mV refused: {e}");
                    });
                }
            }
        }

        // Fans, every sample period.
        if let Some(t) = thermal.as_mut() {
            let target_c = if cfg.pid_target_temp > 0 {
                f32::from(cfg.pid_target_temp)
            } else {
                profile.fan_target_c
            };
            let vr_target_c = if cfg.vr_target_temp > 0 {
                f32::from(cfg.vr_target_temp)
            } else {
                vr_fan_target_c(profile)
            };
            // The warm-up hold, and its handover to the PID from full duty.
            let now_s = t0.elapsed().as_secs() as u32;
            let hold = warmup
                .as_mut()
                .map(|w| w.holding(now_s, temp, target_c))
                .unwrap_or(false);
            if let Some(w) = warmup.as_ref().filter(|w| w.done()) {
                log::info!(
                    "board: warm-up over after {} s at {temp:?} °C: fans to the PID",
                    now_s.saturating_sub(w.started_s())
                );
                pid.reset(100.0, temp.unwrap_or(target_c));
                pid_vr.reset(100.0, vr_temp.map_or(vr_target_c, |c| c as f32));
                warmup = None;
                update(|s| s.fan_warmup = false);
            }
            // Chips off for want of a pool: nothing heats, so a fan set
            // by hand or one without a reading runs at the lowest duty
            // instead of its setting or full (the PID gets there on its
            // own as the chips cool). An overheat still means full.
            let idle = no_pool() && !POWERED.load(Ordering::Relaxed);
            // Off on purpose (no pool, by hand, the firmware's own off
            // and on) and no reading: nothing heats and nothing is
            // wrong, so not the full fan a missing reading means while
            // the chips run.
            let dark = !POWERED.load(Ordering::Relaxed) && (no_pool() || shutdown());
            let low = cfg.min_fan_speed.min(100);
            // A chip whose diode has never read runs unwatched: full
            // fan while the rail is on.
            let unwatched = diode_never_read && POWERED.load(Ordering::Relaxed);
            // A second diode's reading over the line, held back while
            // it settles after the chips' start: full fan meanwhile.
            let full = overheat() || FANS_FULL.load(Ordering::Relaxed) || unwatched || settle_held;
            let want = if full {
                (100, 100)
            } else {
                let chip = if !cfg.autofanspeed {
                    if idle {
                        cfg.fanspeed.min(low)
                    } else {
                        cfg.fanspeed.min(100)
                    }
                } else if hold && !idle {
                    100
                } else {
                    match temp {
                        Some(c) => pid.compute(c, target_c) as u8,
                        None if dark => low,
                        None => 100, // no reading: full fan
                    }
                };
                let vrf = if !cfg.autofanspeed2 {
                    if idle {
                        cfg.manual_fan_speed2.min(low)
                    } else {
                        cfg.manual_fan_speed2.min(100)
                    }
                } else if hold && !idle {
                    100
                } else {
                    match vr_temp {
                        Some(c) => pid_vr.compute(c as f32, vr_target_c) as u8,
                        None if dark => low,
                        None => 100,
                    }
                };
                (chip, if cfg.link_fans { chip } else { vrf })
            };
            // The self-test's fan step: at least the asked duty on both
            // outputs, never less than the loop wants.
            let want = match fan_boost_now() {
                Some(p) => (want.0.max(p), want.1.max(p)),
                None => want,
            };
            // One fan output: the second value mirrors the first. On the
            // NerdAxe boards that one fan also cools a regulator running
            // near its current limit, so it follows whichever of chip and
            // regulator needs more (the Bitaxe boards keep AxeOS's chip-only
            // fan).
            let two = matches!(t, Thermal::Nerd { .. });
            let want = if two {
                want
            } else if profile.name.starts_with("nerdaxe") {
                let one = want.0.max(want.1);
                (one, one)
            } else {
                (want.0, want.0)
            };
            if want != fans && t.set_fans(want.0, want.1) {
                fans = want;
            }
            // Reported every pass, not only on a change: a manual 100 %
            // equal to the start-up duty was shown as 0 % / "–". A board
            // without a fan reports none.
            let shown = if matches!(t, Thermal::Ntc { .. }) {
                (0, 0)
            } else {
                fans
            };
            update(|s| {
                s.fan_pct = shown.0;
                // The Nano's second fan runs at the first one's speed.
                s.fan2_pct = (two || matches!(t, Thermal::Nano(_))).then_some(shown.1);
            });
        }
        std::thread::sleep(Duration::from_millis(u64::from(SAMPLE_MS)));
    }
}

/// A board without a regulator has one way to stop its chip: the reset
/// line. Held low here on an overheat stop or a shutdown (the NerdNOS,
/// whose stock firmware drops the same line as "PEN"); the miner's
/// `reset_chain` releases it when the chain may run again.
fn hold_reset_without_regulator(profile: &Profile, no_regulator: bool) {
    if !no_regulator || profile.asic_reset.is_none() {
        return;
    }
    if let Some(pin) = RESET.lock().unwrap().as_ref() {
        pin.set(false);
        log::warn!("board: chip held in reset (no regulator to switch off)");
    }
}

/// `v` if it lies within `lo..=hi`, else `None`; the first few rejects are
/// logged.
fn plausible(v: Option<f32>, lo: f32, hi: f32, what: &str) -> Option<f32> {
    static REJECTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let v = v?;
    if v.is_finite() && (lo..=hi).contains(&v) {
        return Some(v);
    }
    if REJECTS.fetch_add(1, Ordering::Relaxed) < 20 {
        log::warn!("board: implausible {what} reading {v} ignored");
    }
    None
}

/// Outcome of a first-start detection or of a board choice, for the API
/// and the web UI.
#[derive(Clone, Debug, Serialize)]
pub struct DetectInfo {
    /// "detected" (taken by the detection, this boot), "chosen" (named
    /// in the settings, verified on this boot or not), "choose" (chips
    /// stay off until one is picked), "unsupported", "rejected" (the
    /// chosen board does not fit the bus) or "mismatch" (its chips did
    /// not answer as the profile says).
    pub status: &'static str,
    pub why: &'static str,
    /// Board names that fit, best guess first.
    pub candidates: Vec<&'static str>,
    /// Settings taken from the stock firmware.
    pub imported: Vec<&'static str>,
    /// "unsupported": the board that was recognised.
    pub model: Option<&'static str>,
    /// The board under test: the one named in the settings or taken by
    /// the detection.
    pub chosen: Option<&'static str>,
    /// Where its verification stands.
    pub choice: Option<Choice>,
    /// "rejected": why (`warpaxe_board::choice::Reason` as JSON).
    pub reason: Option<serde_json::Value>,
    /// "mismatch": what the enumeration found.
    pub detail: Option<serde_json::Value>,
    /// The core voltage of the pending first start, mV (0 = the
    /// profile's own).
    #[serde(rename = "cautiousMv")]
    pub cautious_mv: u16,
    /// What the probe found, read-only.
    pub bus: serde_json::Value,
}

static DETECT: Mutex<Option<DetectInfo>> = Mutex::new(None);

/// What the first-start probe found on both pin pairs, kept for the
/// plausibility check of a chosen board and for the API.
#[derive(Clone, Debug, Default)]
struct Found {
    bitaxe: Vec<u8>,
    nerd: Vec<u8>,
    /// The Nerd pair was probed at all (not when the Bitaxe pair named
    /// the board on its own: GPIO43/44 are the console UART there).
    nerd_probed: bool,
    vin_v: Option<f32>,
    code: Option<u16>,
    thermistor: bool,
}

impl Found {
    fn evidence(&self) -> choice::Evidence<'_> {
        choice::Evidence {
            bitaxe_bus: &self.bitaxe,
            nerd_bus: &self.nerd,
            vin_v: self.vin_v,
            tps5364x_code: self.code,
            thermistor: self.thermistor,
        }
    }

    fn json(&self) -> serde_json::Value {
        let hex = |v: &[u8]| v.iter().map(|a| format!("{a:#04x}")).collect::<Vec<_>>();
        serde_json::json!({
            "bitaxe": hex(&self.bitaxe), "nerd": hex(&self.nerd), "nerdProbed": self.nerd_probed,
            "vin": self.vin_v, "tps5364xCode": self.code.map(|c| format!("{c:#06x}")),
            "thermistor": self.thermistor,
        })
    }
}

static FOUND: Mutex<Option<Found>> = Mutex::new(None);

/// What one pin pair answered.
struct Probe {
    found: Vec<u8>,
    /// READ_VIN of a TPS546 that answered, V (a plausible value only).
    vin_v: Option<f32>,
    /// MFR_SPECIFIC_44 of a TPS5364x that answered.
    code: Option<u16>,
}

/// Both pin pairs (the Nerd pair only when the Bitaxe pair did not name
/// the board on its own), plus the NerdNOS's thermistor when nothing
/// answered anywhere. Read-only, as `probe` is.
fn gather(i2c0: &mut I2C0) -> Found {
    let b = probe(i2c0, 47, 48);
    let mut f = Found {
        bitaxe: b.found,
        vin_v: b.vin_v,
        code: b.code,
        ..Default::default()
    };
    if !matches!(
        detect::from_bus(&f.bitaxe, &[], f.vin_v),
        Detection::Board(..)
    ) {
        let n = probe(i2c0, 44, 43);
        f.nerd_probed = true;
        f.nerd = n.found;
        f.vin_v = f.vin_v.or(n.vin_v);
        f.code = f.code.or(n.code);
    }
    if f.bitaxe.is_empty() && f.nerd.is_empty() {
        // The NerdNOS carrier's thermistor divider on GPIO2, read once
        // (read-only; a bare board's pin floats).
        let (samples, ntc) = crate::ntc::probe(warpaxe_board::ntc::Config::NERDNOS);
        log::info!(
            "board: GPIO2 reads {samples:?} mV: {}",
            match ntc {
                Some(t) => format!("a thermistor at {t:.1} °C (NerdNOS?)"),
                None => "no thermistor divider".into(),
            }
        );
        f.thermistor = ntc.is_some();
    }
    *FOUND.lock().unwrap() = Some(f.clone());
    f
}

/// A reason the chosen board does not fit, for the API.
fn reason_json(r: choice::Reason) -> serde_json::Value {
    let mut v = serde_json::json!({ "code": r.code() });
    if let Some(p) = r.part() {
        v["part"] = serde_json::Value::from(p.name());
        v["addr"] = serde_json::Value::from(format!("{:#04x}", p.addr()));
    }
    if let choice::Reason::InputVoltage { read_dv, nominal_v } = r {
        v["readV"] = serde_json::Value::from(f32::from(read_dv) / 10.0);
        v["nominalV"] = serde_json::Value::from(nominal_v);
    }
    v
}

fn reason_text(r: choice::Reason) -> String {
    match r {
        choice::Reason::Missing(p) => format!("the {} did not answer", p.name()),
        choice::Reason::Unexpected(p) => format!("a {} answered, which it does not have", p.name()),
        choice::Reason::InputVoltage { read_dv, nominal_v } => format!(
            "the regulator reads {:.1} V in, the board runs on {nominal_v} V",
            f32::from(read_dv) / 10.0
        ),
        choice::Reason::PartsAnswered => {
            "parts answered on the I²C bus, which it has none of".into()
        }
        choice::Reason::NoThermistor => "no thermistor divider on GPIO2".into(),
    }
}

/// A board named in the settings or taken by the detection, before any
/// of its pins is driven: refused when the bus says otherwise (the chips
/// stay off, the web UI shows why and what fits), else on its way to a
/// first start at the cautious voltage. A board without a chain (the
/// devkit) has nothing to count and is verified here.
fn gate(
    p: &'static Profile,
    found: &Found,
    status: &'static str,
    why: &'static str,
    imported: Vec<&'static str>,
) -> &'static Profile {
    let cfg = config::get();
    let e = found.evidence();
    match choice::check(p, &e) {
        Ok(()) => {
            let mv = choice::cautious_core_mv(p, &e, cfg.core_voltage);
            CAUTIOUS_MV.store(mv, Ordering::Relaxed);
            *DETECT.lock().unwrap() = Some(DetectInfo {
                status,
                why,
                candidates: vec![p.name],
                imported,
                model: None,
                chosen: Some(p.name),
                choice: None,
                reason: None,
                detail: None,
                cautious_mv: mv,
                bus: found.json(),
            });
            if p.asic_count == 0 {
                if let Err(e) = config::set_verified_board(p.name) {
                    log::warn!("board: saving the verified board failed: {e}");
                }
                set_choice(Some(Choice::Verified));
                log::info!("board: {} fits the bus; no chain to count", p.name);
            } else {
                // The entry present but not a name: a choice made under
                // this firmware, not yet verified (init_bus tells it from
                // a boardModel an earlier version saved).
                if config::verified_board().as_deref() != Some(config::BOARD_PENDING) {
                    if let Err(e) = config::set_verified_board(config::BOARD_PENDING) {
                        log::warn!("board: marking the choice pending failed: {e}");
                    }
                }
                set_choice(Some(Choice::Pending));
                let at = if mv > 0 {
                    format!(" at {mv} mV")
                } else {
                    String::new()
                };
                log::warn!(
                    "board: {} fits the bus; first start{at}, the chips are counted against the profile",
                    p.name
                );
                crate::events::record(
                    "board",
                    warpaxe_diag::events::detail::first_start(p.name, mv),
                );
                crate::events::flush();
            }
            p
        }
        Err(r) => {
            let candidates: Vec<&'static Profile> = choice::candidates(&e).collect();
            let names: Vec<&'static str> = candidates.iter().map(|c| c.name).collect();
            log::error!(
                "board: {} refused: {}; fits: {names:?}; chips stay off until a board is chosen",
                p.name,
                reason_text(r)
            );
            crate::events::record("board", warpaxe_diag::events::detail::refused(p.name, r));
            // On the main task, before the restart the web UI will ask
            // for; without a board thread nothing else flushes.
            crate::events::flush();
            *DETECT.lock().unwrap() = Some(DetectInfo {
                status: "rejected",
                why: "the chosen board does not fit what answered on the bus",
                candidates: names,
                imported,
                model: None,
                chosen: Some(p.name),
                choice: None,
                reason: Some(reason_json(r)),
                detail: None,
                cautious_mv: 0,
                bus: found.json(),
            });
            set_choice(Some(Choice::Rejected));
            warpaxe_board::profile::undetected_for(&candidates)
        }
    }
}

pub fn detect_info() -> Option<DetectInfo> {
    let d = DETECT.lock().unwrap().clone()?;
    // A board from the settings that passed its first start: nothing to
    // show, as before (`boardDetect` was null for a configured board).
    if d.status == "chosen" && d.choice == Some(Choice::Verified) {
        return None;
    }
    Some(d)
}

/// Which chips answer on a pin pair (read-only address probes), with the
/// TPS546's input voltage and the TPS5364x's device code where they
/// answered.
fn probe(i2c0: &mut I2C0, sda: i32, scl: i32) -> Probe {
    // SAFETY: the two pairs probed drive no power or reset line on any
    // supported board (warpaxe_board::detect); the pins are released when
    // the driver drops.
    let (sda_pin, scl_pin) = unsafe { (AnyIOPin::new(sda), AnyIOPin::new(scl)) };
    let Ok(mut bus) = I2cDriver::new(
        &mut *i2c0,
        sda_pin,
        scl_pin,
        &I2cConfig::new().baudrate(Hertz(100_000)),
    ) else {
        return Probe {
            found: Vec::new(),
            vin_v: None,
            code: None,
        };
    };
    use warpaxe_board::pmbus as pm;
    /// PMBus CAPABILITY (not used anywhere else).
    const CAPABILITY: u8 = 0x19;
    let mut found = Vec::new();
    let mut vin_v = None;
    let mut code = None;
    let tps546 = warpaxe_board::detect::addr::TPS546;
    let tps5364x = warpaxe_board::detect::addr::TPS5364X;
    for a in warpaxe_board::detect::addr::ALL {
        let mut b = [0u8; 2];
        // A TPS546 may not acknowledge a bare read (the BitForge Nano's
        // does not): ask it for its PMBus CAPABILITY instead. The
        // TPS5364x is asked for its device code, which also tells the
        // TPS53647 from the TPS53667.
        let ok = if a == tps546 {
            bus.write_read(a, &[CAPABILITY], &mut b[..1], 20).is_ok()
        } else if a == tps5364x {
            let ok = bus
                .write_read(a, &[tps53647::DEVICE_CODE_CMD], &mut b, 20)
                .is_ok();
            if ok {
                code = Some(u16::from_le_bytes(b));
            }
            ok
        } else {
            bus.read(a, &mut b[..1], 20).is_ok()
        };
        if ok {
            found.push(a);
        }
    }
    log::info!(
        "board: probe {sda}/{scl}: {found:02x?}{}",
        code.map(|c| format!(", TPS5364x device code {c:#06x}"))
            .unwrap_or_default()
    );
    if found.contains(&tps546) {
        // What the stock firmware left in the regulator (kept while the
        // 12 V stays), before our init overwrites it.
        let mut out = String::new();
        let mut on = false;
        for (name, cmd, len) in [
            ("OPERATION", pm::OPERATION, 1),
            ("ON_OFF_CONFIG", pm::ON_OFF_CONFIG, 1),
            ("PHASE", pm::PHASE, 1),
            ("VOUT_COMMAND", pm::VOUT_COMMAND, 2),
            ("VOUT_MODE", pm::VOUT_MODE, 1),
            ("VOUT_MAX", pm::VOUT_MAX, 2),
            ("VOUT_MIN", pm::VOUT_MIN, 2),
            ("VOUT_SCALE_LOOP", pm::VOUT_SCALE_LOOP, 2),
            ("FREQUENCY_SWITCH", pm::FREQUENCY_SWITCH, 2),
            ("VIN_ON", pm::VIN_ON, 2),
            ("VIN_OFF", pm::VIN_OFF, 2),
            ("VOUT_OV_FAULT", pm::VOUT_OV_FAULT_LIMIT, 2),
            ("VOUT_UV_FAULT", pm::VOUT_UV_FAULT_LIMIT, 2),
            ("IOUT_OC_FAULT", pm::IOUT_OC_FAULT_LIMIT, 2),
            ("IOUT_OC_FAULT_RESPONSE", pm::IOUT_OC_FAULT_RESPONSE, 1),
            ("IOUT_OC_WARN", pm::IOUT_OC_WARN_LIMIT, 2),
            ("OT_FAULT", pm::OT_FAULT_LIMIT, 2),
            ("OT_WARN", pm::OT_WARN_LIMIT, 2),
            ("VIN_OV_FAULT", pm::VIN_OV_FAULT_LIMIT, 2),
            ("VIN_UV_WARN", pm::VIN_UV_WARN_LIMIT, 2),
            ("SYNC_CONFIG", 0xE4, 1),
            ("STACK_CONFIG", 0xEC, 2),
            ("PIN_DETECT_OVERRIDE", 0xEE, 2),
        ] {
            let mut b = [0u8; 2];
            let r = bus.write_read(tps546, &[cmd], &mut b[..len], 20);
            if cmd == pm::OPERATION && r.is_ok() {
                on = b[0] & 0x80 != 0;
            }
            match (r, len) {
                (Ok(()), 1) => out += &format!(" {name}={:02x}", b[0]),
                (Ok(()), _) => out += &format!(" {name}={:04x}", u16::from_le_bytes(b)),
                (Err(_), _) => out += &format!(" {name}=?"),
            }
            // Short lines: the web log cuts long ones.
            if out.len() > 60 {
                log::info!("board: TPS546 as found:{out}");
                out.clear();
            }
        }
        if !out.is_empty() {
            log::info!("board: TPS546 as found:{out}");
        }
        // The input voltage on its own line, in volts (the as-found line
        // above was cut after this value in the web log, 2026-10-22). The
        // input class (5 or 12 V) tells a NerdAxe Gamma from a Gaia; a
        // part still coming up answers garbage, which is no reading.
        let mut b = [0u8; 2];
        match bus.write_read(tps546, &[pm::READ_VIN], &mut b, 20) {
            Ok(()) => {
                let raw = u16::from_le_bytes(b);
                let v = pm::linear11_to_f32(raw);
                if v.is_finite() && (1.0..=60.0).contains(&v) {
                    vin_v = Some(v);
                    log::info!("board: TPS546 READ_VIN {v:.2} V (raw {raw:#06x})");
                } else {
                    log::warn!("board: TPS546 READ_VIN implausible: {v} V (raw {raw:#06x})");
                }
            }
            Err(e) => log::warn!("board: TPS546 READ_VIN failed: {e}"),
        }
        // A stock firmware that was mining when the ESP restarted leaves
        // the core rail on: the chips sit powered, with nothing watching
        // their temperature until a board is chosen. Switch it off.
        if on {
            match bus.write(tps546, &[pm::OPERATION, 0x00], 20) {
                Ok(()) => {
                    log::warn!("board: the stock firmware left the core rail on: switched off")
                }
                Err(e) => log::error!("board: core rail left on, switching it off failed: {e}"),
            }
        }
    }
    Probe { found, vin_v, code }
}

/// No board configured: take settings from the stock firmware, then find
/// the board from its NVS or its chips. A board that is not unique (or has
/// never run WarpAxe) is offered in the web UI; until then nothing is
/// powered.
fn first_start(i2c0: &mut I2C0) -> &'static Profile {
    let stock = config::Stock::open();
    let imported = config::import_stock(&stock);
    if !imported.is_empty() {
        log::info!(
            "board: first start, took {} from the stock firmware",
            imported.join(", ")
        );
    }
    let keys = (
        stock.str("devicemodel"),
        stock.str("asicmodel"),
        stock.str("boardversion"),
    );
    let stock_keys = (keys.0.as_deref(), keys.1.as_deref(), keys.2.as_deref());
    // The bus is read even when the stock keys name the board: the
    // probe switches off a core rail the stock firmware left on, and the
    // board taken is checked against the bus like a chosen one.
    let found = gather(i2c0);
    let d = detect::detect(stock_keys, &found.bitaxe, &found.nerd, found.vin_v);
    match d {
        Detection::Board(p, why) => {
            log::warn!("board: detected {} ({why})", p.name);
            let mut cfg = config::get();
            cfg.board_model = p.name.into();
            if let Err(e) = config::save(&cfg) {
                log::warn!("board: saving the detected board failed: {e}");
            }
            gate(p, &found, "detected", why, imported)
        }
        Detection::Choose(c, why) => {
            // Nothing on either bus and no stock firmware keys: possibly a
            // bare T-Display-S3 (the NerdMiner devkit). What a NerdMiner
            // left in flash decides: its config.json found means the board
            // becomes the devkit, hashing on the CPU, with its wallet and
            // the Wi-Fi library's station taken over (the Wi-Fi only with
            // the wallet). Without it the owner chooses in the web
            // UI, where t-display-s3 is on offer; a board with chips whose
            // bus is dead must not quietly turn into a CPU miner. A board
            // with a boardModel in NVS never gets here (init_bus).
            let mut imported = imported;
            let mut c = c;
            let mut why = why;
            if c.is_empty() && stock_keys == (None, None, None) {
                // The NerdNOS carrier's thermistor divider on GPIO2 (read
                // by `gather`, read-only; a bare board's pin floats).
                let ntc = found.thermistor.then_some(());
                // The worker name the wallet gets: a chip board must not
                // share the devkit's at the pool (its VarDiff is keyed by
                // the full name).
                let worker = if ntc.is_some() {
                    warpaxe_cpuminer::nerdminer::WORKER_NERDNOS
                } else {
                    warpaxe_cpuminer::nerdminer::WORKER
                };
                imported.extend(crate::cpuminer::import_nerdminer(worker));
                let settings = imported.contains(&"wallet (nerdminer)");
                // The carrier's core rail on GPIO3: 1.4 V on a NerdNOS once
                // its buck runs. The buck's EN (GPIO13, "PEN") has a 10 k
                // pull-down, so the rail is off at every boot and GPIO3
                // read 0 mV on the .46's first start (2026-10-08). With the
                // thermistor there, the enable goes high for 150 ms for the
                // reading and back off: the chip stays in reset meanwhile
                // (RST floats high through R14 into the inverter), and on a
                // bare T-Display GPIO13 is a free header pin.
                let rail = ntc.is_some().then(|| {
                    esp_output(13, true);
                    std::thread::sleep(Duration::from_millis(150));
                    let mv = crate::ntc::Reader::new_pin(3).and_then(|r| r.millivolts());
                    esp_output(13, false);
                    unsafe {
                        sys::gpio_set_direction(13, sys::gpio_mode_t_GPIO_MODE_INPUT);
                    }
                    log::info!("board: GPIO3 reads {mv:?} mV with GPIO13 high (the NerdNOS core rail would be ~1400)");
                    mv
                }).flatten();
                match detect::bare_board(settings, ntc.is_some(), rail) {
                    Detection::Board(p, why) => {
                        log::warn!(
                            "board: {why}; {}; took {}",
                            if p.asic_count == 0 {
                                "hashing on the CPU"
                            } else {
                                "chips on"
                            },
                            imported.join(", ")
                        );
                        let mut cfg = config::get();
                        cfg.board_model = p.name.into();
                        if let Err(e) = config::save(&cfg) {
                            log::warn!("board: saving the {} profile failed: {e}", p.name);
                        }
                        return gate(p, &found, "detected", why, imported);
                    }
                    Detection::Choose(c2, why2) => {
                        c = c2;
                        why = why2;
                    }
                    Detection::Unsupported(..) => {}
                }
            }
            let p = warpaxe_board::profile::undetected_for(c);
            log::warn!(
                "board: not detected for sure ({why}); candidates {:?}; chips stay off until a board is chosen; display {:?}",
                c.iter().map(|p| p.name).collect::<Vec<_>>(),
                p.display
            );
            *DETECT.lock().unwrap() = Some(DetectInfo {
                status: "choose",
                why,
                candidates: c.iter().map(|p| p.name).collect(),
                imported,
                model: None,
                chosen: None,
                choice: None,
                reason: None,
                detail: None,
                cautious_mv: 0,
                bus: found.json(),
            });
            p
        }
        Detection::Unsupported(model, why) => {
            log::warn!("board: recognised {model} ({why}), which WarpAxe does not support yet; chips stay off");
            *DETECT.lock().unwrap() = Some(DetectInfo {
                status: "unsupported",
                why,
                candidates: Vec::new(),
                imported,
                model: Some(model),
                chosen: None,
                choice: None,
                reason: None,
                detail: None,
                cautious_mv: 0,
                bus: found.json(),
            });
            &warpaxe_board::profile::UNDETECTED
        }
    }
}

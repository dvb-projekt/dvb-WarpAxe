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

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver, I2C0};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::{self, EspError};
use serde::Serialize;
use warpaxe_board::emc2101::Emc2101;
use warpaxe_board::emc2103::Emc2103;
use warpaxe_board::emc2302::{Emc2302, Fan};
use warpaxe_board::fan::{FanPid, Warmup, SAMPLE_MS};
use warpaxe_board::fxl6408::{q13xx, Fxl6408};
use warpaxe_board::ina260::Ina260;
use warpaxe_board::overheat::{Cause, Guard, Reading};
use warpaxe_board::pca9544::Muxed;
use warpaxe_board::pmbus;
use warpaxe_board::power_fault::{self, Condition, Monitor, Verdict};
use warpaxe_board::tmp1075::{self, Addrs};
use warpaxe_board::tps53647::{self, Tps53647};
use warpaxe_board::tps546::Tps546;
use warpaxe_board::{Asic, I2c, Profile, Thermal as ThermalKind, Vr};
use warpaxe_core::chip::{bm1370::BM1370, bm1373::BM1373, ChipProfile};

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
    pub temp_c: Option<f32>,
    pub vr_temp_c: Option<i32>,
    pub vin_mv: Option<f32>,
    pub vout_mv: Option<f32>,
    /// Output current on the Bitaxe boards, input current on the Nerd
    /// boards — what AxeOS and NerdOS each report as `current`.
    pub iout_ma: Option<f32>,
    pub power_w: Option<f32>,
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
    pub last_error: String,
    /// What the regulator answered to each probe at boot.
    pub vr_probe: String,
}

static STATE: Mutex<Option<BoardState>> = Mutex::new(None);
static OVERHEAT: AtomicBool = AtomicBool::new(false);
/// When the chips may run again after an overheat stop.
static GUARD: Mutex<Guard> = Mutex::new(Guard::new());
/// Chips switched off by hand (web UI / API), not by a limit.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// The core rail is up; the miner starts the chain only then.
static POWERED: AtomicBool = AtomicBool::new(true);
/// Set by an overheat reset: the board loop forgets its sensor-loss count.
static CLEAR_LOST: AtomicBool = AtomicBool::new(false);
/// Set by [`prepare_reset`]: the fans at full until the reset.
static FANS_FULL: AtomicBool = AtomicBool::new(false);
/// The board thread runs, so it is the one switching the rail.
static BOARD_THREAD: AtomicBool = AtomicBool::new(false);
/// Core voltage the API asked for, mV; 0 = nothing pending.
static WANTED_MV: AtomicU16 = AtomicU16::new(0);
static RESET: Mutex<Option<Pin>> = Mutex::new(None);
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

pub fn profile() -> &'static Profile {
    PROFILE
        .lock()
        .unwrap()
        .unwrap_or(&warpaxe_board::T_DISPLAY_S3_DEVKIT)
}

/// The chip profile the board's chain is built from.
pub fn chip() -> &'static ChipProfile {
    match profile().asic {
        Asic::Bm1370 => &BM1370,
        Asic::Bm1373 => &BM1373,
    }
}

/// True once a temperature limit was crossed, until the board has cooled
/// down (or the stop is cleared by hand).
pub fn overheat() -> bool {
    OVERHEAT.load(Ordering::Relaxed)
}

/// Chips switched off by hand.
pub fn shutdown() -> bool {
    SHUTDOWN.load(Ordering::Relaxed)
}

/// The chain must not run: overheat, a manual shutdown, or the core rail
/// not (yet) back up.
pub fn chain_stopped() -> bool {
    overheat() || shutdown() || !POWERED.load(Ordering::Relaxed)
}

/// Boot, before anything slow: the chain in reset and the core rail off.
/// An ESP reset leaves the board as it was: the TPS546 keeps its
/// OPERATION through a software reset and in ROM download mode, the reset
/// line floats, and the chips sit powered at their last clock with
/// nothing watching them until [`start`] runs, seconds after USB and
/// Wi-Fi (BitForge Nano 2026-10-03: 82 °C chip, 95 °C regulator, 35 W at
/// uptime 5 s after three minutes of download mode). ESP-Miner holds the
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
/// PowerManagementTask::restart ("shutdown asics and LDOs before reset");
/// ESP-Miner restarts with the rail on. Returns once the board thread
/// reports the rail off, or after about two seconds.
pub fn prepare_reset(why: &str) {
    log::warn!("board: {why}: chips off before the reset");
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
}

/// Switch the chips off (core rail down) or back on; the board loop does
/// the switching, the miner restarts the chain once the rail is up.
pub fn set_shutdown(off: bool) {
    SHUTDOWN.store(off, Ordering::Relaxed);
    log::warn!(
        "board: chips switched {} by request",
        if off { "off" } else { "on" }
    );
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
    WANTED_MV.store(mv, Ordering::Relaxed);
}

/// The chain init is over, answered or not: drop from the bring-up
/// voltage to the configured one (NerdOS sets the final voltage after the
/// chain init). Called on a failed init too — a chain that never answered
/// must not sit at the bring-up voltage (1000 mV on a Gaia, above its
/// 970 mV cap) while the miner retries.
pub fn chain_ready() {
    let core = config::get().core_voltage;
    if profile().init_core_mv > core {
        request_core_mv(core);
    }
}

/// Pull the chain's reset low, then release it, with the given timing.
/// Nothing happens on a board without a reset line.
pub fn reset_chain(low_ms: u32, release_ms: u32) {
    if let Some(pin) = RESET.lock().unwrap().as_ref() {
        pin.set(false);
        std::thread::sleep(Duration::from_millis(u64::from(low_ms)));
        pin.set(true);
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
            Regulator::Tps546(vr, _) => vr.set_vout(volts).map_err(|e| format!("{e:?}")),
            Regulator::Tps5364x { vr, .. } => {
                vr.set_vout(volts).map(|_| ()).map_err(|e| format!("{e:?}"))
            }
        }
    }
    /// Core voltage back on at `volts` after [`off`](Self::off).
    fn on(&mut self, volts: f32) -> Result<(), String> {
        match self {
            Regulator::Tps546(vr, _) => vr.set_vout(volts).map_err(|e| format!("{e:?}")),
            Regulator::Tps5364x { vr, enable, .. } => {
                enable.set(true);
                vr.set_vout(volts).map(|_| ()).map_err(|e| format!("{e:?}"))
            }
        }
    }
    /// Core voltage off: the TPS546 by command, the Nerd regulators by
    /// their enable line (NerdOS `setVoltage(0)`).
    fn off(&mut self) {
        match self {
            Regulator::Tps546(vr, _) => {
                let _ = vr.set_vout(0.0);
            }
            Regulator::Tps5364x { enable, .. } => {
                enable.set(false);
            }
        }
    }
}

/// Chip temperature and fans, whichever the board carries.
enum Thermal {
    Emc2101(Emc2101<SharedI2c>),
    /// Gamma Turbo, Naja Duo, Gamma Hex: two diodes, one fan; the driver
    /// knows which diode is the chip.
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
}

impl Thermal {
    fn chip_temp_c(&mut self) -> Result<f32, String> {
        match self {
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
    /// RPM of the chip fan, and of the second output where there is one.
    fn rpm(&mut self) -> (u16, Option<u16>) {
        match self {
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
            Thermal::Emc2101(_) | Thermal::Emc2103(_) | Thermal::Nano(_) => true,
            Thermal::Nerd { fans, .. } => fans.set_polarity(invert).is_ok(),
        }
    }
    /// Drive the chip fan and, on the Nerd boards, the regulator fan.
    fn set_fans(&mut self, chip: u8, vr: u8) -> bool {
        match self {
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
    let profile = match Profile::by_name(&cfg.board_model) {
        Some(p) => p,
        None => {
            if !cfg.board_model.is_empty() {
                log::warn!("board: unknown boardModel {:?}, detecting", cfg.board_model);
            }
            first_start(&mut i2c0)
        }
    };
    *PROFILE.lock().unwrap() = Some(profile);
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
        // Probe the part step by step so a failure says where.
        let probe = format!(
            "vout_mode={:?} status={:?} id={:?}",
            vr.vout_mode(),
            vr.status_word(),
            vr.device_id()
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
                let mv = profile.init_core_mv.max(cfg.core_voltage);
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

    std::thread::sleep(Duration::from_millis(250));
    if let Some(l) = &ldo {
        l.set(true);
    }
    std::thread::sleep(Duration::from_millis(100));

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
    let mv = profile.init_core_mv.max(cfg.core_voltage);
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
    let probe = format!("device_id={:?}", vr.device_id());
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
    let mv = profile.init_core_mv.max(cfg.core_voltage);
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
        ThermalKind::None => None,
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
        .stack_size(6144)
        .spawn(move || run(profile, vr, thermal))
        .expect("board thread");
}

fn run(profile: &'static Profile, mut vr: Option<Regulator>, mut thermal: Option<Thermal>) {
    let mut cfg = config::get();
    let mut pid = FanPid::new(f32::from(cfg.min_fan_speed));
    pid.reset(100.0, profile.fan_target_c);
    let mut pid_vr = FanPid::new(f32::from(cfg.min_fan_speed));
    pid_vr.reset(100.0, vr_fan_target_c(profile));
    let mut applied_mv = cfg.core_voltage.max(profile.init_core_mv);
    let mut fans: (u8, u8) = (100, 100);
    let mut last_sample = Instant::now() - Duration::from_secs(1);
    let mut temp: Option<f32> = None;
    let mut vr_temp: Option<i32> = None;
    let mut polarity = cfg.invertfanpolarity;
    let mut temp_lost_s: u32 = 0;
    // A chip temperature has been read since boot: losing it then stops
    // the chain (Cause::Blind). A board whose sensor never answered runs
    // without the chip limit, fans at full, as NerdOS does with no
    // ASIC-side TMP1075 (it shows 0 °C). Seen 2026-10-01 on a NerdOctaxe
    // v2.2 under both Nerd profiles: "overheated" after exactly 60 s.
    let mut temp_seen = false;
    let mut no_sensor_warned = false;
    let t0 = Instant::now();
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
    loop {
        // Once a second: sensors, config, the sample.
        if last_sample.elapsed() >= Duration::from_secs(1) {
            last_sample = Instant::now();
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
            let mut rpm = (0, None);
            if let Some(t) = thermal.as_mut() {
                match t.chip_temp_c().and_then(|c| {
                    plausible(Some(c), -40.0, 150.0, "chip temp")
                        .ok_or_else(|| "implausible reading".into())
                }) {
                    Ok(c) => {
                        temp = Some(c);
                        temp_lost_s = 0;
                        temp_seen = true;
                        update(|s| {
                            if s.last_error.starts_with("temp:") {
                                s.last_error.clear();
                            }
                        });
                    }
                    Err(e) => {
                        temp = None;
                        temp_lost_s = temp_lost_s.saturating_add(1);
                        update(|s| s.last_error = format!("temp: {e}"));
                    }
                }
                rpm = t.rpm();
            }
            let chip_limit = if cfg.overheat_temp > 0 {
                f32::from(cfg.overheat_temp)
            } else {
                profile.max_temp_c
            };
            let hot_chip = temp.is_some_and(|c| c > chip_limit);
            let hot_vr = vr_temp.is_some_and(|c| c as f32 > profile.vr_max_c);
            let blind = temp_seen && temp_lost_s >= TEMP_LOST_S;
            if !temp_seen && temp_lost_s >= TEMP_LOST_S && !no_sensor_warned {
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
                } else if cause == Cause::PowerFault {
                    log::error!(
                        "board: regulator fault: core voltage off, retry after {} s",
                        warpaxe_board::overheat::MIN_OFF_S
                    );
                } else {
                    log::error!(
                        "board: OVERHEAT chip {temp:?} °C (limit {chip_limit}), regulator {vr_temp:?} °C (limit {}): core voltage off",
                        profile.vr_max_c
                    );
                }
                if let Some(vr) = vr.as_mut() {
                    vr.off();
                }
                asic_power(false);
                pf.disabled();
                POWERED.store(false, Ordering::Relaxed);
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
                        vr_limit_c: profile.vr_max_c,
                        vr_expected: vr_temp_seen,
                        chip_expected: temp_seen,
                    },
                );
                if cooled {
                    g.resumed();
                    drop(g);
                    temp_lost_s = 0;
                    OVERHEAT.store(false, Ordering::Relaxed);
                    log::warn!(
                        "board: cooled down (chip {temp:?} °C, regulator {vr_temp:?} °C): core voltage back on"
                    );
                    update(|s| {
                        if s.last_error.starts_with("temp:") {
                            s.last_error.clear();
                        }
                    });
                }
            }
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
            temp_lost_s = 0;
        }
        let halt = overheat() || shutdown();
        if halt && POWERED.load(Ordering::Relaxed) {
            if let Some(vr) = vr.as_mut() {
                vr.off();
            }
            asic_power(false);
            pf.disabled();
            POWERED.store(false, Ordering::Relaxed);
            log::warn!("board: core voltage off");
        } else if !halt && !POWERED.load(Ordering::Relaxed) {
            let ok = match vr.as_mut() {
                Some(vr) => {
                    // Latched fault bits stay until cleared; a fault that
                    // is still there trips the guard again after the grace.
                    asic_power(true);
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
                pf.enabled(t0.elapsed().as_secs() as u32);
                POWERED.store(true, Ordering::Relaxed);
                // Start the fans from full again, as at boot: the PID ran
                // down to its minimum over cold chips while the core was
                // off, and a Gaia restarted at 420 MHz then heated its
                // regulator to 76 °C in 45 s at 25 % fan and tripped its
                // over-current limit.
                pid.reset(100.0, profile.fan_target_c);
                pid_vr.reset(100.0, vr_fan_target_c(profile));
                warmup = Some(Warmup::start(t0.elapsed().as_secs() as u32));
                update(|s| s.fan_warmup = true);
                log::warn!("board: core voltage back on, {applied_mv} mV");
            }
        }

        // A new core voltage from the API or the chain coming up.
        let want = WANTED_MV.load(Ordering::Relaxed);
        if want != 0 && want != applied_mv && POWERED.load(Ordering::Relaxed) {
            if let Some(vr) = vr.as_mut() {
                match vr.set_core(profile.vout_for_core_mv(want)) {
                    Ok(()) => log::info!("board: core {want} mV"),
                    Err(e) => log::warn!("board: core {want} mV refused: {e}"),
                }
            }
            applied_mv = want;
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
            let want = if overheat() || FANS_FULL.load(Ordering::Relaxed) {
                (100, 100)
            } else {
                let chip = if !cfg.autofanspeed {
                    cfg.fanspeed.min(100)
                } else if hold {
                    100
                } else {
                    match temp {
                        Some(c) => pid.compute(c, target_c) as u8,
                        None => 100, // no reading: full fan
                    }
                };
                let vrf = if !cfg.autofanspeed2 {
                    cfg.manual_fan_speed2.min(100)
                } else if hold {
                    100
                } else {
                    match vr_temp {
                        Some(c) => pid_vr.compute(c as f32, vr_target_c) as u8,
                        None => 100,
                    }
                };
                (chip, if cfg.link_fans { chip } else { vrf })
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
            // equal to the start-up duty was shown as 0 % / "–".
            let shown = fans;
            update(|s| {
                s.fan_pct = shown.0;
                // The Nano's second fan runs at the first one's speed.
                s.fan2_pct = (two || matches!(t, Thermal::Nano(_))).then_some(shown.1);
            });
        }
        std::thread::sleep(Duration::from_millis(u64::from(SAMPLE_MS)));
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

/// Outcome of a first-start detection, for the API and the web UI.
#[derive(Clone, Debug, Serialize)]
pub struct DetectInfo {
    /// "detected" (board taken) or "choose" (chips stay off until one is
    /// picked).
    pub status: &'static str,
    pub why: &'static str,
    /// Board names that fit, best guess first.
    pub candidates: Vec<&'static str>,
    /// Settings taken from the stock firmware.
    pub imported: Vec<&'static str>,
    /// "unsupported": the board that was recognised.
    pub model: Option<&'static str>,
}

static DETECT: Mutex<Option<DetectInfo>> = Mutex::new(None);

pub fn detect_info() -> Option<DetectInfo> {
    DETECT.lock().unwrap().clone()
}

/// Which chips answer on a pin pair (read-only address probes).
fn probe(i2c0: &mut I2C0, sda: i32, scl: i32) -> Vec<u8> {
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
        return Vec::new();
    };
    use warpaxe_board::pmbus as pm;
    /// PMBus CAPABILITY (not used anywhere else).
    const CAPABILITY: u8 = 0x19;
    let mut found = Vec::new();
    let tps546 = warpaxe_board::detect::addr::TPS546;
    for a in warpaxe_board::detect::addr::ALL {
        let mut b = [0u8; 1];
        // A TPS546 may not acknowledge a bare read (the BitForge Nano's
        // does not): ask it for its PMBus CAPABILITY instead.
        let ok = if a == tps546 {
            bus.write_read(a, &[CAPABILITY], &mut b, 20).is_ok()
        } else {
            bus.read(a, &mut b, 20).is_ok()
        };
        if ok {
            found.push(a);
        }
    }
    log::info!("board: probe {sda}/{scl}: {found:02x?}");
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
    found
}

/// No board configured: take settings from the stock firmware, then find
/// the board from its NVS or its chips. A board that is not unique (or has
/// never run WarpAxe) is offered in the web UI; until then nothing is
/// powered.
fn first_start(i2c0: &mut I2C0) -> &'static Profile {
    use warpaxe_board::detect::{self, Detection};
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
    let d = match detect::from_stock(stock_keys.0, stock_keys.1, stock_keys.2) {
        Some(_) => detect::detect(stock_keys, &[], &[]),
        None => {
            let bitaxe = probe(i2c0, 47, 48);
            let nerd = if matches!(detect::from_bus(&bitaxe, &[]), Detection::Board(..)) {
                Vec::new()
            } else {
                probe(i2c0, 44, 43)
            };
            detect::detect(stock_keys, &bitaxe, &nerd)
        }
    };
    match d {
        Detection::Board(p, why) => {
            log::warn!("board: detected {} ({why})", p.name);
            let mut cfg = config::get();
            cfg.board_model = p.name.into();
            if let Err(e) = config::save(&cfg) {
                log::warn!("board: saving the detected board failed: {e}");
            }
            *DETECT.lock().unwrap() = Some(DetectInfo {
                status: "detected",
                why,
                candidates: vec![p.name],
                imported,
                model: None,
            });
            p
        }
        Detection::Choose(c, why) => {
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
            });
            &warpaxe_board::profile::UNDETECTED
        }
    }
}

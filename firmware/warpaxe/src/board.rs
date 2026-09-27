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
use warpaxe_board::emc2302::{Emc2302, Fan};
use warpaxe_board::fan::{FanPid, SAMPLE_MS};
use warpaxe_board::fxl6408::{q13xx, Fxl6408};
use warpaxe_board::tmp1075::{self, Addrs};
use warpaxe_board::tps53647::Tps53647;
use warpaxe_board::tps546::Tps546;
use warpaxe_board::{Asic, I2c, Profile, Thermal as ThermalKind, Vr};
use warpaxe_core::chip::{bm1370::BM1370, bm1373::BM1373, ChipProfile};

use crate::config;

/// The regulator-temperature target of the second fan channel on the
/// Nerd boards (NerdOS `m_pidSettings[1].targetTemp`).
const VR_FAN_TARGET_C: f32 = 65.0;

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
    pub fan_pct: u8,
    pub fan_rpm: u16,
    /// The second fan output (Nerd boards: M1, on the regulator PID).
    pub fan2_pct: Option<u8>,
    pub fan2_rpm: Option<u16>,
    pub fan_auto: bool,
    pub vr_status: u16,
    /// Regulator population found at boot (Nerd boards).
    pub vr_part: String,
    /// Mining stopped and the regulator switched off; stays until reboot.
    pub overheat: bool,
    pub last_error: String,
    /// What the regulator answered to each probe at boot.
    pub vr_probe: String,
}

static STATE: Mutex<Option<BoardState>> = Mutex::new(None);
static OVERHEAT: AtomicBool = AtomicBool::new(false);
/// Core voltage the API asked for, mV; 0 = nothing pending.
static WANTED_MV: AtomicU16 = AtomicU16::new(0);
static RESET: Mutex<Option<Pin>> = Mutex::new(None);
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

/// True once a temperature limit was crossed: the miner must not run.
pub fn overheat() -> bool {
    OVERHEAT.load(Ordering::Relaxed)
}

/// A new core voltage from `PATCH /api/system`, applied by the board task.
pub fn request_core_mv(mv: u16) {
    WANTED_MV.store(mv, Ordering::Relaxed);
}

/// The chain answered: drop from the bring-up voltage to the configured
/// one (NerdOS sets the final voltage after the chain init).
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

/// The core regulator, whichever the board carries.
enum Regulator {
    Tps546(Tps546<SharedI2c>),
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
            Regulator::Tps546(vr) => vr.temperature_c().ok(),
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
            Regulator::Tps546(vr) => vr.vin().ok(),
            Regulator::Tps5364x { vr, .. } => vr.vin().ok(),
        }
    }
    fn vout(&mut self) -> Option<f32> {
        match self {
            Regulator::Tps546(vr) => vr.vout().ok(),
            Regulator::Tps5364x { vr, .. } => vr.vout().ok(),
        }
    }
    /// The current the stock firmware reports: IOUT on the TPS546,
    /// IIN on the Nerd regulators.
    fn current_a(&mut self) -> Option<f32> {
        match self {
            Regulator::Tps546(vr) => vr.iout().ok(),
            Regulator::Tps5364x { vr, .. } => vr.iin().ok(),
        }
    }
    /// Board power: VOUT × IOUT + offset on the TPS546 boards (ESP-Miner),
    /// the regulator's input power on the Nerd boards (NerdOS).
    fn power_w(&mut self, vout: Option<f32>, current: Option<f32>, offset: f32) -> Option<f32> {
        match self {
            Regulator::Tps546(_) => match (vout, current) {
                (Some(v), Some(i)) => Some(v * i + offset),
                _ => None,
            },
            Regulator::Tps5364x { vr, .. } => vr.pin().ok(),
        }
    }
    fn status(&mut self) -> Option<u16> {
        match self {
            Regulator::Tps546(vr) => vr.status_word().ok(),
            Regulator::Tps5364x { vr, .. } => vr.status_word().ok(),
        }
    }
    fn set_core(&mut self, volts: f32) -> Result<(), String> {
        match self {
            Regulator::Tps546(vr) => vr.set_vout(volts).map_err(|e| format!("{e:?}")),
            Regulator::Tps5364x { vr, .. } => {
                vr.set_vout(volts).map(|_| ()).map_err(|e| format!("{e:?}"))
            }
        }
    }
    /// Core voltage off: the TPS546 by command, the Nerd regulators by
    /// their enable line (NerdOS `setVoltage(0)`).
    fn off(&mut self) {
        match self {
            Regulator::Tps546(vr) => {
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
            Thermal::Nerd { fans, .. } => (
                fans.rpm(Fan::Two).unwrap_or(0),
                Some(fans.rpm(Fan::One).unwrap_or(0)),
            ),
        }
    }
    /// PWM polarity of the Nerd fan outputs; nothing on the EMC2101.
    fn set_polarity(&mut self, invert: bool) -> bool {
        match self {
            Thermal::Emc2101(_) => true,
            Thermal::Nerd { fans, .. } => fans.set_polarity(invert).is_ok(),
        }
    }
    /// Drive the chip fan and, on the Nerd boards, the regulator fan.
    fn set_fans(&mut self, chip: u8, vr: u8) -> bool {
        match self {
            Thermal::Emc2101(t) => t.set_fan_percent(chip).is_ok(),
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
pub fn init_bus(i2c0: I2C0) {
    let cfg = config::get();
    let profile = Profile::by_name(&cfg.board_model).unwrap_or_else(|| {
        log::warn!(
            "board: unknown boardModel {:?}, using the devkit profile",
            cfg.board_model
        );
        &warpaxe_board::T_DISPLAY_S3_DEVKIT
    });
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
    let Some(bus) = i2c() else {
        return;
    };
    if matches!(profile.vr, Vr::Tps5364x { .. }) {
        start_nerd(profile, bus);
    } else {
        start_bitaxe(profile, bus);
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
                update(|s| s.vr_status = status);
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
    let vr = vr.filter(|_| state().has_vr).map(Regulator::Tps546);

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
    let thermal = thermal
        .filter(|_| state().has_thermal)
        .map(Thermal::Emc2101);

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

    // Which regulator is fitted.
    let regulator = match alt {
        Some(a) => {
            let level = match &expander {
                Some(x) => x.lock().unwrap().read_pin(a.pin).unwrap_or(!a.level),
                None => esp_read_pulldown(a.pin),
            };
            if level == a.level {
                a.regulator
            } else {
                regulator
            }
        }
        None => regulator,
    };
    let part = format!("{:?} {} phases", regulator.variant, regulator.phases);
    log::info!("board: regulator {part}");
    update(|s| s.vr_part = part);

    std::thread::sleep(Duration::from_millis(250));
    if let Some(l) = &ldo {
        l.set(true);
    }
    std::thread::sleep(Duration::from_millis(100));

    let mut vr = Tps53647::new(bus.clone(), regulator);
    let probe = format!("device_code={:?}", vr.device_code());
    log::info!("board: TPS5364x probe {probe}");
    update(|s| s.vr_probe = probe);
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

    // Sensors and fans: full speed until the first reading.
    let mut sensor_bus = bus.clone();
    let sensors = tmp1075::detect(&mut sensor_bus);
    log::info!("board: TMP1075 at {:02x?}", sensors.as_slice());
    let mut fans = Emc2302::new(bus.clone());
    if let Ok((product, mfr)) = fans.identify() {
        log::info!("board: EMC2302 product {product:#04x} manufacturer {mfr:#04x}");
    }
    let fans_ok = fans.init(cfg.invertfanpolarity).is_ok()
        && fans.set_fan_percent(Fan::One, 100).is_ok()
        && fans.set_fan_percent(Fan::Two, 100).is_ok();
    if !fans_ok {
        log::error!("board: EMC2302 init failed");
        update(|s| s.last_error = "emc2302 init failed".into());
    }
    let thermal = Thermal::Nerd {
        fans,
        sensors,
        bus: bus.clone(),
        offset: profile.temp_offset_c,
    };
    let regulator = Regulator::Tps5364x {
        vr,
        enable: en,
        bus,
    };
    spawn_run(profile, Some(regulator), Some(thermal));
}

fn spawn_run(profile: &'static Profile, vr: Option<Regulator>, thermal: Option<Thermal>) {
    if vr.is_none() && thermal.is_none() {
        return;
    }
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
    pid_vr.reset(100.0, VR_FAN_TARGET_C);
    let mut applied_mv = cfg.core_voltage.max(profile.init_core_mv);
    let mut fans: (u8, u8) = (100, 100);
    let mut last_sample = Instant::now() - Duration::from_secs(1);
    let mut temp: Option<f32> = None;
    let mut vr_temp: Option<i32> = None;
    let mut polarity = cfg.invertfanpolarity;
    loop {
        // Once a second: sensors, config, the sample.
        if last_sample.elapsed() >= Duration::from_secs(1) {
            last_sample = Instant::now();
            cfg = config::get();
            pid.set_min_percent(f32::from(cfg.min_fan_speed));
            pid_vr.set_min_percent(f32::from(cfg.min_fan_speed));
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
            let mut status = None;
            vr_temp = None;
            if let Some(vr) = vr.as_mut() {
                vr_temp = vr.temperature_c();
                vin = vr.vin();
                vout = vr.vout();
                current = vr.current_a();
                power = vr.power_w(vout, current, profile.power_offset_w);
                status = vr.status();
            }
            let mut rpm = (0, None);
            if let Some(t) = thermal.as_mut() {
                match t.chip_temp_c() {
                    Ok(c) => {
                        temp = Some(c);
                        update(|s| {
                            if s.last_error.starts_with("temp:") {
                                s.last_error.clear();
                            }
                        });
                    }
                    Err(e) => {
                        temp = None;
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
            if (hot_chip || hot_vr) && !overheat() {
                OVERHEAT.store(true, Ordering::Relaxed);
                log::error!(
                    "board: OVERHEAT chip {temp:?} °C (limit {chip_limit}), regulator {vr_temp:?} °C (limit {}): core voltage off",
                    profile.vr_max_c
                );
                if let Some(vr) = vr.as_mut() {
                    vr.off();
                }
                if let Some(t) = thermal.as_mut() {
                    t.set_fans(100, 100);
                }
            }
            update(|s| {
                s.temp_c = temp;
                s.vr_temp_c = vr_temp;
                s.vin_mv = vin.map(|v| v * 1000.0);
                s.vout_mv = vout.map(|v| v * 1000.0);
                s.iout_ma = current.map(|a| a * 1000.0);
                s.power_w = power;
                s.fan_rpm = rpm.0;
                s.fan2_rpm = rpm.1;
                s.fan_auto = cfg.autofanspeed;
                s.overheat = overheat();
                if let Some(st) = status {
                    s.vr_status = st;
                }
            });
        }

        // A new core voltage from the API or the chain coming up.
        let want = WANTED_MV.load(Ordering::Relaxed);
        if want != 0 && want != applied_mv && !overheat() {
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
            let want = if overheat() {
                (100, 100)
            } else if cfg.autofanspeed {
                let chip = match temp {
                    Some(c) => pid.compute(c, target_c) as u8,
                    None => 100, // no reading: full fan
                };
                let vrf = match vr_temp {
                    Some(c) => pid_vr.compute(c as f32, VR_FAN_TARGET_C) as u8,
                    None => 100,
                };
                (chip, vrf)
            } else {
                let m = cfg.fanspeed.min(100);
                (m, m)
            };
            // One fan output: the second value just mirrors the first.
            let two = matches!(t, Thermal::Nerd { .. });
            let want = if two { want } else { (want.0, want.0) };
            if want != fans && t.set_fans(want.0, want.1) {
                fans = want;
                update(|s| {
                    s.fan_pct = want.0;
                    s.fan2_pct = two.then_some(want.1);
                });
            }
        }
        std::thread::sleep(Duration::from_millis(u64::from(SAMPLE_MS)));
    }
}

//! The board layer on ESP-IDF: the profile from the config, the I²C
//! bus, regulator and thermal sensor from `warpaxe-board`, the fan
//! regulator, and the sample the API reports. On a board without
//! regulator and sensor (the devkit) this only owns the reset pin, if
//! any, and reports nothing.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::gpio::{Gpio1, Gpio47, Gpio48, Output, PinDriver};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver, I2C0};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::EspError;
use serde::Serialize;
use warpaxe_board::emc2101::Emc2101;
use warpaxe_board::fan::{FanPid, DEFAULT_MIN_PERCENT, DEFAULT_TARGET_C, SAMPLE_MS};
use warpaxe_board::tps546::Tps546;
use warpaxe_board::{Asic, I2c, Profile};
use warpaxe_core::chip::{bm1370::BM1370, bm1373::BM1373, ChipProfile};

use crate::config;

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
    pub iout_ma: Option<f32>,
    pub power_w: Option<f32>,
    pub fan_pct: u8,
    pub fan_rpm: u16,
    pub fan_auto: bool,
    pub vr_status: u16,
    /// Mining stopped and the regulator switched off; stays until reboot.
    pub overheat: bool,
    pub last_error: String,
}

static STATE: Mutex<Option<BoardState>> = Mutex::new(None);
static OVERHEAT: AtomicBool = AtomicBool::new(false);
/// Core voltage the API asked for, mV; 0 = nothing pending.
static WANTED_MV: AtomicU16 = AtomicU16::new(0);
static RESET: Mutex<Option<PinDriver<'static, Gpio1, Output>>> = Mutex::new(None);
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

/// Pull the chain's reset low, then release it, with the profile's
/// timing. Nothing happens on a board without a reset line.
pub fn reset_chain(low_ms: u32, release_ms: u32) {
    if let Some(pin) = RESET.lock().unwrap().as_mut() {
        let _ = pin.set_low();
        std::thread::sleep(Duration::from_millis(u64::from(low_ms)));
        let _ = pin.set_high();
        std::thread::sleep(Duration::from_millis(u64::from(release_ms)));
    }
}

/// The bus shared by regulator, sensor and display.
#[derive(Clone)]
pub struct SharedI2c(Arc<Mutex<I2cDriver<'static>>>);

static BUS: Mutex<Option<SharedI2c>> = Mutex::new(None);

/// The board's I²C bus, once [`init_bus`] created it.
pub fn i2c() -> Option<SharedI2c> {
    BUS.lock().unwrap().clone()
}

impl I2c for SharedI2c {
    type Error = EspError;
    fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), EspError> {
        self.0.lock().unwrap().write(addr, bytes, BLOCK)
    }
    fn write_read(&mut self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), EspError> {
        self.0.lock().unwrap().write_read(addr, wr, rd, BLOCK)
    }
}

/// Pick the profile from the config and create its I²C bus, before
/// anything (the display first) needs it.
pub fn init_bus(i2c0: I2C0, sda: Gpio47, scl: Gpio48) {
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
        s.has_vr = profile.tps546().is_some();
        s.has_thermal = profile.emc2101().is_some();
        s.fan_auto = cfg.autofanspeed;
    });
    log::info!("board: profile {}", profile.name);
    let Some(pins) = profile.i2c else {
        return;
    };
    match I2cDriver::new(i2c0, sda, scl, &I2cConfig::new().baudrate(Hertz(pins.hz))) {
        Ok(b) => *BUS.lock().unwrap() = Some(SharedI2c(Arc::new(Mutex::new(b)))),
        Err(e) => {
            log::error!("board: i2c failed: {e}");
            update(|s| s.last_error = format!("i2c: {e}"));
        }
    }
}

/// Bring the board up: reset line, regulator at the configured core
/// voltage, sensor, then the sampling and fan task. Returns once the
/// chain may be started.
pub fn start(reset: Gpio1) {
    let cfg = config::get();
    let profile = profile();
    if profile.asic_reset.is_some() {
        match PinDriver::output(reset) {
            Ok(mut p) => {
                let _ = p.set_low(); // held in reset until the regulator is up
                *RESET.lock().unwrap() = Some(p);
            }
            Err(e) => log::error!("board: reset pin failed: {e}"),
        }
    }
    if profile.vr_unsupported() {
        log::error!(
            "board: {} names a regulator this firmware cannot drive yet; the chain stays unpowered",
            profile.name
        );
        update(|s| s.last_error = "regulator not supported yet".into());
        return;
    }
    let Some(bus) = i2c() else {
        return;
    };

    let mut vr = profile.tps546().map(|c| Tps546::new(bus.clone(), c));
    if let Some(vr) = vr.as_mut() {
        match vr.init() {
            Ok(status) => {
                log::info!("board: TPS546 up, status {status:#06x}");
                update(|s| s.vr_status = status);
                let mv = if profile.init_core_mv > cfg.core_voltage {
                    profile.init_core_mv
                } else {
                    cfg.core_voltage
                };
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
    let vr = vr.filter(|_| state().has_vr);

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
    let thermal = thermal.filter(|_| state().has_thermal);

    // Regulator up: release the chain from reset with the init timing.
    std::thread::sleep(Duration::from_millis(u64::from(profile.reset_low_ms)));
    reset_chain(profile.reset_low_ms, profile.reset_release_ms);

    if vr.is_none() && thermal.is_none() {
        return;
    }
    std::thread::Builder::new()
        .name("board".into())
        .stack_size(6144)
        .spawn(move || run(profile, vr, thermal))
        .expect("board thread");
}

fn run(
    profile: &'static Profile,
    mut vr: Option<Tps546<SharedI2c>>,
    mut thermal: Option<Emc2101<SharedI2c>>,
) {
    let mut pid = FanPid::new(DEFAULT_MIN_PERCENT);
    pid.reset(100.0, DEFAULT_TARGET_C);
    let mut applied_mv = config::get().core_voltage;
    let mut fan_pct: u8 = 100;
    let mut last_sample = Instant::now() - Duration::from_secs(1);
    let mut temp: Option<f32> = None;
    let mut cfg = config::get();
    loop {
        // Once a second: sensors, config, the sample.
        if last_sample.elapsed() >= Duration::from_secs(1) {
            last_sample = Instant::now();
            cfg = config::get();
            let mut vr_temp = None;
            let mut vin = None;
            let mut vout = None;
            let mut iout = None;
            let mut status = None;
            if let Some(vr) = vr.as_mut() {
                vr_temp = vr.temperature_c().ok();
                vin = vr.vin().ok();
                vout = vr.vout().ok();
                iout = vr.iout().ok();
                status = vr.status_word().ok();
            }
            let mut rpm = 0;
            if let Some(t) = thermal.as_mut() {
                match t.external_temp_c() {
                    Ok(c) => temp = Some(c),
                    Err(e) => {
                        temp = None;
                        update(|s| s.last_error = format!("temp: {e:?}"));
                    }
                }
                rpm = t.fan_rpm().unwrap_or(0);
            }
            let power = match (vout, iout) {
                (Some(v), Some(i)) => Some(v * i + profile.power_offset_w),
                _ => None,
            };
            let hot_chip = temp.map_or(false, |c| c > profile.max_temp_c);
            let hot_vr = vr_temp.map_or(false, |c| c as f32 > profile.vr_max_c);
            if (hot_chip || hot_vr) && !overheat() {
                OVERHEAT.store(true, Ordering::Relaxed);
                log::error!(
                    "board: OVERHEAT chip {temp:?} °C, regulator {vr_temp:?} °C: core voltage off"
                );
                if let Some(vr) = vr.as_mut() {
                    let _ = vr.set_vout(0.0);
                }
                if let Some(t) = thermal.as_mut() {
                    let _ = t.set_fan_percent(100);
                }
            }
            update(|s| {
                s.temp_c = temp;
                s.vr_temp_c = vr_temp;
                s.vin_mv = vin.map(|v| v * 1000.0);
                s.vout_mv = vout.map(|v| v * 1000.0);
                s.iout_ma = iout.map(|a| a * 1000.0);
                s.power_w = power;
                s.fan_rpm = rpm;
                s.fan_auto = cfg.autofanspeed;
                s.overheat = overheat();
                if let Some(st) = status {
                    s.vr_status = st;
                }
            });
        }

        // A new core voltage from the API.
        let want = WANTED_MV.load(Ordering::Relaxed);
        if want != 0 && want != applied_mv && !overheat() {
            if let Some(vr) = vr.as_mut() {
                let volts = profile.vout_for_core_mv(want);
                match vr.set_vout(volts) {
                    Ok(()) => log::info!("board: core {want} mV"),
                    Err(e) => log::warn!("board: core {want} mV refused: {e:?}"),
                }
            }
            applied_mv = want;
        }

        // Fan, every sample period.
        if let Some(t) = thermal.as_mut() {
            let target = if overheat() {
                100
            } else if cfg.autofanspeed {
                match temp {
                    Some(c) => pid.compute(c, DEFAULT_TARGET_C) as u8,
                    None => 100, // no reading: full fan
                }
            } else {
                cfg.fanspeed.min(100)
            };
            if target != fan_pct {
                if t.set_fan_percent(target).is_ok() {
                    fan_pct = target;
                    update(|s| s.fan_pct = target);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(u64::from(SAMPLE_MS)));
    }
}

//! What a board has and where: pins, regulator, thermal sensor,
//! display, limits. Values for the Bitaxe are ESP-Miner's board table
//! (family Gamma, versions 600–603); the Nerd boards are NerdOS's board
//! classes (`main/boards/*.cpp`, branch `develop`, read 2026-09-24); the
//! devkit profile is the bare T-Display-S3 with nothing but the chain
//! UART.
//!
//! A profile names what is there even when the firmware cannot drive it
//! yet ([`Vr::Tps5364x`], whose driver [`crate::tps53647`] is not wired
//! into the firmware): the firmware then reports the board and refuses
//! to power the chain, instead of guessing.

use crate::{emc2101, tps53647, tps546};

/// The mining chip on the board; the firmware picks the
/// `warpaxe_core` chip profile from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asic {
    /// BM1370 (Bitaxe Gamma, NerdAxe Gamma, NerdQAxe++, Octaxe, Q1370).
    Bm1370,
    /// BM1373 (Q1373 / Gaia).
    Bm1373,
}

/// The core-voltage regulator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Vr {
    /// None known (the devkit).
    None,
    /// TPS546D24A over PMBus, driver in [`crate::tps546`].
    Tps546(tps546::Config),
    /// TPS53647 (up to 4 phases) or TPS53667 (6 phases) over PMBus,
    /// the Nerd multi-chip boards; driver in [`crate::tps53647`].
    /// `enable`/`ldo_enable` are ESP GPIOs, or expander pins when
    /// `via_expander`. Boards that carry either part (Octaxe, Q1373)
    /// name the other one in `alt` with the pin that tells.
    Tps5364x {
        /// Part, phases and current limits.
        regulator: tps53647::Config,
        /// Regulator enable pin.
        enable: u8,
        /// LDO enable pin (chip I/O rails), if separate.
        ldo_enable: Option<u8>,
        /// Pins are on an FXL6408 I²C expander, not the ESP.
        via_expander: bool,
        /// The other regulator population and the pin that selects it.
        alt: Option<VrAlt>,
    },
}

/// A board populated with either of two regulators: `regulator` applies
/// when `pin` reads `level` (ESP GPIO read with pull-down, or an
/// expander input when the regulator's pins are on the expander).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VrAlt {
    /// The detect pin.
    pub pin: u8,
    /// The level that selects `regulator`.
    pub level: bool,
    /// Part, phases and current limits for that population.
    pub regulator: tps53647::Config,
}

/// The chip temperature source and fan output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thermal {
    /// None known (the devkit).
    None,
    /// EMC2101 on the chip diode with its own fan output, driver in
    /// [`crate::emc2101`].
    Emc2101(emc2101::Config),
    /// TMP1075 sensors (one near the regulator at address offset 1,
    /// more near the chips, detected at init) and an EMC2302 dual fan
    /// controller: the Nerd multi-chip boards. TMP1075 driver:
    /// [`crate::tmp1075`], EMC2302 driver: [`crate::emc2302`].
    Tmp1075Emc2302,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The display a board carries.
pub enum Display {
    /// No display.
    None,
    /// LilyGO T-Display-S3: ST7789 170×320 on the 8-bit i80 bus.
    St7789I80,
    /// Bitaxe: SSD1306 128×32 on I²C.
    Ssd1306 {
        /// I²C address.
        addr: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// An I²C bus: pins and clock.
pub struct I2cPins {
    /// SDA GPIO.
    pub sda: u8,
    /// SCL GPIO.
    pub scl: u8,
    /// Bus clock, Hz.
    pub hz: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// A board: what it has and where.
pub struct Profile {
    /// AxeOS `boardVersion`-style name.
    pub name: &'static str,
    /// The board's name as its maker writes it, for people (NerdOS
    /// `deviceModel` spelling); `name` is the id tools match on.
    pub model: &'static str,
    /// The chip.
    pub asic: Asic,
    /// Chips on the chain; 0 = discover.
    pub asic_count: u8,
    /// Chain UART, ESP side.
    pub uart_tx: u8,
    /// UART RX GPIO (chain TX).
    pub uart_rx: u8,
    /// Active-low reset to the chain; held low then released at init.
    pub asic_reset: Option<u8>,
    /// Reset held low this long at init, ms.
    pub reset_low_ms: u32,
    /// Wait after releasing reset, ms.
    pub reset_release_ms: u32,
    /// I²C bus, if the board has one.
    pub i2c: Option<I2cPins>,
    /// Core-voltage regulator.
    pub vr: Vr,
    /// Chain voltage domains in series behind the regulator: VOUT =
    /// core voltage × domains.
    pub voltage_domains: u8,
    /// Thermal sensor and fan controller.
    pub thermal: Thermal,
    /// The display.
    pub display: Display,
    /// Factory clock, MHz.
    pub default_frequency_mhz: u32,
    /// Factory core voltage, mV.
    pub default_core_mv: u16,
    /// Core voltage to bring the chain up with before the configured
    /// point applies (NerdOS `m_initVoltageMillis`); 0 = the configured one.
    pub init_core_mv: u16,
    /// Board input class, V (5 for Bitaxe, 12 for the 12 V boards).
    pub nominal_voltage_v: u8,
    /// Board power limit, W.
    pub max_power_w: u16,
    /// Added to VOUT×IOUT: what the rest of the board draws.
    pub power_offset_w: f32,
    /// Chip temperature above which the firmware throttles / above
    /// which it stops (ESP-Miner THROTTLE_TEMP / MAX_TEMP).
    pub throttle_temp_c: f32,
    /// Chip temperature at which mining stops, °C.
    pub max_temp_c: f32,
    /// Regulator temperature limits (TPS546_THROTTLE_TEMP / MAX_TEMP).
    pub vr_throttle_c: f32,
    /// Regulator temperature at which mining stops, °C.
    pub vr_max_c: f32,
    /// Added to the board temperature sensors: NerdOS adds 10 °C on the
    /// NerdQAxe++ (`NerdQaxePlus2::getTemperature`) and 3 °C on the
    /// Q13xx carriers, whose sensors sit beside the chips. The fan
    /// target and the overheat limit apply to the corrected value.
    pub temp_offset_c: f32,
    /// Chip temperature the fan PID holds (NerdOS `m_pidSettings[0]`).
    pub fan_target_c: f32,
    /// The stock firmware's clock choices, MHz (NerdOS
    /// `/api/system/asic` `frequencyOptions`); empty = not published.
    pub frequency_options: &'static [u32],
    /// The stock firmware's core-voltage choices, mV; empty = none.
    pub voltage_options: &'static [u16],
    /// Highest clock the firmware accepts, MHz; 0 = no limit.
    pub abs_max_frequency_mhz: u32,
    /// Lowest core voltage the firmware accepts, mV; 0 = no limit.
    pub abs_min_core_mv: u16,
    /// Highest core voltage the firmware accepts, mV; 0 = no limit.
    pub abs_max_core_mv: u16,
}

impl Profile {
    /// The profile for a board name from the config; `None` for a
    /// name this firmware does not know.
    pub fn by_name(name: &str) -> Option<&'static Profile> {
        ALL.iter().find(|p| p.name == name)
    }

    /// The regulator this crate can drive, if the board has one.
    pub fn tps546(&self) -> Option<tps546::Config> {
        match self.vr {
            Vr::Tps546(c) => Some(c),
            _ => None,
        }
    }

    /// The multi-phase regulator configuration, if the board has one.
    pub fn tps53647(&self) -> Option<tps53647::Config> {
        match self.vr {
            Vr::Tps5364x { regulator, .. } => Some(regulator),
            _ => None,
        }
    }

    /// The sensor this crate can drive, if the board has one.
    pub fn emc2101(&self) -> Option<emc2101::Config> {
        match self.thermal {
            Thermal::Emc2101(c) => Some(c),
            _ => None,
        }
    }

    /// VOUT to command for a core voltage in mV.
    pub fn vout_for_core_mv(&self, core_mv: u16) -> f32 {
        f32::from(core_mv) / 1000.0 * f32::from(self.voltage_domains.max(1))
    }

    /// Core voltage in mV for a measured VOUT.
    pub fn core_mv_for_vout(&self, vout: f32) -> u16 {
        (vout / f32::from(self.voltage_domains.max(1)) * 1000.0) as u16
    }
}

/// Bitaxe Gamma 600–603: one BM1370, TPS546 at 5 V in, EMC2101 on the
/// chip diode, SSD1306 on the same I²C, reset on GPIO1, UART 17/18.
pub const BITAXE_GAMMA: Profile = Profile {
    name: "bitaxe-gamma",
    model: "Bitaxe Gamma",
    asic: Asic::Bm1370,
    asic_count: 1,
    uart_tx: 17,
    uart_rx: 18,
    asic_reset: Some(1),
    reset_low_ms: 100,
    reset_release_ms: 100,
    i2c: Some(I2cPins {
        sda: 47,
        scl: 48,
        hz: 100_000,
    }),
    vr: Vr::Tps546(tps546::Config::BITAXE_5V),
    voltage_domains: 1,
    thermal: Thermal::Emc2101(emc2101::Config::BM1370_DIODE),
    display: Display::Ssd1306 { addr: 0x3C },
    default_frequency_mhz: 525,
    default_core_mv: 1150,
    init_core_mv: 0,
    nominal_voltage_v: 5,
    max_power_w: 40,
    power_offset_w: 5.0,
    throttle_temp_c: 75.0,
    max_temp_c: 90.0,
    vr_throttle_c: 105.0,
    vr_max_c: 145.0,
    temp_offset_c: 0.0,
    fan_target_c: 60.0,
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
};

/// The NerdMiner V2 / bare LilyGO T-Display-S3 as devkit: chain UART on
/// 17/18 (to the simulator), no regulator, no sensor. GPIO47/48 are
/// display data lines here, so no I²C.
pub const T_DISPLAY_S3_DEVKIT: Profile = Profile {
    name: "t-display-s3",
    model: "T-Display-S3",
    asic: Asic::Bm1370,
    asic_count: 0,
    uart_tx: 17,
    uart_rx: 18,
    asic_reset: None,
    reset_low_ms: 0,
    reset_release_ms: 0,
    i2c: None,
    vr: Vr::None,
    voltage_domains: 1,
    thermal: Thermal::None,
    display: Display::St7789I80,
    default_frequency_mhz: 525,
    default_core_mv: 1150,
    init_core_mv: 0,
    nominal_voltage_v: 5,
    max_power_w: 40,
    power_offset_w: 0.0,
    throttle_temp_c: 75.0,
    max_temp_c: 90.0,
    vr_throttle_c: 105.0,
    vr_max_c: 145.0,
    temp_offset_c: 0.0,
    fan_target_c: 60.0,
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
};

/// Every Nerd board: a LilyGO T-Display-S3 on a carrier. Chain UART
/// 17/18, I²C SDA 44 / SCL 43, chain reset GPIO1, the ST7789 on the
/// module (NerdOS `serial.cpp`, `i2c_master.cpp`, board classes).
const NERD_I2C: I2cPins = I2cPins {
    sda: 44,
    scl: 43,
    hz: 100_000,
};

/// NerdAxe Gamma: one BM1370 on a 5 V carrier, TPS546 with NerdOS's
/// window (VIN 4.8/4.5 on/off, UV warn 5.8, OV fault 6.0, VOUT 1–3 V),
/// EMC2101 on the chip diode, 515 MHz / 1150 mV, 25 W, +5 W offset.
pub const NERDAXE_GAMMA: Profile = Profile {
    name: "nerdaxe-gamma",
    model: "NerdAxe Gamma",
    asic: Asic::Bm1370,
    asic_count: 1,
    uart_tx: 17,
    uart_rx: 18,
    asic_reset: Some(1),
    reset_low_ms: 100,
    reset_release_ms: 100,
    i2c: Some(NERD_I2C),
    vr: Vr::Tps546(tps546::Config {
        vin_uv_warn: 5.8,
        vin_ov_fault: 6.0,
        vout_max: 3.0,
        ..tps546::Config::BITAXE_5V
    }),
    voltage_domains: 1,
    thermal: Thermal::Emc2101(emc2101::Config::BM1370_DIODE),
    display: Display::St7789I80,
    default_frequency_mhz: 515,
    default_core_mv: 1150,
    init_core_mv: 0,
    nominal_voltage_v: 5,
    max_power_w: 25,
    power_offset_w: 5.0,
    throttle_temp_c: 75.0,
    max_temp_c: 90.0,
    vr_throttle_c: 105.0,
    vr_max_c: 145.0,
    temp_offset_c: 0.0,
    fan_target_c: 60.0,
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
};

/// NerdQAxe++: four BM1370 on 12 V, TPS53647 three-phase (enable
/// GPIO10, LDO GPIO13), TMP1075 + EMC2302, 600 MHz / 1150 mV, brought
/// up at 1200 mV, 100 W.
pub const NERDQAXE_PLUSPLUS: Profile = Profile {
    name: "nerdqaxe-plusplus",
    model: "NerdQAxe++",
    asic: Asic::Bm1370,
    asic_count: 4,
    uart_tx: 17,
    uart_rx: 18,
    asic_reset: Some(1),
    reset_low_ms: 100,
    reset_release_ms: 100,
    i2c: Some(NERD_I2C),
    vr: Vr::Tps5364x {
        regulator: tps53647::Config::NERDQAXE_PLUSPLUS,
        enable: 10,
        ldo_enable: Some(13),
        via_expander: false,
        alt: None,
    },
    voltage_domains: 1,
    thermal: Thermal::Tmp1075Emc2302,
    display: Display::St7789I80,
    default_frequency_mhz: 600,
    default_core_mv: 1150,
    init_core_mv: 1200,
    nominal_voltage_v: 12,
    max_power_w: 100,
    power_offset_w: 0.0,
    // NerdOS: CONFIG_OVERHEAT_TEMP 70 °C for the chips and, on the
    // second fan channel, for the regulator; no throttle step.
    throttle_temp_c: 70.0,
    max_temp_c: 70.0,
    vr_throttle_c: 70.0,
    vr_max_c: 70.0,
    // NerdOS `NerdQaxePlus2::getTemperature`: TMP1075 + 10 °C ("should
    // be about right" for the chips). Without it the 70 °C limit would
    // let the sensors reach what NerdOS calls 80 °C.
    temp_offset_c: 10.0,
    fan_target_c: 55.0,
    // NerdOS 1.1.0 on a Rev 6.1 board, `/api/system/asic` (2026-09-27).
    frequency_options: &[500, 515, 525, 550, 575, 590, 600],
    voltage_options: &[1120, 1130, 1140, 1150, 1160, 1170, 1180, 1190, 1200],
    abs_max_frequency_mhz: 800,
    abs_min_core_mv: 1050,
    abs_max_core_mv: 1400,
};

/// NerdOctaxe-γ: eight BM1370 on 12 V, TPS53667 six-phase (or TPS53647
/// four-phase when VR_DETECT GPIO3 reads low:
/// [`tps53647::Config::NERDOCTAXE_GAMMA_4P`]), 700 MHz / 1210 mV, 300 W.
pub const NERDOCTAXE_GAMMA: Profile = Profile {
    name: "nerdoctaxe-gamma",
    model: "NerdOCTAXE-γ",
    asic_count: 8,
    vr: Vr::Tps5364x {
        regulator: tps53647::Config::NERDOCTAXE_GAMMA_6P,
        enable: 10,
        ldo_enable: Some(13),
        via_expander: false,
        // VR_DETECT GPIO3 with pull-down: low (or unfitted) = TPS53647.
        alt: Some(VrAlt {
            pin: 3,
            level: false,
            regulator: tps53647::Config::NERDOCTAXE_GAMMA_4P,
        }),
    },
    default_frequency_mhz: 700,
    default_core_mv: 1210,
    init_core_mv: 0,
    max_power_w: 300,
    // NerdOS reads the Octaxe sensors as they are (no `getTemperature`
    // override); not inherited from the NerdQAxe++.
    temp_offset_c: 0.0,
    // Options depend on the fitted regulator; published once this
    // board is brought up.
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    ..NERDQAXE_PLUSPLUS
};

/// Q1370: four BM1370 on 12 V, TPS53647 four-phase behind an FXL6408
/// expander (reset 0, regulator 1, LDO 2), EMC2302, 600 MHz / 1150 mV,
/// 150 W. Expander driver: [`crate::fxl6408`].
pub const Q1370: Profile = Profile {
    name: "q1370",
    model: "Q1370",
    asic_count: 4,
    asic_reset: None,
    vr: Vr::Tps5364x {
        regulator: tps53647::Config::Q1370,
        enable: 1,
        ldo_enable: Some(2),
        via_expander: true,
        alt: None,
    },
    default_frequency_mhz: 600,
    default_core_mv: 1150,
    init_core_mv: 1200,
    max_power_w: 150,
    temp_offset_c: 3.0,
    fan_target_c: 55.0,
    // Options depend on the fitted regulator; published once this
    // board is brought up.
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    ..NERDQAXE_PLUSPLUS
};

/// Q1373 (NerdAxe Gaia): four BM1373 on 12 V, TPS53647 four-phase or
/// TPS53667 six-phase (expander pin 6 low:
/// [`tps53647::Config::Q1373_6P`]), 425 MHz / 1010 mV, brought up at
/// 1050 mV, 180 W. Expander driver: [`crate::fxl6408`].
pub const Q1373: Profile = Profile {
    name: "q1373",
    model: "Q1373",
    asic: Asic::Bm1373,
    asic_count: 4,
    asic_reset: None,
    vr: Vr::Tps5364x {
        regulator: tps53647::Config::Q1373_4P,
        enable: 1,
        ldo_enable: Some(2),
        via_expander: true,
        // Expander pin 6: low = six-phase TPS53667.
        alt: Some(VrAlt {
            pin: crate::fxl6408::q13xx::PHASE_DETECT,
            level: false,
            regulator: tps53647::Config::Q1373_6P,
        }),
    },
    default_frequency_mhz: 425,
    default_core_mv: 1010,
    init_core_mv: 1050,
    max_power_w: 180,
    temp_offset_c: 3.0,
    fan_target_c: 60.0,
    // Options depend on the fitted regulator; published once this
    // board is brought up.
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    ..NERDQAXE_PLUSPLUS
};

/// Every profile this firmware knows.
pub const ALL: &[Profile] = &[
    BITAXE_GAMMA,
    T_DISPLAY_S3_DEVKIT,
    NERDAXE_GAMMA,
    NERDQAXE_PLUSPLUS,
    NERDOCTAXE_GAMMA,
    Q1370,
    Q1373,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_resolve_by_name_and_names_are_unique() {
        assert_eq!(Profile::by_name("bitaxe-gamma"), Some(&BITAXE_GAMMA));
        assert_eq!(Profile::by_name("t-display-s3"), Some(&T_DISPLAY_S3_DEVKIT));
        assert_eq!(Profile::by_name("nerdaxe-gamma"), Some(&NERDAXE_GAMMA));
        assert_eq!(Profile::by_name("nerdaxe"), None);
        for (i, a) in ALL.iter().enumerate() {
            assert!(ALL[i + 1..].iter().all(|b| b.name != a.name));
        }
    }

    #[test]
    fn core_voltage_scales_with_the_domains() {
        assert_eq!(BITAXE_GAMMA.vout_for_core_mv(1150), 1.15);
        assert_eq!(BITAXE_GAMMA.core_mv_for_vout(1.15), 1150);
        let two = Profile {
            voltage_domains: 2,
            ..BITAXE_GAMMA
        };
        assert_eq!(two.vout_for_core_mv(1150), 2.3);
        assert_eq!(two.core_mv_for_vout(2.3), 1150);
    }

    #[test]
    fn tps546_defaults_lie_inside_the_regulator_window() {
        for p in [&BITAXE_GAMMA, &NERDAXE_GAMMA] {
            let vr = p.tps546().unwrap();
            let v = p.vout_for_core_mv(p.default_core_mv);
            assert!(v >= vr.vout_min && v <= vr.vout_max, "{}", p.name);
        }
    }

    #[test]
    fn nerd_boards_carry_a_tps5364x_and_the_board_details() {
        for p in [&NERDQAXE_PLUSPLUS, &NERDOCTAXE_GAMMA, &Q1370, &Q1373] {
            assert!(p.tps546().is_none());
            let vr = p.tps53647().unwrap();
            assert!((1..=6).contains(&vr.phases), "{}", p.name);
            assert!(
                p.default_core_mv >= tps53647::VOUT_MIN_MV
                    && p.default_core_mv <= tps53647::VOUT_MAX_MV,
                "{}",
                p.name
            );
            assert_eq!(p.thermal, Thermal::Tmp1075Emc2302);
            assert_eq!(p.i2c, Some(NERD_I2C));
            assert_eq!(p.nominal_voltage_v, 12);
        }
        assert_eq!(Q1370.temp_offset_c, 3.0);
        assert_eq!(Q1370.fan_target_c, 55.0);
        assert_eq!(NERDQAXE_PLUSPLUS.fan_target_c, 55.0);
        assert_eq!(NERDQAXE_PLUSPLUS.abs_max_frequency_mhz, 800);
        assert_eq!(NERDQAXE_PLUSPLUS.model, "NerdQAxe++");
        assert!(
            NERDOCTAXE_GAMMA.frequency_options.is_empty(),
            "not inherited"
        );
        assert!(
            BITAXE_GAMMA.frequency_options.is_empty(),
            "Gamma keeps its ladder"
        );
        assert_eq!(NERDOCTAXE_GAMMA.fan_target_c, 55.0);
        for p in [&NERDQAXE_PLUSPLUS, &NERDOCTAXE_GAMMA, &Q1370, &Q1373] {
            assert_eq!((p.max_temp_c, p.vr_max_c), (70.0, 70.0), "{}", p.name);
        }
        assert_eq!(NERDOCTAXE_GAMMA.tps53647().unwrap().vr_temp_offset_c, 8);
        assert_eq!(Q1373.temp_offset_c, 3.0);
        assert_eq!(NERDQAXE_PLUSPLUS.temp_offset_c, 10.0, "NerdOS +10 °C");
        assert_eq!(NERDOCTAXE_GAMMA.temp_offset_c, 0.0, "not inherited");
        assert_eq!(BITAXE_GAMMA.temp_offset_c, 0.0);
        let alt = |p: &Profile| match p.vr {
            Vr::Tps5364x { alt, .. } => alt,
            _ => None,
        };
        assert_eq!(alt(&NERDOCTAXE_GAMMA).unwrap().regulator.phases, 4);
        assert_eq!(alt(&Q1373).unwrap().regulator.phases, 6);
        assert_eq!(alt(&NERDQAXE_PLUSPLUS), None);
        assert_eq!(
            alt(&NERDOCTAXE_GAMMA).unwrap().regulator.vr_temp_offset_c,
            0
        );
        assert!(NERDAXE_GAMMA.tps53647().is_none());
        assert_eq!(NERDOCTAXE_GAMMA.tps53647().unwrap().phases, 6);
        assert_eq!(Q1373.tps53647().unwrap().phases, 4);
        assert!(NERDAXE_GAMMA.emc2101().is_some());
        assert_eq!(Q1373.asic_reset, None, "reset sits on the expander");
        assert_eq!(Q1373.asic, Asic::Bm1373);
        assert!(ALL
            .iter()
            .filter(|p| p.name != "q1373")
            .all(|p| p.asic == Asic::Bm1370));
    }
}

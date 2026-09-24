//! What a board has and where: pins, regulator, thermal sensor,
//! display, limits. Values for the Bitaxe are ESP-Miner's board table
//! (family Gamma, versions 600–603); the devkit profile is the bare
//! T-Display-S3 with nothing but the chain UART.

use crate::{emc2101, tps546};

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
    /// Core-voltage regulator, if the board has one.
    pub vr: Option<tps546::Config>,
    /// Chain voltage domains in series behind the regulator: VOUT =
    /// core voltage × domains.
    pub voltage_domains: u8,
    /// Thermal sensor and fan controller, if the board has one.
    pub thermal: Option<emc2101::Config>,
    /// The display.
    pub display: Display,
    /// Factory clock, MHz.
    pub default_frequency_mhz: u32,
    /// Factory core voltage, mV.
    pub default_core_mv: u16,
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
}

impl Profile {
    /// The profile for a board name from the config; `None` for a
    /// name this firmware does not know.
    pub fn by_name(name: &str) -> Option<&'static Profile> {
        ALL.iter().find(|p| p.name == name)
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
    vr: Some(tps546::Config::BITAXE_5V),
    voltage_domains: 1,
    thermal: Some(emc2101::Config::BM1370_DIODE),
    display: Display::Ssd1306 { addr: 0x3C },
    default_frequency_mhz: 525,
    default_core_mv: 1150,
    nominal_voltage_v: 5,
    max_power_w: 40,
    power_offset_w: 5.0,
    throttle_temp_c: 75.0,
    max_temp_c: 90.0,
    vr_throttle_c: 105.0,
    vr_max_c: 145.0,
};

/// The NerdMiner V2 / bare LilyGO T-Display-S3 as devkit: chain UART on
/// 17/18 (to the simulator), no regulator, no sensor. GPIO47/48 are
/// display data lines here, so no I²C.
pub const T_DISPLAY_S3_DEVKIT: Profile = Profile {
    name: "t-display-s3",
    asic_count: 0,
    uart_tx: 17,
    uart_rx: 18,
    asic_reset: None,
    reset_low_ms: 0,
    reset_release_ms: 0,
    i2c: None,
    vr: None,
    voltage_domains: 1,
    thermal: None,
    display: Display::St7789I80,
    default_frequency_mhz: 525,
    default_core_mv: 1150,
    nominal_voltage_v: 5,
    max_power_w: 40,
    power_offset_w: 0.0,
    throttle_temp_c: 75.0,
    max_temp_c: 90.0,
    vr_throttle_c: 105.0,
    vr_max_c: 145.0,
};

/// Every profile this firmware knows.
pub const ALL: &[Profile] = &[BITAXE_GAMMA, T_DISPLAY_S3_DEVKIT];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_resolve_by_name_and_names_are_unique() {
        assert_eq!(Profile::by_name("bitaxe-gamma"), Some(&BITAXE_GAMMA));
        assert_eq!(Profile::by_name("t-display-s3"), Some(&T_DISPLAY_S3_DEVKIT));
        assert_eq!(Profile::by_name("nerdaxe-gamma"), None);
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
    fn gamma_defaults_lie_inside_the_regulator_window() {
        let vr = BITAXE_GAMMA.vr.unwrap();
        let v = BITAXE_GAMMA.vout_for_core_mv(BITAXE_GAMMA.default_core_mv);
        assert!(v >= vr.vout_min && v <= vr.vout_max);
    }
}

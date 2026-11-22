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

use crate::{emc2101, emc2103, ina260, ntc, pca9544, tps53647, tps546};

/// The mining chip on the board; the firmware picks the
/// `warpaxe_core` chip profile from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asic {
    /// BM1370 (Bitaxe Gamma, NerdAxe Gamma, NerdQAxe++, Octaxe, Q1370).
    Bm1370,
    /// BM1373 (Q1373 / Gaia).
    Bm1373,
    /// BM1397 (NerdNOS, Bitaxe Max).
    Bm1397,
    /// BM1368 (NerdQAxe+; Bitaxe Supra and Supra Hex, which WarpAxe has
    /// no profile for).
    Bm1368,
}

/// The core-voltage regulator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Vr {
    /// None known (the devkit).
    None,
    /// TPS546D24A over PMBus, driver in [`crate::tps546`].
    Tps546(tps546::Config),
    /// TPS546D24A plus an INA260 on the input rail that measures the
    /// board's voltage, current and power (BitForge Nano). Driver in
    /// [`crate::ina260`]; the regulator path is the Bitaxe one.
    Tps546Ina260 {
        /// The regulator window.
        regulator: tps546::Config,
        /// INA260 I²C address.
        ina260: u8,
    },
    /// TPS546D24A on a Nerd carrier with an LDO line to bring up first:
    /// the NerdQAxe++ Rev 7 (NerdOS `drivers/rev7/TPS546`) and the
    /// NerdAxe Gaia. An enable line, where there is one, stays high once
    /// up; the output is switched by OPERATION like on the Bitaxe.
    Tps546Nerd {
        /// The regulator window.
        regulator: tps546::Config,
        /// Regulator enable GPIO, if the board has one (Rev 7: GPIO10;
        /// the Gaia has none).
        enable: Option<u8>,
        /// LDO enable GPIO (chip I/O rails).
        ldo_enable: Option<u8>,
    },
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
    /// The part is taken from the regulator's device code and the pin
    /// only gives the phase count ([`tps53647::resolve_by_code`]), as
    /// NerdOS does on the Q1373; false = the pin decides both (Octaxe).
    pub part_by_code: bool,
}

/// The chip temperature source and fan output.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// EMC2103 with two diode channels and one fan output: the Bitaxe
    /// Gamma Turbo, Naja Duo and Gamma Hex. Driver in [`crate::emc2103`].
    Emc2103(emc2103::Config),
    /// Two EMC2101, one per chip with its own fan, behind a PCA9544 mux
    /// (both at the EMC2101's fixed address): the BitForge Nano. Drivers:
    /// [`crate::emc2101`] through [`crate::pca9544::Muxed`].
    DualEmc2101Mux {
        /// Diode tuning for both.
        config: emc2101::Config,
        /// Mux address.
        mux: u8,
        /// Mux channel of the first and the second sensor.
        channels: [u8; 2],
    },
    /// A thermistor divider on an ADC pin, no fan (the NerdNOS). The
    /// arithmetic is [`crate::ntc`]; the firmware reads the ADC.
    Ntc(ntc::Config),
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
    /// Board power class, W, as the stock firmware reports it (AxeOS /
    /// NerdOS `maxPower`). Reported only; nothing throttles on it — the
    /// temperature limits and the regulator's own protection do that.
    pub max_power_w: u16,
    /// Added to VOUT×IOUT: what the rest of the board draws.
    pub power_offset_w: f32,
    /// Chip temperature above which the core voltage goes off, °C: the
    /// stock firmware's stop, not its warning level. ESP-Miner stops at
    /// `THROTTLE_TEMP` (75); its `MAX_TEMP` (90) is defined and never
    /// read (v2.15.3 `power_management_task.c` 16–17, 159–161). NerdOS
    /// stops at `overheat_temp` (Kconfig default 70). The setting
    /// `overheat_temp` overrides this per device; 0 = this value.
    pub chip_stop_c: f32,
    /// Regulator temperature above which the core voltage goes off, °C.
    /// ESP-Miner stops at `TPS546_THROTTLE_TEMP` (105); its
    /// `TPS546_MAX_TEMP` (145) is never read either (lines 24–25, 163).
    /// 145 °C is the TPS546's own OT_FAULT ([`tps546`]), which stays as
    /// the hardware backstop behind this. Not configurable.
    pub vr_stop_c: f32,
    /// Added to the board temperature sensors: NerdOS adds 10 °C on the
    /// NerdQAxe++ (`NerdQaxePlus2::getTemperature`) and 3 °C on the
    /// Q13xx carriers, whose sensors sit beside the chips. The fan
    /// target and the overheat limit apply to the corrected value.
    pub temp_offset_c: f32,
    /// Chip temperature the fan PID holds (NerdOS `m_pidSettings[0]`).
    pub fan_target_c: f32,
    /// Fresh work at least this often, ms (NerdOS `m_asicJobIntervalMs`);
    /// 0 = the chip's own default. The Octaxe hashed 8 % below NerdOS
    /// with a new job only every ~22 s and on par at 500 ms.
    pub job_interval_ms: u16,
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
    /// The panel sits the other way round on this carrier than on the
    /// bare LilyGO: NerdOS and the BitMaker Gaia firmware draw every Nerd
    /// carrier rotated 180° as their base, so their `flipscreen = 0` is
    /// upright in the case. XORed with the configured flip (Oliver,
    /// 2026-09-29: "the 180° turn concerns the whole Nerd series").
    pub display_flip: bool,
    /// A GPIO that must enable the chips' power before the regulator
    /// goes on (ESP-Miner `asic_enable`, Naja Duo and Gamma Hex).
    pub asic_enable: Option<AsicEnable>,
    /// A second line the stock firmware pulses high for `reset_low_ms`
    /// and then holds low before its first chip init, which the
    /// firmware repeats before every chain start so the board sees what
    /// it is used to (the NerdNOS's GPIO12, WantClue `_reset`; see
    /// [`NERDNOS`]). `None` on every other board.
    pub stock_reset_pulse: Option<u8>,
    /// The pins a W5500 Ethernet HAT ("NerdHat", Bitronics NERDETH rev
    /// 1.1) sits on, if the board has the header for it ([`EthPins`]);
    /// `None` on every other board. NerdOS v1.1.1 supports the HAT on
    /// the NerdQAxe family (`NerdQaxePlus::getEthPins`), the NerdOctaxe
    /// up to rev 3.3 (`NerdOctaxeGamma::getEthPins`) and the NerdAxe
    /// Gaia over its interposer (`NerdaxeGaia::getEthPins`). The firmware
    /// starts Ethernet only when a W5500 answers on these pins at boot
    /// (`net/eth.rs`); without the HAT the pins are never driven beyond
    /// that one read.
    pub ethernet: Option<EthPins>,
    /// A power estimate for a board without any power reading
    /// ([`PowerEstimate`]); `None` where the regulator reports power.
    pub power_estimate: Option<PowerEstimate>,
    /// Small cores per chip as this board's maker counts them, where
    /// that differs from the chip profile's (the Naja Duo used
    /// ESP-Miner's 6725 until its counters read about 6900 per chip,
    /// like the Gaia's; both now take the chip's 6860). 0 = the chip's own
    /// count. The nominal rate, the efficiency, the self-test's target
    /// and the silence watchdog's window all multiply by it; the
    /// firmware's `board::chip()` carries it into the chip profile.
    pub small_cores: u16,
    /// Whether one chip silent for ten minutes while the others hash has
    /// the chain started over for it (`warpaxe_chain::lone`). `false`
    /// only reports: the silence is logged and shown, nothing restarts.
    /// For a board whose nonce-to-chip attribution has not been seen on
    /// a device yet (the Naja Duo: ESP-Miner reads bit 24 of the nonce,
    /// NerdOS bits 24–25, nobody has run two BM1373 under either), where
    /// a wrong attribution would call a hashing chip silent and restart
    /// a healthy chain every hour. Flip to `true` once the per-chip
    /// nonce counts on the device agree with the hash counters.
    pub silent_chip_restart: bool,
}

/// Where a W5500 Ethernet HAT connects: the SPI lines and chip select
/// as ESP GPIOs, and the reset and interrupt lines where a board wires
/// them. Without `rst` the HAT resets itself by an RC network at power-on
/// and the firmware resets the chip over SPI (MR.RST) at every start;
/// without `int` the driver polls the chip (NerdOS: every millisecond).
/// NerdOS `EthPins` (`main/boards/board.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthPins {
    /// SPI clock GPIO.
    pub sclk: u8,
    /// SPI MOSI GPIO (ESP to W5500).
    pub mosi: u8,
    /// SPI MISO GPIO (W5500 to ESP).
    pub miso: u8,
    /// Chip select GPIO.
    pub cs: u8,
    /// W5500 reset GPIO, if wired.
    pub rst: Option<u8>,
    /// W5500 interrupt GPIO, if wired.
    pub int: Option<u8>,
}

impl EthPins {
    /// Every GPIO the HAT uses, for the collision checks.
    pub fn gpios(&self) -> [Option<u8>; 6] {
        [
            Some(self.sclk),
            Some(self.mosi),
            Some(self.miso),
            Some(self.cs),
            self.rst,
            self.int,
        ]
    }
}

/// The NerdHat on the NerdQAxe family's header (NerdQAxe+, NerdQAxe++,
/// NerdOctaxe up to rev 3.3): NerdOS v1.1.1 `kHatPins` in
/// `nerdqaxeplus.cpp`. No reset line (an RC network on the HAT) and no
/// interrupt line: NerdOS leaves the driver's defaults of GPIO13 and
/// GPIO11 unused on every board (`GPIO_NUM_NC`), and so does WarpAxe.
pub const NERDHAT_QAXE: EthPins = EthPins {
    sclk: 2,
    mosi: 12,
    miso: 16,
    cs: 21,
    rst: None,
    int: None,
};

/// The NerdHat on the NerdAxe Gaia's interposer: NerdOS v1.1.1
/// `kEthPins` in `nerdaxegaia.cpp` (SCLK on JP2 2-3, MOSI on JP3 2-3).
/// Not the QAxe pins: on the Gaia GPIO2 is /VDD, GPIO12 the LDO enable,
/// GPIO13 PMB_ALRT and GPIO11 PGOOD, and SPI traffic on any of them
/// would switch or upset the power rails. Also no reset and no interrupt
/// line.
pub const NERDHAT_GAIA: EthPins = EthPins {
    sclk: 10,
    mosi: 3,
    miso: 16,
    cs: 21,
    rst: None,
    int: None,
};

/// A power figure for a board that measures none: `base_w` for the
/// controller, display and regulator losses plus `per_ghz_w` per GHz of
/// chip clock (dynamic power scales with the clock at a fixed core
/// voltage). Reported as the board's power with `powerEstimated: true`,
/// never as a measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerEstimate {
    /// Watts with the chip clock at zero.
    pub base_w: f32,
    /// Watts per GHz of chip clock.
    pub per_ghz_w: f32,
}

impl PowerEstimate {
    /// The estimate at a chip clock in Hz.
    pub fn watts(&self, hz: u32) -> f32 {
        self.base_w + self.per_ghz_w * (hz as f32 / 1e9)
    }
}

/// The chip power-enable line: driven to `active_high` 20 ms before the
/// regulator output goes on, to the other level when it goes off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsicEnable {
    /// ESP GPIO.
    pub pin: u8,
    /// The level that enables.
    pub active_high: bool,
}

/// Clock and voltage choices a board publishes, with its absolute limits
/// (NerdOS `/api/system/asic` and `/api/v2/settings`); empty lists and 0
/// limits for a board whose stock firmware publishes none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choices {
    /// Clock steps, MHz.
    pub frequency_options: &'static [u32],
    /// Core voltage steps, mV.
    pub voltage_options: &'static [u16],
    /// Highest clock a PATCH may set, MHz; 0 = no limit.
    pub abs_max_frequency_mhz: u32,
    /// Lowest core voltage a PATCH may set, mV; 0 = no limit.
    pub abs_min_core_mv: u16,
    /// Highest core voltage a PATCH may set, mV; 0 = no limit.
    pub abs_max_core_mv: u16,
}

/// NerdOS `NerdOctaxeGamma` with the 6-phase TPS53667 fitted: the
/// extended lists and an 850 MHz ceiling; the voltage limits stay the
/// NerdQAxe++'s it derives from.
const OCTAXE_6P_FREQUENCIES: &[u32] = &[525, 550, 575, 600, 625, 650, 675, 700, 725, 750, 775, 800];
const OCTAXE_6P_VOLTAGES: &[u16] = &[
    1120, 1130, 1140, 1150, 1160, 1170, 1180, 1190, 1200, 1210, 1220, 1230, 1240, 1250, 1260,
];

impl Profile {
    /// Whether the board has a page key (GPIO14) beside its LCD: the Nerd
    /// boards and the T-Display S3 do, a Bitaxe with an LCD (Naja Duo,
    /// Gamma Hex) has only the boot key, which then does both jobs.
    pub fn has_page_key(&self) -> bool {
        self.display == Display::St7789I80 && !self.name.starts_with("bitaxe")
    }

    /// Whether the LCD backlight hangs on an AW9364 (pulse-count
    /// dimming, 16 steps): the Nerd boards and the T-Display S3. A
    /// Bitaxe with an LCD (Naja Duo) switches it some other way that a
    /// PWM dims and the pulses do not (seen on the Naja, 2026-11-14).
    pub fn backlight_aw9364(&self) -> bool {
        self.has_page_key()
    }

    /// How high an owner may set the chip stop: NerdOS lets them choose up
    /// to 90 °C (`range 50 90` in its Kconfig), so its boards keep that;
    /// AxeOS has no such setting, so a Bitaxe (and every other board) can
    /// only go below its own line (Oliver, 2026-10-20).
    pub fn chip_stop_max_c(&self) -> f32 {
        let nerdos = ["nerdqaxe", "nerdoctaxe", "nerdaxe", "q13"]
            .iter()
            .any(|n| self.name.starts_with(n));
        if nerdos {
            90.0
        } else {
            self.chip_stop_c
        }
    }

    /// A stored chip stop above this board's ceiling, as it should be
    /// stored: the ceiling. `None` when it fits (or is 0, the default).
    /// v0.1.32 lowered the Bitaxe ceiling to 75 °C; a 90 kept from before
    /// acts as 75 already, but the web UI's field (max 75) refused it and
    /// with it every save of the settings form.
    pub fn stored_chip_stop_fix(&self, setting: u8) -> Option<u8> {
        let max = self.chip_stop_max_c();
        // Whole degrees, rounded down (`as` truncates a positive float).
        (setting != 0 && f32::from(setting) > max).then_some(max as u8)
    }

    /// A stored core voltage above the board's ceiling (a Naja Duo's
    /// 1150–1250 mV from before v0.1.45): brought down to the ceiling, so
    /// the web UI's field, capped there, does not refuse the whole form.
    pub fn stored_core_mv_fix(&self, mv: u16) -> Option<u16> {
        let max = self.abs_max_core_mv;
        (max > 0 && mv > max).then_some(max)
    }

    /// The chip temperature that stops mining: the owner's setting (0 =
    /// the board's line), never above [`Profile::chip_stop_max_c`].
    pub fn chip_limit_c(&self, setting: u8) -> f32 {
        if setting == 0 {
            self.chip_stop_c
        } else {
            f32::from(setting).min(self.chip_stop_max_c())
        }
    }

    /// The choices this board publishes. The NerdOCTAXE-γ's depend on the
    /// regulator found at start (`tps53667`): the 6-phase part gets
    /// NerdOS's extended lists, the 4-phase TPS53647 the NerdQAxe++'s it
    /// inherits there.
    pub fn choices(&self, tps53667: bool) -> Choices {
        let own = Choices {
            frequency_options: self.frequency_options,
            voltage_options: self.voltage_options,
            abs_max_frequency_mhz: self.abs_max_frequency_mhz,
            abs_min_core_mv: self.abs_min_core_mv,
            abs_max_core_mv: self.abs_max_core_mv,
        };
        if self.name != NERDOCTAXE_GAMMA.name {
            return own;
        }
        let base = NERDQAXE_PLUSPLUS.choices(false);
        if tps53667 {
            Choices {
                frequency_options: OCTAXE_6P_FREQUENCIES,
                voltage_options: OCTAXE_6P_VOLTAGES,
                abs_max_frequency_mhz: 850,
                ..base
            }
        } else {
            base
        }
    }

    /// The board drives a fan: a thermal part with a fan output. A
    /// thermistor-only board (the NerdNOS) and the devkit have none the
    /// firmware can see, whatever is blowing in the case.
    pub fn has_fan(&self) -> bool {
        !matches!(self.thermal, Thermal::None | Thermal::Ntc(_))
    }

    /// The pins to look for a W5500 HAT on, or `None` when Ethernet is
    /// not to be tried: a board without the header, or a board whose
    /// TMP451 temperature multiplexer shares the HAT's SCLK and MOSI
    /// (`tmp451_mux`: the NerdOctaxe from rev 3.4, where GPIO2/12 are the
    /// mux selects A0/A1; NerdOS `NerdOctaxeGamma::getEthPins`). SPI
    /// traffic there would switch the mux, so the HAT is not even probed.
    /// The Gaia's pins are others, so a mux reading never blocks them.
    pub fn ethernet_pins(&self, tmp451_mux: bool) -> Option<EthPins> {
        let pins = self.ethernet?;
        if tmp451_mux && pins == NERDHAT_QAXE {
            return None;
        }
        Some(pins)
    }

    /// The profile for a board name from the config; `None` for a
    /// name this firmware does not know.
    pub fn by_name(name: &str) -> Option<&'static Profile> {
        ALL.iter().find(|p| p.name == name)
    }

    /// The regulator this crate can drive, if the board has one.
    pub fn tps546(&self) -> Option<tps546::Config> {
        match self.vr {
            Vr::Tps546(c)
            | Vr::Tps546Nerd { regulator: c, .. }
            | Vr::Tps546Ina260 { regulator: c, .. } => Some(c),
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

    /// The EMC2103 setup, if the board has one.
    pub fn emc2103(&self) -> Option<emc2103::Config> {
        match self.thermal {
            Thermal::Emc2103(c) => Some(c),
            _ => None,
        }
    }

    /// VOUT to command for a core voltage in mV.
    pub fn vout_for_core_mv(&self, core_mv: u16) -> f32 {
        f32::from(core_mv) / 1000.0 * f32::from(self.voltage_domains.max(1))
    }

    /// The highest VOUT a reading may show and still count: 2 V per
    /// domain. A fixed 2 V dropped every VOUT of a two-domain board above
    /// 1000 mV core (Naja Duo at 1010 mV read 2.02 V, 2026-11-13), and
    /// the power with it.
    pub fn vout_plausible_max(&self) -> f32 {
        2.0 * f32::from(self.voltage_domains.max(1))
    }

    /// A chip profile's small-core count is not the one this board asks
    /// for. A board that asks for none (`small_cores` 0: the chip's own
    /// count) fits every chip profile.
    pub fn small_cores_differ(&self, chip_small_cores: u16) -> bool {
        self.small_cores != 0 && self.small_cores != chip_small_cores
    }

    /// Core voltage in mV for a measured VOUT.
    pub fn core_mv_for_vout(&self, vout: f32) -> u16 {
        (vout / f32::from(self.voltage_domains.max(1)) * 1000.0) as u16
    }

    /// The core voltage the API reports as set, mV: the stored setting.
    /// A board without a regulator (the NerdNOS, fixed 1.4 V rail)
    /// reports the profile's value instead, since no setting reaches that
    /// rail; its stored setting is only the generic default, 1150 mV.
    pub fn reported_core_mv(&self, configured_mv: u16) -> u16 {
        match self.vr {
            Vr::None => self.default_core_mv,
            _ => configured_mv,
        }
    }

    /// The lowest core voltage the regulator holds cleanly, mV per
    /// voltage domain: a TPS546's configured floor, never below the clean
    /// range of its divider ([`tps546::Config::min_vout`]); the TPS5364x
    /// driver's VID floor; 0 without a regulator. Rounded up, so the
    /// value itself is one the driver takes. Published as
    /// `regulatorMinMv`; a core voltage below it is refused whatever the
    /// board's own choices say — never a core voltage below the
    /// regulator's clean range (Oliver, 2026-10-05).
    pub fn regulator_min_core_mv(&self) -> u16 {
        let vout = match self.vr {
            Vr::Tps546(c)
            | Vr::Tps546Nerd { regulator: c, .. }
            | Vr::Tps546Ina260 { regulator: c, .. } => c.min_vout(),
            Vr::Tps5364x { .. } => f32::from(tps53647::VOUT_MIN_MV) / 1000.0,
            _ => return 0,
        };
        let mv = vout / f32::from(self.voltage_domains.max(1)) * 1000.0;
        let mut whole = mv as u16;
        // 0.8 as f32 is a hair above 0.8: a tolerance keeps 800 at 800.
        if f32::from(whole) + 0.01 < mv {
            whole += 1;
        }
        whole
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
    // AxeOS stops where ESP-Miner v2.15.3 `power_management_task.c`
    // compares: chip > THROTTLE_TEMP 75 (lines 159–161), regulator >
    // TPS546_THROTTLE_TEMP 105 (line 163). Up to v0.1.31 these profiles
    // carried the unused MAX_TEMP / TPS546_MAX_TEMP (90 / 145) instead
    // and stopped 15 / 40 °C later than AxeOS. The Duo, Turbo, Naja Duo
    // and Hex inherit them: ESP-Miner has no per-board values.
    chip_stop_c: 75.0,
    vr_stop_c: 105.0,
    temp_offset_c: 0.0,
    fan_target_c: 60.0,
    job_interval_ms: 0,
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    display_flip: false,
    asic_enable: None,
    stock_reset_pulse: None,
    small_cores: 0,
    silent_chip_restart: true,
    ethernet: None,
    power_estimate: None,
};

/// Bitaxe Gamma Duo (650): two BM1370 of the XP bin on the 5 V Gamma
/// carrier (ESP-Miner `FAMILY_GAMMA_DUO`, `ASIC_BM1370XP`): original
/// pins, TPS546 with the 5 V window, EMC2101 on one chip's diode,
/// SSD1306. Stock 400 MHz / 1150 mV, clocks 350–410, 40 W, +5 W offset.
/// Runs at a user's since v0.1.10. The clock steps are the stock ones;
/// free values (`?oc`) have no upper clock limit, as on the Gamma and
/// in AxeOS.
pub const BITAXE_GAMMA_DUO: Profile = Profile {
    name: "bitaxe-gamma-duo",
    model: "Bitaxe Gamma Duo",
    asic_count: 2,
    default_frequency_mhz: 400,
    frequency_options: &[350, 375, 380, 400, 410],
    voltage_options: &[1000, 1060, 1100, 1150, 1200, 1250],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 1000,
    abs_max_core_mv: 1250,
    ..BITAXE_GAMMA
};

/// Bitaxe Gamma Turbo (801): two BM1370 on 12 V, two stacked TPS546
/// (ESP-Miner `TPS546_CONFIG_GAMMA_TURBO`), EMC2103 with the chip on
/// diode 2, SSD1306, original pins. Stock 525 MHz / 1150 mV, 60 W,
/// +10 W offset. Runs at a user's since v0.1.11, one pair at 850 MHz /
/// 1250 mV under AXP90 coolers: free values (`?oc`) have no upper clock
/// limit, as on the Gamma and in AxeOS (John11, 2026-10-06).
pub const BITAXE_GAMMA_TURBO: Profile = Profile {
    name: "bitaxe-gamma-turbo",
    model: "Bitaxe Gamma Turbo",
    asic_count: 2,
    vr: Vr::Tps546(tps546::Config::GAMMA_TURBO),
    thermal: Thermal::Emc2103(emc2103::Config::GAMMA_TURBO),
    nominal_voltage_v: 12,
    max_power_w: 60,
    power_offset_w: 10.0,
    frequency_options: &[400, 490, 525, 550, 600, 625, 690],
    voltage_options: &[1000, 1060, 1100, 1150, 1200, 1250],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 1000,
    abs_max_core_mv: 1250,
    ..BITAXE_GAMMA
};

/// The Bitaxe Color carriers (Naja Duo, Gamma Hex): I²C on 44/43 and
/// the ST7789 over the same i80 pins as the LilyGO T-Display-S3
/// (ESP-Miner `BITAXE_COLOR_PINS`, `ST7789_I80_PINS`), chip power
/// enable on GPIO10 (`CONFIG_GPIO_ASIC_ENABLE`), chain reset GPIO1.
const COLOR_ENABLE: Option<AsicEnable> = Some(AsicEnable {
    pin: 10,
    active_high: true,
});

/// Bitaxe Naja Duo (1201): two BM1373 in two series voltage domains on
/// 12 V, two stacked TPS546 (ESP-Miner `TPS546_CONFIG_NAJA_DUO`, VOUT =
/// 2 × core), EMC2103 with the BM1373 diode fit, ST7789 display,
/// Color pins. Stock 327 MHz / 1000 mV. Runs on our bench since
/// 2026-11-12 (v0.1.44).
/// The preset list goes to 550 MHz, the voltage to 1120 mV (Oliver,
/// 2026-11-15, after the .47 bench); free values (`?oc`) go up to
/// 600 MHz, the maker's documented overclock (Solo Satoshi: 600 MHz /
/// 1120 mV, 8 TH/s, ~92 W, ~38 A of the stacked TPS546's 61 A warning).
/// AxeOS's own overclock mode has no upper bound at all.
pub const BITAXE_NAJA_DUO: Profile = Profile {
    name: "bitaxe-naja-duo",
    model: "Bitaxe Naja Duo",
    asic: Asic::Bm1373,
    asic_count: 2,
    i2c: Some(NERD_I2C),
    vr: Vr::Tps546(tps546::Config::NAJA_DUO),
    voltage_domains: 2,
    thermal: Thermal::Emc2103(emc2103::Config::NAJA_DUO),
    display: Display::St7789I80,
    default_frequency_mhz: 327,
    default_core_mv: 1000,
    nominal_voltage_v: 12,
    // ESP-Miner reports 60 W. WarpTune uses 90 % of `maxPower` as its
    // hard limit: at 60 W that limit (54 W) blocked the Max 410 MHz step
    // on the bench, 2026-11-13 (54.2 W predicted). 90 W at the 2.24 V
    // stacked rail is about 40 A, well below the 61 A warning; the
    // regulator's protection and the temperature limits still guard the
    // board. 105 W since 2026-11-15 (Oliver, .47 bench): WarpTune's
    // limit becomes 94.5 W, enough for 550 MHz at 1120 mV (~92 W by the
    // bench model, 9.8 W static + f·V²); at 95 W the search stopped at
    // 500 MHz. 105 W at 2.24 V is about 47 A, below the 61 A warning.
    max_power_w: 105,
    power_offset_w: 0.0,
    // WarpTune's Max ceiling is the top clock option (550 MHz: at 600 the
    // regulator reached 84 °C without a rear fan, 2026-11-15); 550 MHz
    // needs about 1100 mV on the weaker chip, 1120 leaves 20 mV.
    frequency_options: &[327, 350, 375, 380, 400, 410, 450, 500, 550],
    voltage_options: &[1000, 1060, 1100, 1120],
    abs_max_frequency_mhz: 600,
    abs_min_core_mv: 1000,
    // WarpTune takes `absMaxVoltage` as its voltage ceiling; the maker's
    // own overclock stops at 1120 mV too. Manual values are capped with
    // it, and a higher stored value is brought down at start
    // (`stored_core_mv_fix`).
    abs_max_core_mv: 1120,
    // The factory NVS of a delivered board (config-1201.csv, read
    // 2026-11-12): `temptarget 50`; ESP-Miner's own default is 60.
    fan_target_c: 50.0,
    asic_enable: COLOR_ENABLE,
    // The chip's own count (NerdOS's 6860), as on the Gaia: both chips'
    // hash counters read about 6900 cores at a fixed clock (2026-11-13,
    // ESP-Miner issue #2102), ESP-Miner's 6725 was 2.5 % low.
    small_cores: 0,
    // Report only until the nonce-to-chip attribution has been seen on a
    // device: see the field. One line to arm it after the first run.
    silent_chip_restart: false,
    ..BITAXE_GAMMA
};

/// Bitaxe Gamma Hex (1300): six BM1370 in two series voltage domains on
/// 12 V, four stacked TPS546 (ESP-Miner `TPS546_CONFIG_GAMMA_HEX`),
/// EMC2103 in direct PWM with the chip on diode 2, ST7789 display drawn
/// rotated (`display_flip`), Color pins. Stock 690 MHz / 1200 mV, 180 W,
/// +25 W offset. Profile only: never run under WarpAxe.
pub const BITAXE_GAMMA_HEX: Profile = Profile {
    name: "bitaxe-gamma-hex",
    model: "Bitaxe Gamma Hex",
    asic_count: 6,
    i2c: Some(NERD_I2C),
    vr: Vr::Tps546(tps546::Config::GAMMA_HEX),
    voltage_domains: 2,
    thermal: Thermal::Emc2103(emc2103::Config::GAMMA_HEX),
    display: Display::St7789I80,
    default_frequency_mhz: 690,
    default_core_mv: 1200,
    nominal_voltage_v: 12,
    max_power_w: 180,
    power_offset_w: 25.0,
    frequency_options: &[400, 490, 525, 550, 600, 625, 690],
    voltage_options: &[1000, 1060, 1100, 1150, 1200, 1250],
    abs_max_frequency_mhz: 690,
    abs_min_core_mv: 1000,
    abs_max_core_mv: 1250,
    display_flip: true,
    asic_enable: COLOR_ENABLE,
    ..BITAXE_GAMMA
};

/// BitForge Nano (board version 800 under ForgeOS): two BM1370 in
/// parallel on a 12 V carrier with the Bitaxe pin set (UART 17/18, reset
/// GPIO1, I²C 47/48), one TPS546D24A, an INA260 on the input rail, two
/// EMC2101 behind a PCA9544 mux (channels 2 and 3, one per chip, each
/// driving its own fan), no display (schematic
/// WantClueTechnologies/BitForge-Nano, CERN-OHL-S; its stock firmware
/// ForgeOS is open source too, github.com/wantclue/forge-os, GPL-3.0).
/// Stock 525 MHz / 1150 mV, 60 W (ForgeOS v1.7 reports the 60 W, it
/// doesn't enforce it). The clock and voltage choices are WarpTune's
/// vendor table for the board (425/1000, 525/1150, 625/1160, up to
/// 700 MHz and 1200 mV); ForgeOS's own presets are 430/1000, 525/1150
/// and 636/1160. Runs under WarpAxe since 2026-09-29; power comes from
/// the INA260, so no offset.
pub const BITFORGE_NANO: Profile = Profile {
    name: "bitforge-nano",
    model: "BitForge Nano",
    asic: Asic::Bm1370,
    asic_count: 2,
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
    vr: Vr::Tps546Ina260 {
        regulator: tps546::Config::BITFORGE_NANO,
        ina260: ina260::ADDR,
    },
    voltage_domains: 1,
    // The maker's diode tuning (ideality 0x37), not ESP-Miner's 0x24:
    // the same chip reads about 8 °C lower with it, so the 75 °C line
    // and the API's figures are on the scale ForgeOS runs the board on
    // (`emc2101::Config::BITFORGE_NANO_DIODE`, measured 2026-10-23;
    // before v0.1.36 the Nano ran on 0x24). The fan target moves with
    // the scale, see `fan_target_c` below.
    thermal: Thermal::DualEmc2101Mux {
        config: emc2101::Config::BITFORGE_NANO_DIODE,
        mux: pca9544::ADDR,
        channels: [2, 3],
    },
    display: Display::None,
    default_frequency_mhz: 525,
    default_core_mv: 1150,
    init_core_mv: 0,
    nominal_voltage_v: 12,
    max_power_w: 60,
    power_offset_w: 0.0,
    // ForgeOS v1.7's own stops, read from its source: either chip
    // > 75 °C, regulator > 105 °C (`main/tasks/power_management_task.c`
    // 21, 28, 149-151). Under WarpAxe the board reached 82 °C on the
    // 0x24 diode scale (about 74 °C on the maker's 0x37 scale used since
    // v0.1.36; regulator 95 °C) before the fans were fixed, below neither
    // of the old 90 / 145 lines.
    chip_stop_c: 75.0,
    vr_stop_c: 105.0,
    temp_offset_c: 0.0,
    // 52 on the 0x37 scale is the chip temperature the 60 °C target held
    // on the 0x24 scale up to v0.1.35 (60 − 8.2, see the diode note
    // above): the fans run as before and the chips stay as cool. The
    // first v0.1.36 candidate kept 60 on the new scale, and the Nano .44
    // at Eco 425 / 1000 answered with a 6 % error rate instead of 1.5 %,
    // the fans at 35 % and the core at 28 A instead of at most 24.9 A
    // (leakage of warmer chips; 2026-10-23, 10 min). ForgeOS itself runs
    // no target but a curve (chip 45→75 °C = fan 35→100 %,
    // `power_management_task.c` 39–86), about 67 % at 60 °C, so it holds
    // the chips cooler than a 60 °C target would. Oliver, 2026-10-23.
    // A Nano that still has the old 60 stored goes back to the profile's
    // value once (`NANO_FAN_TARGET_BEFORE_V0136`, `migrated_fan_target`).
    fan_target_c: 52.0,
    job_interval_ms: 0,
    frequency_options: &[425, 525, 625],
    voltage_options: &[1000, 1150, 1160],
    abs_max_frequency_mhz: 700,
    abs_min_core_mv: 1000,
    abs_max_core_mv: 1200,
    display_flip: false,
    asic_enable: None,
    stock_reset_pulse: None,
    small_cores: 0,
    silent_chip_restart: true,
    ethernet: None,
    power_estimate: None,
};

/// The NerdMiner V2 / bare LilyGO T-Display-S3 as devkit: chain UART on
/// 17/18 (to the simulator), no regulator, no sensor. GPIO47/48 are
/// display data lines here, so no I²C.
pub const T_DISPLAY_S3_DEVKIT: Profile = Profile {
    name: "t-display-s3",
    // What the display, the splash and the web UI call the devkit: the
    // toy's own name (Oliver, 2026-10-05), not the module's.
    model: "WarpMiner",
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
    // The temperature here is the ESP32-S3's own die (the cpu-miner build
    // reads the chip's sensor; there is no other). The ESP32-S3R8 in the
    // T-Display-S3 is rated for -40 to 65 °C ambient (ESP32-S3 datasheet,
    // Series Comparison: the in-package octal PSRAM, where the FN8 has
    // 85 and the plain S3 105), and the die runs about 20 K above ambient
    // under load (44 °C measured in a 24 °C room). So: red from 85 °C,
    // yellow from 70 (target + 5). The CPU miner's coordinator stops
    // hashing at chip_stop_c and resumes 10 °C below it (cpuminer.rs);
    // there is no regulator to switch off, so the regulator stop is
    // reported only (AxeOS's figure, for a swarm card reading it).
    chip_stop_c: 85.0,
    vr_stop_c: 105.0,
    temp_offset_c: 0.0,
    fan_target_c: 65.0,
    job_interval_ms: 0,
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    display_flip: false,
    asic_enable: None,
    stock_reset_pulse: None,
    small_cores: 0,
    silent_chip_restart: true,
    ethernet: None,
    power_estimate: None,
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
    // NerdOS v1.1.0.1 stops the chip at `overheat_temp`, Kconfig default
    // 70 (`main/Kconfig.projbuild` 24–27; `fan_controller.cpp` 114,
    // `power_management_task.cpp` 305–314). The regulator has no stop
    // there: a one-fan board gets `fan1_overheat = 0` (`nvs_config.cpp`
    // 246–247), and `m_vr_maxTemp = TPS546_THROTTLE_TEMP` in
    // `nerdaxegamma.cpp` 40 is a dashboard figure, never compared. Only
    // the TPS546's own 145 °C OT_FAULT protects it. WarpAxe keeps AxeOS's
    // 105 °C stop on purpose: same regulator, same carrier class as the
    // Bitaxe Gamma, and a stop 40 °C below the part's fault is the
    // safer default (Oliver's call, 2026-10-20 handoff).
    chip_stop_c: 70.0,
    vr_stop_c: 105.0,
    temp_offset_c: 0.0,
    fan_target_c: 60.0,
    job_interval_ms: 1500,
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    display_flip: true,
    asic_enable: None,
    stock_reset_pulse: None,
    small_cores: 0,
    silent_chip_restart: true,
    ethernet: None,
    power_estimate: None,
};

/// NerdAxe Gaia: one BM1373 on a 12 V carrier (BitMaker fork of NerdOS,
/// `nerdaxegaia.cpp` at ae509770, the firmware on the reference board).
/// TPS546 with the 12 V window and 400 kHz ([`tps546::Config::NERDAXE_GAIA`]),
/// LDO on GPIO12 up before the bus, EMC2101 on the chip diode with one
/// fan (ideality 0x24, beta off, as on the BM1370), chain reset GPIO1,
/// UART 17/18, I²C 44/43. Stock 350 MHz / 940 mV, brought up at
/// 1000 mV, 40 W gauge with +5 W offset, fan target 65 °C and overheat
/// 75 °C as the reference board runs (its NVS; the code defaults are 60
/// and 70 °C, `nerdaxegaia.cpp` 44 and `Kconfig.projbuild` 27 at ae509770).
/// Clock and core voltage capped at 420 MHz / 970 mV: at 440 MHz /
/// 970 mV the regulator dropped the core voltage without a fault flag
/// (WarpTune, 2026-09), so the stock 440 MHz and 980–1040 mV options
/// are not offered. That ran on BitMaker's 0.25 divider, below the
/// part's clean range; since v0.1.16 the divider is 0.5 (see
/// [`tps546::Config::NERDAXE_GAIA`]) and the cap is up for a re-check
/// on the board, not lifted here. Profile only: never run under WarpAxe; the BM1373
/// chain path has only run in simulation.
pub const NERDAXE_GAIA: Profile = Profile {
    name: "nerdaxe-gaia",
    model: "NerdAxe Gaia",
    asic: Asic::Bm1373,
    asic_count: 1,
    uart_tx: 17,
    uart_rx: 18,
    asic_reset: Some(1),
    // NerdOS: 250 + 100 ms in reset before the rail, 250 ms after.
    reset_low_ms: 350,
    reset_release_ms: 250,
    i2c: Some(NERD_I2C),
    vr: Vr::Tps546Nerd {
        regulator: tps546::Config::NERDAXE_GAIA,
        enable: None,
        ldo_enable: Some(12),
    },
    voltage_domains: 1,
    thermal: Thermal::Emc2101(emc2101::Config::BM1370_DIODE),
    display: Display::St7789I80,
    default_frequency_mhz: 350,
    default_core_mv: 940,
    init_core_mv: 1000,
    nominal_voltage_v: 12,
    max_power_w: 40,
    power_offset_w: 5.0,
    // The reference board's setting rather than the Kconfig default 70
    // (a board switched over keeps its own `overheat_temp` anyway).
    chip_stop_c: 75.0,
    // BitMaker's firmware (NerdOS, one fan) has no regulator stop, as on
    // the NerdAxe Gamma: `m_vr_maxTemp = TPS546_THROTTLE_TEMP`
    // (`nerdaxegaia.cpp` 42) is display only, `fan1_overheat` is forced
    // to 0 for the NerdAxe family. 105 kept on purpose, see NERDAXE_GAMMA.
    vr_stop_c: 105.0,
    temp_offset_c: 0.0,
    fan_target_c: 65.0,
    // BitMaker uses 1500 ms; under WarpAxe that made 87 % of the
    // expected rate at 420 MHz and 500 ms made 102 % (pool, ±5 %).
    job_interval_ms: 500,
    frequency_options: &[300, 320, 340, 350, 360, 380, 400, 420],
    voltage_options: &[900, 920, 940, 960],
    abs_max_frequency_mhz: 420,
    abs_min_core_mv: 800,
    abs_max_core_mv: 970,
    // Seen upside down under WarpAxe with the imported flipscreen = 0
    // (2026-09-29), like the other Nerd carriers.
    display_flip: true,
    asic_enable: None,
    stock_reset_pulse: None,
    small_cores: 0,
    silent_chip_restart: true,
    // The NerdHat over the Gaia's interposer (NerdOS v1.1.1, PR #724).
    ethernet: Some(NERDHAT_GAIA),
    power_estimate: None,
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
    // NerdOS v1.1.0.1: `overheat_temp` (Kconfig default 70, range 50–90)
    // on fan channel 0 for the chips, `fan1_overheat` on channel 1 for
    // the regulator, which a two-fan board inherits from the chip value
    // (`nvs_config.cpp` 249–254); either channel over its line switches
    // the buck off (`power_management_task.cpp` 305–314), and NerdOS
    // stays off. No throttle step. The Octaxe, Q1370 and Q1373 inherit.
    chip_stop_c: 70.0,
    vr_stop_c: 70.0,
    // NerdOS `NerdQaxePlus2::getTemperature`: TMP1075 + 10 °C ("should
    // be about right" for the chips). Without it the 70 °C limit would
    // let the sensors reach what NerdOS calls 80 °C.
    temp_offset_c: 10.0,
    fan_target_c: 55.0,
    job_interval_ms: 500,
    // NerdOS 1.1.0 on a Rev 6.1 board, `/api/system/asic` (2026-09-27).
    frequency_options: &[500, 515, 525, 550, 575, 590, 600],
    voltage_options: &[1120, 1130, 1140, 1150, 1160, 1170, 1180, 1190, 1200],
    abs_max_frequency_mhz: 800,
    abs_min_core_mv: 1050,
    abs_max_core_mv: 1400,
    display_flip: true,
    asic_enable: None,
    stock_reset_pulse: None,
    small_cores: 0,
    silent_chip_restart: true,
    // The NerdHat on the family's header (NerdOS v1.1.1, PR #731); the
    // Rev 7 and the NerdOctaxe inherit it, the NerdQX below does not.
    ethernet: Some(NERDHAT_QAXE),
    power_estimate: None,
};

/// NerdQAxe+: the NerdQAxe++'s carrier with four BM1368 and the
/// TPS53647 on two phases (60 A, fault 55 A). NerdOS v1.1.2
/// `main/boards/nerdqaxeplus.cpp` (the NerdQAxe++ is the same class with
/// BM1370, `NerdQaxePlus2 : NerdQaxePlus`): the same pins, sensors, fans,
/// display and NerdHat header; 490 MHz / 1250 mV, brought up at 1250 mV,
/// 70 W (`maxPin`), the option lists, job interval 1200 ms. Profile only:
/// never run under WarpAxe.
///
/// The chip temperature: NerdOS takes the hotter of the TMP1075 and the
/// BM1368's own diode reading (register 0xB4, `asic_result_task.cpp`)
/// and adds nothing; WarpAxe does not read the BM1368's diode, so it
/// keeps the NerdQAxe++'s TMP1075 + 10 °C, which stands in for the chip
/// on the same carrier.
pub const NERDQAXE_PLUS: Profile = Profile {
    name: "nerdqaxe-plus",
    model: "NerdQAxe+",
    asic: Asic::Bm1368,
    vr: Vr::Tps5364x {
        regulator: tps53647::Config::NERDQAXE_PLUS,
        enable: 10,
        ldo_enable: Some(13),
        via_expander: false,
        alt: None,
    },
    default_frequency_mhz: 490,
    default_core_mv: 1250,
    init_core_mv: 1250,
    max_power_w: 70,
    job_interval_ms: 1200,
    frequency_options: &[400, 425, 450, 475, 490, 500, 525, 550, 575],
    voltage_options: &[1100, 1150, 1200, 1250, 1300, 1350],
    ..NERDQAXE_PLUSPLUS
};

/// NerdQAxe++ Rev 7: four BM1370 in two series voltage domains on one
/// TPS546 (NerdOS `Rev7TPS546`, VOUT = 2 × core), the Rev 6 carrier
/// pins (enable GPIO10, LDO GPIO13), 1200 mV default, 120 W, fan target
/// 65 °C, clock up to 1000 MHz (NerdOS `applyRev7Profile`). Its
/// `m_vr_maxTemp = TPS546_THROTTLE_TEMP` (`nerdqaxeplus2.cpp` 65) is a
/// dashboard figure; the stop stays the two-fan board's `fan1_overheat`
/// (70, inherited below). Profile only: never run under WarpAxe.
pub const NERDQAXE_PLUSPLUS_REV7: Profile = Profile {
    name: "nerdqaxe-plusplus-rev7",
    model: "NerdQAxe++ Rev 7",
    vr: Vr::Tps546Nerd {
        regulator: tps546::Config::NERDQAXE_PLUSPLUS_REV7,
        enable: Some(10),
        ldo_enable: Some(13),
    },
    voltage_domains: 2,
    default_core_mv: 1200,
    init_core_mv: 1200,
    max_power_w: 120,
    fan_target_c: 65.0,
    job_interval_ms: 500,
    frequency_options: &[
        500, 515, 525, 550, 575, 590, 600, 625, 650, 675, 700, 725, 750, 775, 800, 825, 850, 875,
        900, 925, 950, 975, 1000,
    ],
    voltage_options: &[
        1120, 1130, 1140, 1150, 1160, 1170, 1180, 1190, 1200, 1210, 1220, 1230, 1240, 1250, 1260,
        1270, 1280, 1290, 1300,
    ],
    abs_max_frequency_mhz: 1000,
    ..NERDQAXE_PLUSPLUS
};

/// NerdOctaxe-γ: eight BM1370 on 12 V, TPS53667 six-phase (or TPS53647
/// four-phase when VR_DETECT GPIO3 reads low:
/// [`tps53647::Config::NERDOCTAXE_GAMMA_4P`]), 700 MHz / 1210 mV, 300 W.
pub const NERDOCTAXE_GAMMA: Profile = Profile {
    name: "nerdoctaxe-gamma",
    model: "NerdOctaxe",
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
            part_by_code: false,
        }),
    },
    default_frequency_mhz: 700,
    default_core_mv: 1210,
    init_core_mv: 0,
    max_power_w: 300,
    // NerdOS `NerdOctaxeGamma` derives from `NerdQaxePlus2` and does not
    // override `getTemperature`, so its TMP1075 reading carries the same
    // +10 °C as the NerdQAxe++; the per-chip TMP451 path reads 0 on the
    // Rev 3.1 board. Seen 2026-09-28 on .45: 55 °C here vs 66.3 °C under
    // NerdOS at the same point.
    temp_offset_c: 10.0,
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
    job_interval_ms: 500,
    // Options depend on the fitted regulator; published once this
    // board is brought up.
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    // NerdOS v1.1.1 drives a W5500 on the QAxe pins here too (`nerdqx.h`,
    // no HAT when the TMP451 mux is fitted); not part of WarpAxe's first
    // Ethernet round, which covers the boards that have run here.
    ethernet: None,
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
            part_by_code: true,
        }),
    },
    default_frequency_mhz: 425,
    default_core_mv: 1010,
    init_core_mv: 1050,
    max_power_w: 180,
    temp_offset_c: 3.0,
    fan_target_c: 60.0,
    job_interval_ms: 500,
    // Options depend on the fitted regulator; published once this
    // board is brought up.
    frequency_options: &[],
    voltage_options: &[],
    abs_max_frequency_mhz: 0,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    // NerdOS v1.1.1 drives a W5500 on the QAxe pins here too (`nerdqx.h`,
    // no HAT when the TMP451 mux is fitted); not part of WarpAxe's first
    // Ethernet round, which covers the boards that have run here.
    ethernet: None,
    ..NERDQAXE_PLUSPLUS
};

/// Every profile this firmware knows.
/// What a first start runs on until the board is known: no bus, no
/// regulator, no display, no chips; the web UI asks which board it is.
/// NerdNOS: a LilyGO T-Display-S3 on a carrier with one BM1397. Pins as
/// WantClue's `nerdnos.h` has them (NerdMiner_v2 branch `nerdnos`,
/// first working version 2024-09-07, the firmware on Oliver's .46):
/// chain UART on GPIO17/18 (the S3's native U1, right header) through
/// a TXU0202 level shifter, the NTC on GPIO2, the core rail sensed on
/// GPIO3, and two control lines the stock firmware calls RST (GPIO12:
/// driven high 100 ms, then low, once at init) and PEN (GPIO13: high
/// from init on, low to stop the chip after an overheat, high again
/// before a re-init).
///
/// The schematic of this revision is public after all: WantClue/NerdNOS
/// (rev 1.3, last change 2024-10-31, a fork of benjamin-wilson/NerdNOS
/// whose own KiCad stopped at an older board in 2024-05). Its PCB nets
/// confirm every pin above: U1 pad 3 = GPIO2 = TMP (R8 10 k to 3V3, the
/// NTC to GND), pad 4 = GPIO3 = the 1V4 rail, pad 7 = GPIO12 = RST (R14
/// 10 k to 3V3, Q1 2N7002 inverting into RST_N, which R1 5.1 k pulls to
/// 1V8: GPIO12 high = chip in reset, hence the stock's 100 ms high
/// pulse), pad 8 = GPIO13 = PEN = the buck's EN (R11 10 k to GND: low =
/// rail off), pads 19/20 = GPIO17/18 = TX/RX through a TXU0202, pad 12 =
/// the T-Display's 5 V through D1 1N5817 from the carrier's USB-C J3 (so
/// the chip only runs with the carrier powered). WarpAxe drives the
/// lines as the stock firmware does: `asic_reset` is GPIO13 (low at boot
/// and on an overheat stop, a low–high pulse before every init), and
/// GPIO12 gets the stock pulse after it (`stock_reset_pulse`). The core
/// voltage is fixed by the buck's divider (U7 TPS566247, 0.6 V reference,
/// R12 13.3 k / R10 10 k → 1.398 V; 1397–1400 mV read on GPIO3): no
/// voltage choices, `default_core_mv` is reported only. USB-C powered,
/// a 2-pin 5 V fan header (J1) without control: the clock is capped at
/// 400 MHz until someone has measured more.
pub const NERDNOS: Profile = Profile {
    name: "nerdnos",
    model: "NerdNOS",
    asic: Asic::Bm1397,
    asic_count: 1,
    uart_tx: 17,
    uart_rx: 18,
    asic_reset: Some(13),
    // WantClue `_reset`: 100 ms in, 100 ms out.
    reset_low_ms: 100,
    reset_release_ms: 100,
    i2c: None,
    vr: Vr::None,
    voltage_domains: 1,
    thermal: Thermal::Ntc(ntc::Config::NERDNOS),
    display: Display::St7789I80,
    // `BM1397_init(300, 1)`.
    default_frequency_mhz: 300,
    default_core_mv: 1400,
    init_core_mv: 0,
    nominal_voltage_v: 5,
    // ~8 W at stock per the OSMU wiki; the USB-C port is the limit.
    max_power_w: 10,
    power_offset_w: 0.0,
    // WantClue `MAX_SAFE_TEMP 80.0` (`mining_nerdnos.cpp` 51, stop at
    // 292, back below 70 at 297, re-init at 210 MHz at 303) on a
    // thermistor beside the chip; yellow from 70. No regulator
    // temperature: the regulator stop is reported only.
    chip_stop_c: 80.0,
    vr_stop_c: 105.0,
    temp_offset_c: 0.0,
    fan_target_c: 65.0,
    // The chip's own 30 ms (WantClue `NERDNOS_JOB_INTERVAL_MS`).
    job_interval_ms: 0,
    frequency_options: &[200, 225, 250, 275, 300, 325, 350, 375, 400],
    voltage_options: &[],
    abs_max_frequency_mhz: 400,
    abs_min_core_mv: 0,
    abs_max_core_mv: 0,
    // Unknown until seen: the carrier may sit either way round in a
    // case. The setting flips it.
    display_flip: false,
    asic_enable: None,
    stock_reset_pulse: Some(12),
    // The OSMU wiki's "about 8 W at stock" (300 MHz) less about 1.5 W for
    // the T-Display-S3, its LCD and the LDOs: 21.7 W per GHz of clock.
    // Nothing on the board measures power.
    small_cores: 0,
    silent_chip_restart: true,
    ethernet: None,
    power_estimate: Some(PowerEstimate {
        base_w: 1.5,
        per_ghz_w: 21.7,
    }),
};

/// The Nano's fan target up to v0.1.35 (AxeOS's default, on the 0x24
/// diode scale of the time), as a device may still have it stored in
/// `pidTargetTemp`: a settings save in the web UI stores the shown
/// value (60 when nothing was set), a PATCH or the owner may have
/// written it. ForgeOS stores no fan target of its own.
pub const NANO_FAN_TARGET_BEFORE_V0136: u8 = 60;

/// The one-time move of a Nano's stored fan target to the new diode
/// scale (v0.1.36): a stored target that is exactly the old default
/// goes back to 0 (= the profile's `fan_target_c`, 52 on the new
/// scale), any other stored value is the owner's and stays. `None` =
/// nothing to change. Other boards never change: their scale did not
/// move. The regulator target (`vrTargetTemp`) is not touched either: it
/// reads the TPS546's own sensor, not the diode.
pub fn migrated_fan_target(p: &Profile, stored_target_c: u8) -> Option<u8> {
    (p.name == BITFORGE_NANO.name && stored_target_c == NANO_FAN_TARGET_BEFORE_V0136).then_some(0)
}

/// Not in [`ALL`], so it cannot be configured by name.
pub const UNDETECTED: Profile = Profile {
    name: "undetected",
    model: "Board not detected",
    asic_count: 0,
    asic_reset: None,
    i2c: None,
    vr: Vr::None,
    thermal: Thermal::None,
    display: Display::None,
    ..T_DISPLAY_S3_DEVKIT
};

/// [`UNDETECTED`] on a Nerd carrier (LilyGO T-Display-S3, LCD upside
/// down): the setup hotspot's password has somewhere to go while the
/// owner picks the board.
pub const UNDETECTED_LCD_FLIPPED: Profile = Profile {
    display: Display::St7789I80,
    display_flip: true,
    ..UNDETECTED
};

/// [`UNDETECTED`] on a T-Display-S3 with the LCD upright.
pub const UNDETECTED_LCD: Profile = Profile {
    display: Display::St7789I80,
    display_flip: false,
    ..UNDETECTED
};

/// [`UNDETECTED`] on a Bitaxe carrier: the SSD1306 on the Bitaxe pins.
pub const UNDETECTED_OLED_BITAXE: Profile = Profile {
    i2c: BITAXE_GAMMA.i2c,
    display: BITAXE_GAMMA.display,
    display_flip: BITAXE_GAMMA.display_flip,
    ..UNDETECTED
};

/// The profile to run on while the owner chooses between `candidates`:
/// [`UNDETECTED`] with their display when they all share one (display,
/// its bus and orientation), so the setup hotspot's password is shown;
/// plain [`UNDETECTED`] (no display) otherwise. Seen 2026-10-01 on a
/// NerdOctaxe v2.2: the LCD stayed dark in the choose state and the
/// hotspot password was nowhere to read.
pub fn undetected_for(candidates: &[&Profile]) -> &'static Profile {
    let Some(first) = candidates.first() else {
        return &UNDETECTED;
    };
    // The bus matters only to a display that sits on it.
    let key = |p: &Profile| {
        let bus = match p.display {
            Display::Ssd1306 { .. } => p.i2c,
            _ => None,
        };
        (p.display, p.display_flip, bus)
    };
    let same = |p: &Profile| key(p) == key(first);
    if !candidates.iter().all(|p| same(p)) {
        return &UNDETECTED;
    }
    [
        &UNDETECTED_LCD_FLIPPED,
        &UNDETECTED_LCD,
        &UNDETECTED_OLED_BITAXE,
    ]
    .into_iter()
    .find(|u| same(u))
    .unwrap_or(&UNDETECTED)
}

/// Every board a `boardModel` can name.
pub const ALL: &[Profile] = &[
    BITAXE_GAMMA,
    BITAXE_GAMMA_DUO,
    BITAXE_GAMMA_TURBO,
    BITAXE_NAJA_DUO,
    BITAXE_GAMMA_HEX,
    BITFORGE_NANO,
    T_DISPLAY_S3_DEVKIT,
    NERDNOS,
    NERDAXE_GAMMA,
    NERDAXE_GAIA,
    NERDQAXE_PLUSPLUS,
    NERDQAXE_PLUSPLUS_REV7,
    NERDOCTAXE_GAMMA,
    // After the NerdQAxe++ and the Octaxe it looks like: offered last.
    NERDQAXE_PLUS,
    Q1370,
    Q1373,
];

#[cfg(test)]
mod tests {

    /// The stop lines are the stock firmware's, board by board: ESP-Miner
    /// v2.15.3 compares against THROTTLE_TEMP 75 / TPS546_THROTTLE_TEMP
    /// 105, NerdOS against `overheat_temp` 70 on both fan channels,
    /// WantClue's NerdNOS against MAX_SAFE_TEMP 80. Up to v0.1.31 the
    /// Bitaxe profiles carried ESP-Miner's unused MAX_TEMP constants
    /// (90 / 145) and stopped 15 / 40 °C after AxeOS.
    #[test]
    fn a_bitaxe_chip_stop_only_goes_down_a_nerd_one_up_to_90() {
        let gamma = Profile::by_name("bitaxe-gamma").unwrap();
        assert_eq!(gamma.chip_limit_c(0), 75.0);
        assert_eq!(gamma.chip_limit_c(70), 70.0);
        assert_eq!(gamma.chip_limit_c(90), 75.0, "a stored 90 is capped");
        let nq = Profile::by_name("nerdqaxe-plusplus").unwrap();
        assert_eq!(nq.chip_limit_c(0), 70.0);
        assert_eq!(nq.chip_limit_c(85), 85.0);
        assert_eq!(nq.chip_limit_c(95), 90.0);
        for p in ALL {
            assert!(p.chip_stop_max_c() >= p.chip_stop_c, "{}", p.name);
            assert!(p.chip_stop_max_c() <= 90.0, "{}", p.name);
        }
    }

    /// The Naja Duo's own figures from its maker's firmware and factory
    /// NVS; nothing else carries a board small-core count, and every
    /// other board restarts its chain for a silent chip.
    #[test]
    fn naja_duo_counts_the_chips_own_cores_and_only_reports_silence() {
        use super::*;
        let naja = Profile::by_name("bitaxe-naja-duo").unwrap();
        assert_eq!(naja.small_cores, 0);
        assert_eq!(naja.fan_target_c, 50.0);
        assert!(!naja.silent_chip_restart);
        let vr = naja.tps546().unwrap();
        assert_eq!(
            (vr.iout_oc_warn, vr.iout_oc_fault),
            (61.0, 68.0),
            "ESP-Miner master f5e74b1"
        );
        assert_eq!(
            naja.max_power_w, 105,
            "WarpTune's limit 94.5 W fits 550 MHz / 1120 mV (~92 W)"
        );
        // WarpTune's Max ceiling: the top clock option, and absMaxVoltage
        // as its voltage ceiling (Oliver, 2026-11-15).
        assert_eq!(naja.frequency_options.last(), Some(&550));
        assert_eq!(naja.voltage_options.last(), Some(&1120));
        let c = naja.choices(false);
        assert_eq!((c.abs_max_frequency_mhz, c.abs_max_core_mv), (600, 1120));
        assert_eq!(naja.stored_core_mv_fix(1250), Some(1120));
        assert_eq!(naja.stored_core_mv_fix(1121), Some(1120));
        assert_eq!(naja.stored_core_mv_fix(1120), None);
        assert_eq!(naja.stored_core_mv_fix(1000), None);
        assert_eq!(
            T_DISPLAY_S3_DEVKIT.stored_core_mv_fix(1400),
            None,
            "no ceiling"
        );
        assert_eq!(
            migrated_fan_target(&BITAXE_NAJA_DUO, 60),
            None,
            "the Nano's one-time move never touches the Naja's stored target"
        );
        for p in ALL {
            if p.name == BITAXE_NAJA_DUO.name {
                continue;
            }
            assert_eq!(p.small_cores, 0, "{}", p.name);
            assert!(p.silent_chip_restart, "{}", p.name);
        }
        assert_eq!(NERDAXE_GAIA.small_cores, 0);
        assert_eq!(NERDAXE_GAIA.fan_target_c, 65.0);
    }

    #[test]
    fn stop_limits_are_the_stock_firmwares() {
        use super::*;
        let axeos = [
            &BITAXE_GAMMA,
            &BITAXE_GAMMA_DUO,
            &BITAXE_GAMMA_TURBO,
            &BITAXE_NAJA_DUO,
            &BITAXE_GAMMA_HEX,
            &BITFORGE_NANO,
        ];
        for p in axeos {
            assert_eq!((p.chip_stop_c, p.vr_stop_c), (75.0, 105.0), "{}", p.name);
        }
        // NerdOS two-fan boards: chip and regulator at the same 70.
        for p in [
            &NERDQAXE_PLUSPLUS,
            &NERDQAXE_PLUSPLUS_REV7,
            &NERDOCTAXE_GAMMA,
            &Q1370,
            &Q1373,
        ] {
            assert_eq!((p.chip_stop_c, p.vr_stop_c), (70.0, 70.0), "{}", p.name);
        }
        // NerdOS one-fan boards: NerdOS has no regulator stop at all;
        // WarpAxe keeps AxeOS's 105 on the same TPS546.
        assert_eq!(
            (NERDAXE_GAMMA.chip_stop_c, NERDAXE_GAMMA.vr_stop_c),
            (70.0, 105.0)
        );
        assert_eq!(
            (NERDAXE_GAIA.chip_stop_c, NERDAXE_GAIA.vr_stop_c),
            (75.0, 105.0)
        );
        assert_eq!(NERDNOS.chip_stop_c, 80.0);
        assert_eq!(T_DISPLAY_S3_DEVKIT.chip_stop_c, 85.0);
        for p in ALL {
            // Nothing stops above the old lines, and every chip line sits
            // above the fan target (NerdOS leaves the Rev 7 only 5 °C).
            assert!(p.chip_stop_c <= 90.0 && p.vr_stop_c <= 105.0, "{}", p.name);
            assert!(p.chip_stop_c > p.fan_target_c, "{}", p.name);
            // The regulator stop stays under the TPS546's 145 °C OT_FAULT
            // by a wide margin on every board.
            assert!(p.vr_stop_c <= 145.0 - 40.0, "{}", p.name);
        }
    }

    #[test]
    fn nerd_job_intervals_follow_nerdos() {
        for (name, ms) in [
            ("nerdqaxe-plusplus", 500),
            ("nerdqaxe-plusplus-rev7", 500),
            // NerdOS v1.1.2 `nerdqaxeplus.cpp`: 1200 ms (the NerdQAxe++
            // class sets its own 500).
            ("nerdqaxe-plus", 1200),
            ("nerdoctaxe-gamma", 500),
            ("q1370", 500),
            ("q1373", 500),
            ("nerdaxe-gamma", 1500),
            ("nerdaxe-gaia", 500),
            ("bitaxe-gamma", 0),
            ("bitforge-nano", 0),
        ] {
            let p = ALL.iter().find(|p| p.name == name).unwrap();
            assert_eq!(p.job_interval_ms, ms, "{name}");
        }
    }
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

    /// The Naja Duo since 2026-11-13 asks for no count of its own: the
    /// chip's 6860 is not a mismatch (the firmware warned "the board's
    /// copy is missing" at every chain start).
    #[test]
    fn a_board_without_a_core_count_of_its_own_fits_every_chip_profile() {
        use super::*;
        assert_eq!(BITAXE_NAJA_DUO.small_cores, 0);
        assert!(!BITAXE_NAJA_DUO.small_cores_differ(6860));
        assert!(!BITAXE_NAJA_DUO.small_cores_differ(6725));
        let own = Profile {
            small_cores: 6725,
            ..BITAXE_NAJA_DUO
        };
        assert!(!own.small_cores_differ(6725));
        assert!(own.small_cores_differ(6860));
    }

    #[test]
    fn a_two_domain_board_keeps_its_vout_above_two_volts() {
        // Naja Duo at 1010 mV: VOUT 2.02 V must still count.
        let v = BITAXE_NAJA_DUO.vout_for_core_mv(1010);
        assert!(v > 2.0 && v <= BITAXE_NAJA_DUO.vout_plausible_max());
        assert_eq!(BITAXE_NAJA_DUO.vout_plausible_max(), 4.0);
        assert_eq!(BITAXE_GAMMA_HEX.vout_plausible_max(), 4.0);
        assert_eq!(BITAXE_GAMMA_TURBO.vout_plausible_max(), 2.0);
        assert_eq!(BITAXE_GAMMA.vout_plausible_max(), 2.0);
        for p in ALL {
            assert!(
                p.vout_for_core_mv(1500) <= p.vout_plausible_max(),
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn the_four_axeos_boards_carry_their_family_table() {
        for p in [&BITAXE_GAMMA_DUO, &BITAXE_GAMMA_TURBO] {
            assert_eq!((p.uart_tx, p.uart_rx, p.asic_reset), (17, 18, Some(1)));
            assert_eq!(p.i2c, BITAXE_GAMMA.i2c, "{}", p.name);
            assert_eq!(p.display, Display::Ssd1306 { addr: 0x3C });
            assert!(p.asic_enable.is_none());
        }
        for p in [&BITAXE_NAJA_DUO, &BITAXE_GAMMA_HEX] {
            assert_eq!(p.i2c, Some(NERD_I2C), "{}", p.name);
            assert_eq!(p.display, Display::St7789I80);
            assert_eq!(p.asic_enable, COLOR_ENABLE);
            assert_eq!(p.voltage_domains, 2);
            assert!(p.emc2103().is_some());
            let vr = p.tps546().unwrap();
            let v = p.vout_for_core_mv(p.default_core_mv);
            assert!(v >= vr.vout_min && v <= vr.vout_max, "{} {v}", p.name);
        }
        assert_eq!(BITAXE_GAMMA_DUO.default_frequency_mhz, 400);
        // Free clock values as on the Gamma and in AxeOS (a Turbo runs
        // 850 MHz under AXP90 coolers); the voltage stays capped.
        for p in [&BITAXE_GAMMA_DUO, &BITAXE_GAMMA_TURBO] {
            let c = p.choices(false);
            assert_eq!(c.abs_max_frequency_mhz, 0, "{}", p.name);
            assert_eq!(c.abs_max_core_mv, 1250, "{}", p.name);
            assert!(!c.frequency_options.is_empty(), "{}", p.name);
        }
        assert_eq!(BITAXE_GAMMA_DUO.tps546(), Some(tps546::Config::BITAXE_5V));
        assert_eq!(BITAXE_GAMMA_TURBO.tps546().unwrap().stack_config, 0x0001);
        assert_eq!(BITAXE_GAMMA_HEX.tps546().unwrap().stack_config, 0x0003);
        assert_eq!(BITAXE_GAMMA_HEX.asic_count, 6);
        assert_eq!(BITAXE_NAJA_DUO.asic, Asic::Bm1373);
        for p in [
            &BITAXE_GAMMA_DUO,
            &BITAXE_GAMMA_TURBO,
            &BITAXE_NAJA_DUO,
            &BITAXE_GAMMA_HEX,
        ] {
            assert!(
                p.frequency_options.contains(&p.default_frequency_mhz),
                "{}",
                p.name
            );
            assert!(p.voltage_options.contains(&p.default_core_mv), "{}", p.name);
        }
    }

    #[test]
    fn nano_has_the_bitaxe_pins_two_muxed_sensors_and_an_input_monitor() {
        let p = &BITFORGE_NANO;
        assert_eq!((p.uart_tx, p.uart_rx, p.asic_reset), (17, 18, Some(1)));
        assert_eq!(p.i2c, BITAXE_GAMMA.i2c);
        assert_eq!(p.asic_count, 2);
        assert!(p.tps546().is_some());
        assert!(matches!(p.vr, Vr::Tps546Ina260 { ina260: 0x40, .. }));
        assert!(matches!(
            p.thermal,
            Thermal::DualEmc2101Mux {
                mux: 0x70,
                channels: [2, 3],
                ..
            }
        ));
        assert!(p.emc2101().is_none(), "the sensors sit behind the mux");
        // The maker's diode tuning on the Nano only; every other EMC2101
        // board keeps ESP-Miner's 0x24.
        let Thermal::DualEmc2101Mux { config, .. } = p.thermal else {
            panic!("mux");
        };
        assert_eq!(config, emc2101::Config::BITFORGE_NANO_DIODE);
        assert_eq!(config.ideality, Some(0x37));
        for q in ALL {
            if let Some(c) = q.emc2101() {
                assert_eq!(c.ideality, Some(0x24), "{}", q.name);
            }
        }
        assert_eq!(p.display, Display::None);
        assert_eq!(p.power_offset_w, 0.0, "the INA260 measures the input");
        assert!(p.frequency_options.contains(&p.default_frequency_mhz));
        assert!(p.voltage_options.contains(&p.default_core_mv));
        let v = p.vout_for_core_mv(p.default_core_mv);
        let vr = p.tps546().unwrap();
        assert!(v >= vr.vout_min && v <= vr.vout_max);
        assert!(!p.display_flip);
    }

    /// v0.1.36: the Nano's fan target follows its diode scale, and a
    /// stored old default is moved once; anything else stays.
    #[test]
    fn the_nano_fan_target_moved_with_its_diode_scale() {
        assert_eq!(
            BITFORGE_NANO.fan_target_c, 52.0,
            "60 on the 0x24 scale, 8.2 °C lower on 0x37"
        );
        assert_eq!(NANO_FAN_TARGET_BEFORE_V0136, 60);
        // Every other board keeps AxeOS's 60 or NerdOS's own value.
        for p in ALL {
            if p.name != BITFORGE_NANO.name {
                assert_ne!(p.fan_target_c, 52.0, "{}", p.name);
            }
        }
        // The stored old default moves to "the profile's"; an owner's
        // value and the unset 0 stay; other boards never move.
        assert_eq!(migrated_fan_target(&BITFORGE_NANO, 60), Some(0));
        assert_eq!(migrated_fan_target(&BITFORGE_NANO, 0), None);
        assert_eq!(migrated_fan_target(&BITFORGE_NANO, 55), None);
        assert_eq!(migrated_fan_target(&BITFORGE_NANO, 52), None);
        assert_eq!(migrated_fan_target(&BITFORGE_NANO, 61), None);
        for p in ALL {
            if p.name != BITFORGE_NANO.name {
                assert_eq!(migrated_fan_target(p, 60), None, "{}", p.name);
            }
        }
    }

    #[test]
    fn gaia_is_capped_where_the_regulator_gave_up() {
        let p = &NERDAXE_GAIA;
        assert_eq!(p.asic, Asic::Bm1373);
        assert_eq!(p.asic_count, 1);
        assert_eq!(p.nominal_voltage_v, 12);
        assert_eq!(p.abs_max_frequency_mhz, 420);
        assert_eq!(p.abs_max_core_mv, 970);
        assert!(p.frequency_options.iter().all(|f| *f <= 420));
        assert!(p.voltage_options.iter().all(|v| *v <= 970));
        assert!(p.frequency_options.contains(&p.default_frequency_mhz));
        assert!(p.voltage_options.contains(&p.default_core_mv));
        // The bring-up voltage is NerdOS's, above the running cap on purpose.
        assert_eq!(p.init_core_mv, 1000);
        assert!(matches!(
            p.vr,
            Vr::Tps546Nerd {
                enable: None,
                ldo_enable: Some(12),
                ..
            }
        ));
        assert!(p.tps546().is_some());
        assert!(p.emc2101().is_some());
        let c = p.choices(false);
        assert_eq!(c.frequency_options, p.frequency_options);
        assert_eq!(c.abs_max_core_mv, 970);
        // Every Nerd carrier draws rotated 180° as its base; the Bitaxe
        // and the bare devkit do not.
        for q in ALL {
            let nerd = q.i2c == Some(NERD_I2C) && !q.name.starts_with("bitaxe-");
            let hex = q.name == BITAXE_GAMMA_HEX.name;
            assert_eq!(q.display_flip, nerd || hex, "{}", q.name);
        }
    }

    #[test]
    fn nerdnos_is_a_t_display_carrier_with_one_bm1397_and_a_thermistor() {
        let p = &NERDNOS;
        assert_eq!(p.asic, Asic::Bm1397);
        assert_eq!(p.asic_count, 1);
        assert_eq!((p.uart_tx, p.uart_rx), (17, 18));
        assert_eq!(p.asic_reset, Some(13), "the stock firmware's PEN");
        assert_eq!(p.stock_reset_pulse, Some(12), "the stock firmware's RST");
        assert_eq!((p.reset_low_ms, p.reset_release_ms), (100, 100));
        assert_eq!(p.vr, Vr::None);
        assert!(p.tps546().is_none() && p.tps53647().is_none());
        assert_eq!(p.regulator_min_core_mv(), 0);
        assert_eq!(p.thermal, Thermal::Ntc(ntc::Config::NERDNOS));
        assert_eq!(ntc::Config::NERDNOS.vcore_gpio, Some(3));
        assert_eq!(p.display, Display::St7789I80);
        assert_eq!(p.i2c, None);
        assert_eq!((p.default_frequency_mhz, p.default_core_mv), (300, 1400));
        assert_eq!(
            p.reported_core_mv(1150),
            1400,
            "the fixed rail, not the stored default"
        );
        assert_eq!(
            BITAXE_GAMMA.reported_core_mv(1150),
            1150,
            "a regulator: the setting"
        );
        assert_eq!(p.chip_stop_c, 80.0);
        assert_eq!(p.abs_max_frequency_mhz, 400);
        assert!(p.voltage_options.is_empty());
        assert!(p.frequency_options.contains(&300));
        assert_eq!(p.asic_enable, None);
        assert!(!p.has_fan());
        assert!(BITAXE_GAMMA.has_fan());
        assert!(!T_DISPLAY_S3_DEVKIT.has_fan());
        let e = p.power_estimate.unwrap();
        assert!(
            (e.watts(300_000_000) - 8.01).abs() < 0.02,
            "{}",
            e.watts(300_000_000)
        );
        assert!((e.watts(0) - 1.5).abs() < 0.01);
        assert!(ALL
            .iter()
            .filter(|q| q.name != "nerdnos")
            .all(|q| q.power_estimate.is_none()));
        assert_eq!(Profile::by_name("nerdnos"), Some(p));
        // The other boards keep their reset lines as they were.
        for q in ALL {
            if q.name != "nerdnos" {
                assert_eq!(q.stock_reset_pulse, None, "{}", q.name);
            }
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
    fn regulator_floor_is_the_clean_range_per_voltage_domain() {
        for p in ALL {
            let want = match p.vr {
                // The VID floor of the TPS5364x driver.
                Vr::Tps5364x { .. } => 800,
                // 1 V on the 0.25 divider, 2 V across two domains on
                // 0.125; the Gaia's own 0.8 V floor on the 0.5 divider.
                Vr::Tps546(_) | Vr::Tps546Nerd { .. } | Vr::Tps546Ina260 { .. } => {
                    if p.name == NERDAXE_GAIA.name {
                        800
                    } else {
                        1000
                    }
                }
                _ => 0,
            };
            assert_eq!(p.regulator_min_core_mv(), want, "{}", p.name);
            // The floor is one the driver takes, and the factory point
            // sits on or above it.
            if want > 0 {
                let vr_floor = match p.vr {
                    Vr::Tps5364x { .. } => f32::from(tps53647::VOUT_MIN_MV) / 1000.0,
                    _ => p.tps546().unwrap().min_vout(),
                };
                assert!(p.vout_for_core_mv(want) >= vr_floor - 1e-6, "{}", p.name);
                assert!(p.default_core_mv >= want, "{}", p.name);
            }
        }
    }

    #[test]
    fn tps546_defaults_lie_inside_the_regulator_window() {
        for p in [
            &BITAXE_GAMMA,
            &NERDAXE_GAMMA,
            &NERDAXE_GAIA,
            &NERDQAXE_PLUSPLUS_REV7,
        ] {
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
            assert_eq!((p.chip_stop_c, p.vr_stop_c), (70.0, 70.0), "{}", p.name);
        }
        assert_eq!(NERDOCTAXE_GAMMA.tps53647().unwrap().vr_temp_offset_c, 8);
        assert_eq!(Q1373.temp_offset_c, 3.0);
        assert_eq!(NERDQAXE_PLUSPLUS.temp_offset_c, 10.0, "NerdOS +10 °C");
        assert_eq!(
            NERDOCTAXE_GAMMA.temp_offset_c, 10.0,
            "inherited from NerdQaxePlus2"
        );
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
        // Rev 7: the Nerd carrier with a TPS546 and two voltage domains.
        assert_eq!(NERDQAXE_PLUSPLUS_REV7.vout_for_core_mv(1200), 2.4);
        assert_eq!(NERDQAXE_PLUSPLUS_REV7.core_mv_for_vout(2.4), 1200);
        assert!(NERDQAXE_PLUSPLUS_REV7.tps53647().is_none());
        assert_eq!(NERDQAXE_PLUSPLUS_REV7.i2c, Some(NERD_I2C));
        assert_eq!(NERDQAXE_PLUSPLUS_REV7.thermal, Thermal::Tmp1075Emc2302);
        assert_eq!(NERDOCTAXE_GAMMA.tps53647().unwrap().phases, 6);
        assert_eq!(Q1373.tps53647().unwrap().phases, 4);
        // Only the Q1373 lets the device code overrule the strap (NerdOS).
        let by_code = |p: &Profile| match p.vr {
            Vr::Tps5364x { alt: Some(a), .. } => a.part_by_code,
            _ => false,
        };
        assert!(by_code(&Q1373));
        assert!(!by_code(&NERDOCTAXE_GAMMA));
        assert!(NERDAXE_GAMMA.emc2101().is_some());
        assert_eq!(Q1373.asic_reset, None, "reset sits on the expander");
        assert_eq!(Q1373.asic, Asic::Bm1373);
        assert_eq!(NERDAXE_GAIA.asic, Asic::Bm1373);
        assert_eq!(NERDNOS.asic, Asic::Bm1397);
        assert_eq!(NERDQAXE_PLUS.asic, Asic::Bm1368);
        assert_eq!(NERDQAXE_PLUS.tps53647().unwrap().phases, 2);
        assert_eq!(NERDQAXE_PLUS.thermal, Thermal::Tmp1075Emc2302);
        assert!(ALL
            .iter()
            .filter(|p| p.asic != Asic::Bm1373 && p.asic != Asic::Bm1397 && p.asic != Asic::Bm1368)
            .all(|p| p.asic == Asic::Bm1370));
        assert!(ALL
            .iter()
            .filter(|p| p.asic == Asic::Bm1368)
            .all(|p| p.name == "nerdqaxe-plus"));
    }

    #[test]
    fn octaxe_choices_follow_the_fitted_regulator() {
        let six = NERDOCTAXE_GAMMA.choices(true);
        assert_eq!(six.frequency_options.first(), Some(&525));
        assert_eq!(six.frequency_options.last(), Some(&800));
        assert!(six
            .frequency_options
            .contains(&NERDOCTAXE_GAMMA.default_frequency_mhz));
        assert!(six
            .voltage_options
            .contains(&NERDOCTAXE_GAMMA.default_core_mv));
        assert_eq!(six.abs_max_frequency_mhz, 850);
        assert_eq!((six.abs_min_core_mv, six.abs_max_core_mv), (1050, 1400));
        assert_eq!(
            NERDOCTAXE_GAMMA.choices(false),
            NERDQAXE_PLUSPLUS.choices(false)
        );
        assert_eq!(BITAXE_GAMMA.choices(true), BITAXE_GAMMA.choices(false));
        assert!(BITAXE_GAMMA.choices(true).frequency_options.is_empty());
    }

    #[test]
    fn the_choose_state_keeps_the_candidates_display() {
        // NerdQAxe++ or NerdOctaxe: both on the T-Display-S3, LCD flipped.
        let p = undetected_for(&[&NERDQAXE_PLUSPLUS, &NERDOCTAXE_GAMMA]);
        assert_eq!(p.name, "undetected");
        assert_eq!(p.display, Display::St7789I80);
        assert!(p.display_flip);
        assert_eq!(p.i2c, None, "the LCD needs no I²C");
        assert_eq!(p.asic_count, 0);
        let p = undetected_for(&[&Q1370, &Q1373]);
        assert_eq!(p.display, Display::St7789I80);
        // A Bitaxe carrier: the OLED on its I²C pins.
        let p = undetected_for(&[&BITAXE_GAMMA_TURBO]);
        assert_eq!(p.display, BITAXE_GAMMA.display);
        assert_eq!(p.i2c, BITAXE_GAMMA.i2c);
        // No display, or candidates that disagree: none.
        assert_eq!(undetected_for(&[&BITFORGE_NANO]).display, Display::None);
        assert_eq!(
            undetected_for(&[&BITAXE_GAMMA, &NERDAXE_GAMMA]).display,
            Display::None
        );
        assert_eq!(undetected_for(&[]).display, Display::None);
    }

    #[test]
    fn only_the_nerd_lcd_boards_have_a_page_key() {
        assert!(NERDQAXE_PLUSPLUS.has_page_key());
        assert!(!BITAXE_NAJA_DUO.has_page_key());
        assert!(!BITAXE_GAMMA.has_page_key());
        assert!(NERDQAXE_PLUSPLUS.backlight_aw9364());
        assert!(!BITAXE_NAJA_DUO.backlight_aw9364());
        assert!(!BITAXE_GAMMA_HEX.backlight_aw9364());
        assert!(T_DISPLAY_S3_DEVKIT.backlight_aw9364());
    }

    #[test]
    fn a_stored_chip_stop_above_the_ceiling_is_brought_down() {
        let gamma = Profile::by_name("bitaxe-gamma").unwrap();
        assert_eq!(gamma.chip_stop_max_c(), 75.0);
        assert_eq!(gamma.stored_chip_stop_fix(90), Some(75));
        assert_eq!(gamma.stored_chip_stop_fix(75), None);
        assert_eq!(gamma.stored_chip_stop_fix(70), None);
        assert_eq!(gamma.stored_chip_stop_fix(0), None, "0 = the default");
        let qx = Profile::by_name("nerdqaxe-plusplus").unwrap();
        assert_eq!(qx.stored_chip_stop_fix(90), None);
    }

    /// Every GPIO a profile drives or reads itself, for the HAT collision
    /// check: the chain UART and reset, the I²C pair, the regulator's
    /// enable and LDO lines on the ESP, the regulator-detect pin, the chip
    /// power enable and the NerdNOS's stock reset pulse.
    fn own_gpios(p: &Profile) -> Vec<u8> {
        let mut v = vec![p.uart_tx, p.uart_rx];
        v.extend(p.asic_reset);
        if let Some(i2c) = p.i2c {
            v.extend([i2c.sda, i2c.scl]);
        }
        match p.vr {
            Vr::Tps5364x {
                enable,
                ldo_enable,
                via_expander: false,
                alt,
                ..
            } => {
                v.push(enable);
                v.extend(ldo_enable);
                v.extend(alt.map(|a| a.pin));
            }
            Vr::Tps546Nerd {
                enable, ldo_enable, ..
            } => {
                v.extend(enable);
                v.extend(ldo_enable);
            }
            _ => {}
        }
        v.extend(p.asic_enable.map(|a| a.pin));
        v.extend(p.stock_reset_pulse);
        v
    }

    #[test]
    fn the_nerdhat_sits_on_five_boards_and_on_nerdos_pins() {
        // NerdOS v1.1.1: `kHatPins` (nerdqaxeplus.cpp) and `kEthPins`
        // (nerdaxegaia.cpp), both without RST and IRQ.
        assert_eq!(
            NERDQAXE_PLUSPLUS.ethernet,
            Some(EthPins {
                sclk: 2,
                mosi: 12,
                miso: 16,
                cs: 21,
                rst: None,
                int: None
            })
        );
        assert_eq!(NERDQAXE_PLUSPLUS_REV7.ethernet, Some(NERDHAT_QAXE));
        assert_eq!(NERDOCTAXE_GAMMA.ethernet, Some(NERDHAT_QAXE));
        assert_eq!(
            NERDAXE_GAIA.ethernet,
            Some(EthPins {
                sclk: 10,
                mosi: 3,
                miso: 16,
                cs: 21,
                rst: None,
                int: None
            })
        );
        let with_hat = [
            "nerdqaxe-plus",
            "nerdqaxe-plusplus",
            "nerdqaxe-plusplus-rev7",
            "nerdoctaxe-gamma",
            "nerdaxe-gaia",
        ];
        for p in ALL {
            assert_eq!(
                p.ethernet.is_some(),
                with_hat.contains(&p.name),
                "{}",
                p.name
            );
        }
        for u in [
            &UNDETECTED,
            &UNDETECTED_LCD,
            &UNDETECTED_LCD_FLIPPED,
            &UNDETECTED_OLED_BITAXE,
        ] {
            assert!(u.ethernet.is_none(), "{}", u.name);
        }
    }

    #[test]
    fn the_nerdhat_pins_touch_nothing_the_board_drives() {
        for p in ALL {
            let Some(e) = p.ethernet else { continue };
            let own = own_gpios(p);
            for g in e.gpios().into_iter().flatten() {
                assert!(!own.contains(&g), "{}: GPIO{g} is used twice", p.name);
            }
            // No reset or interrupt line on any board (an RC network on
            // the HAT, and the chip is polled).
            assert_eq!((e.rst, e.int), (None, None), "{}", p.name);
        }
        // The Gaia's danger pins (NerdOS nerdaxegaia.cpp: /VDD, LDO_EN,
        // PMB_ALRT, PGOOD) are not among its HAT pins; its LDO enable is
        // one of them and shows up in the own-pin list above.
        let gaia = NERDAXE_GAIA.ethernet.unwrap();
        for g in [2, 12, 13, 11] {
            assert!(!gaia.gpios().contains(&Some(g)), "Gaia GPIO{g}");
        }
        assert!(own_gpios(&NERDAXE_GAIA).contains(&12));
    }

    #[test]
    fn a_tmp451_mux_blocks_the_hat_on_the_qaxe_pins_only() {
        // The NerdOctaxe from rev 3.4: GPIO2/12 are the mux selects.
        assert_eq!(NERDOCTAXE_GAMMA.ethernet_pins(false), Some(NERDHAT_QAXE));
        assert_eq!(NERDOCTAXE_GAMMA.ethernet_pins(true), None);
        assert_eq!(NERDQAXE_PLUSPLUS.ethernet_pins(true), None);
        // The Gaia's pins are others.
        assert_eq!(NERDAXE_GAIA.ethernet_pins(true), Some(NERDHAT_GAIA));
        // A board without the header never gets pins.
        assert_eq!(BITAXE_GAMMA.ethernet_pins(false), None);
        assert_eq!(Q1373.ethernet_pins(false), None);
    }
}

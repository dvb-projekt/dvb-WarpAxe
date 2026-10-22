//! TPS53647 / TPS53667: the multi-phase core-voltage regulator on the
//! Nerd multi-chip boards (NerdQAxe+/++, NerdOctaxe-γ, Q1370, Q1373),
//! PMBus over I²C at 0x71. The register sequence follows NerdOS's
//! `TPS53647` and `TPS53667` classes (commit 762092d), so a board that
//! runs under NerdOS runs under the same limits here.
//!
//! This driver is configuration, readback and the output VID only. The
//! part is switched on and off through its EN pin (an ESP GPIO or an
//! FXL6408 expander pin, see the board profile), not through OPERATION,
//! so enabling lives in the firmware: NerdOS pulls EN low for "0 V" and
//! high before the first `set_vout`.
//!
//! VOUT_COMMAND is a VR12 VID, not LINEAR16: 5 mV per step from 0.25 V
//! (VID 1 = 0.25 V, VID 0 = output off). The output is read back from
//! MFR_SPECIFIC_04 as ULINEAR16 with exponent −9; everything else
//! (currents, powers, temperature) is LINEAR11.

use crate::i2c::{Error, I2c};
use crate::pmbus::{self as pm, f32_to_linear11};

/// I²C address of the TPS53647 and TPS53667.
pub const ADDR: u8 = 0x71;
/// What MFR_SPECIFIC_44 answers on a TPS53647 (NerdOS `TPS53647.h`).
pub const DEVICE_CODE: u16 = 0x01F0;
/// What MFR_SPECIFIC_44 answers on a TPS53667 (NerdOS `TPS53667.h`;
/// read back as 0x01F8 on a NerdOCTAXE-γ Rev 3.1, 2026-09-28).
pub const DEVICE_CODE_667: u16 = 0x01F8;

// Manufacturer-specific commands (TPS53647/TPS53667 datasheets, NerdOS
// `pmbus_commands.h`).
/// Per-phase over-current limit.
const MFR_00_PER_PHASE_OC: u8 = 0xD0;
/// Output voltage readback, ULINEAR16 with exponent −9.
const MFR_04_VOUT: u8 = 0xD4;
/// IMAX: full-scale output current, A.
const MFR_10_IMAX: u8 = 0xDA;
/// Switching frequency.
const MFR_12_FREQUENCY: u8 = 0xDC;
/// Operating mode: VID table, phase shedding, slew rate.
const MFR_13_MODE: u8 = 0xDD;
/// VIN under-voltage lockout threshold.
const MFR_16_VIN_UVLO: u8 = 0xE0;
/// Written as a word by NerdOS on the TPS53667, purpose not commented.
const MFR_19: u8 = 0xE3;
/// Number of phases minus one.
const MFR_20_PHASES: u8 = 0xE4;
/// Per-phase enable mask, 0 = all phases on (TPS53667).
const MFR_24_PHASE_ENABLE: u8 = 0xE8;
/// Device code, [`DEVICE_CODE`].
const MFR_44_DEVICE_CODE: u8 = 0xFC;
/// The same command for a bare probe (the first-start detection reads
/// it without the driver, before any board is chosen).
pub const DEVICE_CODE_CMD: u8 = MFR_44_DEVICE_CODE;

/// ON_OFF_CONFIG as NerdOS writes it (0b0001_0111): the EN pin
/// controls the output, active high, with PMBus OPERATION ignored.
const ON_OFF_CONFIG: u8 = 0b0001_0111;
/// MFR_12: 500 kHz switching (the chip default).
const FREQUENCY_500KHZ: u8 = 0x20;
/// MFR_13: VR12 mode, dynamic phase shedding, 0.68 mV/µs slew. NerdOS
/// writes the same byte on every board; its TPS53667 comment calls it
/// "phase shedding disabled", the TPS53647 comment "enabled" — the
/// byte is what matters.
const MODE_VR12: u8 = 0x89;
/// Over-temperature warning, °C.
const OT_WARN_C: f32 = 95.0;
/// Over-temperature fault, °C.
const OT_FAULT_C: f32 = 125.0;
// TPS53667 only.
/// MFR_00: per-phase over-current limit 45 A.
const PER_PHASE_OC_45A: u8 = 0x07;
/// MFR_16: VIN UVLO 6 V.
const VIN_UVLO_6V: u8 = 0x01;
/// MFR_19 value NerdOS writes.
const MFR_19_VALUE: u16 = 0x0003;
/// Input over-current warning, A (about 300 W at 12 V).
const IIN_OC_WARN_A: f32 = 25.0;
/// Input over-current fault, A (about 336 W).
const IIN_OC_FAULT_A: f32 = 28.0;

/// VID step, mV.
const VID_STEP_MV: u32 = 5;
/// Voltage of VID 1, mV.
const VID_BASE_MV: u32 = 250;
/// The factory-default VID (1.000 V). Never written: NerdOS keeps it
/// as the tell-tale that the regulator has reset itself, and writes
/// 0x96 (0.995 V) instead.
const VID_FACTORY_1V: u8 = 0x97;
/// Lowest output the driver commands, mV (NerdOS `m_initVOutMin`).
pub const VOUT_MIN_MV: u16 = 800;
/// Highest output the driver commands, mV (NerdOS `m_initVOutMax`).
pub const VOUT_MAX_MV: u16 = 1400;

/// Which part the board carries; the init sequence differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// TPS53647, up to four phases.
    Tps53647,
    /// TPS53667, up to six phases; NerdOS also writes the phase-enable
    /// mask, VOUT_MAX, per-phase OC, UVLO and the input-current limits.
    Tps53667,
}

impl Variant {
    /// The code MFR_SPECIFIC_44 answers on this part.
    pub fn device_code(self) -> u16 {
        match self {
            Variant::Tps53647 => DEVICE_CODE,
            Variant::Tps53667 => DEVICE_CODE_667,
        }
    }
}

/// What init writes: the part, its phases and the current limits, as
/// NerdOS's board classes pass them to `init(num_phases, imax, ifault)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The part on the board.
    pub variant: Variant,
    /// Phases populated, 1–6.
    pub phases: u8,
    /// MFR_10 full-scale current, A.
    pub imax: u8,
    /// IOUT over-current warning and fault (both the same), A.
    pub ifault: u16,
    /// Added to the regulator-side TMP1075: NerdOS adds 8 °C for the
    /// Octaxe's six-phase part to correct the sensor's placement.
    pub vr_temp_offset_c: i8,
}

impl Config {
    /// NerdQAxe++: TPS53647, 3 phases, 90 A, fault 95 A.
    pub const NERDQAXE_PLUSPLUS: Config = Config {
        variant: Variant::Tps53647,
        phases: 3,
        imax: 90,
        ifault: 95,
        vr_temp_offset_c: 0,
    };
    /// NerdQAxe+: TPS53647, 2 phases, 60 A, fault 55 A.
    pub const NERDQAXE_PLUS: Config = Config {
        variant: Variant::Tps53647,
        phases: 2,
        imax: 60,
        ifault: 55,
        vr_temp_offset_c: 0,
    };
    /// NerdOctaxe-γ with the six-phase part (VR_DETECT GPIO3 high).
    pub const NERDOCTAXE_GAMMA_6P: Config = Config {
        variant: Variant::Tps53667,
        phases: 6,
        imax: 240,
        ifault: 235,
        vr_temp_offset_c: 8,
    };
    /// NerdOctaxe-γ with the four-phase part (VR_DETECT low).
    pub const NERDOCTAXE_GAMMA_4P: Config = Config {
        variant: Variant::Tps53647,
        phases: 4,
        imax: 180,
        ifault: 160,
        vr_temp_offset_c: 0,
    };
    /// Q1370: TPS53647, 4 phases, 123 A, fault 160 A (fault above
    /// IMAX as NerdOS has it).
    pub const Q1370: Config = Config {
        variant: Variant::Tps53647,
        phases: 4,
        imax: 123,
        ifault: 160,
        vr_temp_offset_c: 0,
    };
    /// Q1373 with the four-phase part (expander pin 6 high): IMAX from
    /// the 48.7 kΩ Rmon formula = 122 A, fault 160 A.
    pub const Q1373_4P: Config = Config {
        variant: Variant::Tps53647,
        phases: 4,
        imax: 122,
        ifault: 160,
        vr_temp_offset_c: 0,
    };
    /// Q1373 with the six-phase part (expander pin 6 low): IMAX from
    /// the 33.2 kΩ Rmon formula = 179 A, fault 6 × 40 A.
    pub const Q1373_6P: Config = Config {
        variant: Variant::Tps53667,
        phases: 6,
        imax: 179,
        ifault: 240,
        vr_temp_offset_c: 0,
    };
}

/// The population a board with either part really carries, from the
/// device code. NerdOS `q1373.cpp` takes the part from MFR_SPECIFIC_44
/// and only the phase count from the strap, because a TPS53667 may be
/// populated with four phases. `by_strap` is what the strap alone
/// selects, `other` the alternative population. Returns the population
/// to init with and a note when it differs from the strap's reading; an
/// unknown code keeps `by_strap` (init then refuses it as WrongDevice).
pub fn resolve_by_code(
    code: u16,
    by_strap: Config,
    other: Config,
) -> (Config, Option<&'static str>) {
    if code == by_strap.variant.device_code() {
        return (by_strap, None);
    }
    if code != other.variant.device_code() {
        return (by_strap, Some("device code matches neither part"));
    }
    match other.variant {
        // A TPS53667 where the strap said TPS53647: the strap's phases,
        // the 667's IMAX (its Rmon), IFAULT per phase as NerdOS scales it.
        Variant::Tps53667 => {
            let phases = by_strap.phases;
            let ifault = other.ifault * u16::from(phases) / u16::from(other.phases.max(1));
            (
                Config {
                    variant: Variant::Tps53667,
                    phases,
                    imax: other.imax,
                    ifault,
                    ..other
                },
                Some("TPS53667 populated with the strap's phase count"),
            )
        }
        // A TPS53647 cannot run six phases: the four-phase population.
        Variant::Tps53647 => (
            other,
            Some("six-phase strap, but the part is a TPS53647: four phases"),
        ),
    }
}

/// Volts to the VID VOUT_COMMAND takes, rounded to the 5 mV step;
/// 0 for 0 V (output off). 1.000 V gives 0x96, not the factory 0x97.
pub fn volts_to_vid(volts: f32) -> u8 {
    if volts <= 0.0 {
        return 0;
    }
    let mv = (volts * 1000.0 + 0.5) as u32;
    let vid = (mv.saturating_sub(VID_BASE_MV) + VID_STEP_MV / 2) / VID_STEP_MV + 1;
    let vid = vid.min(0xFF) as u8;
    if vid == VID_FACTORY_1V {
        VID_FACTORY_1V - 1
    } else {
        vid
    }
}

/// The VID's voltage; 0 for VID 0.
pub fn vid_to_volts(vid: u8) -> f32 {
    if vid == 0 {
        return 0.0;
    }
    (u32::from(vid - 1) * VID_STEP_MV + VID_BASE_MV) as f32 / 1000.0
}

/// The driver, owning the bus.
pub struct Tps53647<I> {
    bus: I,
    cfg: Config,
    /// The last PMBus command sent, for a failure report.
    last_cmd: u8,
}

impl<I: I2c> Tps53647<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, cfg: Config) -> Self {
        Tps53647 {
            bus,
            cfg,
            last_cmd: 0,
        }
    }

    /// Give the bus back.
    pub fn release(self) -> I {
        self.bus
    }

    /// The configuration this driver was built with.
    pub fn config(&self) -> Config {
        self.cfg
    }

    /// The PMBus command code of the last transaction: after an error,
    /// the one that failed.
    pub fn last_command(&self) -> u8 {
        self.last_cmd
    }

    fn read_byte(&mut self, cmd: u8) -> Result<u8, Error<I::Error>> {
        self.last_cmd = cmd;
        let mut b = [0u8; 1];
        self.bus.write_read(ADDR, &[cmd], &mut b)?;
        Ok(b[0])
    }

    fn read_word(&mut self, cmd: u8) -> Result<u16, Error<I::Error>> {
        self.last_cmd = cmd;
        let mut b = [0u8; 2];
        self.bus.write_read(ADDR, &[cmd], &mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    fn write_command(&mut self, cmd: u8) -> Result<(), Error<I::Error>> {
        self.last_cmd = cmd;
        Ok(self.bus.write(ADDR, &[cmd])?)
    }

    fn write_byte(&mut self, cmd: u8, v: u8) -> Result<(), Error<I::Error>> {
        self.last_cmd = cmd;
        Ok(self.bus.write(ADDR, &[cmd, v])?)
    }

    fn write_word(&mut self, cmd: u8, v: u16) -> Result<(), Error<I::Error>> {
        self.last_cmd = cmd;
        let [lo, hi] = v.to_le_bytes();
        Ok(self.bus.write(ADDR, &[cmd, lo, hi])?)
    }

    fn write_linear11(&mut self, cmd: u8, v: f32) -> Result<(), Error<I::Error>> {
        self.write_word(cmd, f32_to_linear11(v))
    }

    /// Raw word read, for diagnostics.
    pub fn read_register(&mut self, cmd: u8) -> Result<u16, Error<I::Error>> {
        self.read_word(cmd)
    }

    /// Raw word write, for diagnostics.
    pub fn write_register(&mut self, cmd: u8, v: u16) -> Result<(), Error<I::Error>> {
        self.write_word(cmd, v)
    }

    /// MFR_SPECIFIC_44, [`DEVICE_CODE`] on a present part.
    pub fn device_code(&mut self) -> Result<u16, Error<I::Error>> {
        self.read_word(MFR_44_DEVICE_CODE)
    }

    /// Check the device code, restore the NVM defaults and write the
    /// operating configuration in NerdOS's order. The output stays as
    /// the EN pin has it; nothing here turns it on. Fails with
    /// `OutOfRange` for a phase count outside 1–6 before touching the
    /// part, `WrongDevice` when the code does not match.
    pub fn init(&mut self) -> Result<(), Error<I::Error>> {
        let c = self.cfg;
        if !(1..=6).contains(&c.phases) {
            return Err(Error::OutOfRange);
        }
        if self.device_code()? != c.variant.device_code() {
            return Err(Error::WrongDevice);
        }
        self.write_command(pm::CLEAR_FAULTS)?;
        self.write_command(pm::RESTORE_DEFAULT_ALL)?;
        self.write_byte(pm::ON_OFF_CONFIG, ON_OFF_CONFIG)?;
        self.write_byte(MFR_12_FREQUENCY, FREQUENCY_500KHZ)?;
        self.write_byte(MFR_10_IMAX, c.imax)?;
        self.write_byte(MFR_13_MODE, MODE_VR12)?;
        match c.variant {
            Variant::Tps53647 => {
                // NerdOS writes these two a second time; kept so the
                // bus trace matches.
                self.write_byte(pm::ON_OFF_CONFIG, ON_OFF_CONFIG)?;
                self.write_byte(MFR_12_FREQUENCY, FREQUENCY_500KHZ)?;
                self.write_byte(MFR_20_PHASES, c.phases - 1)?;
            }
            Variant::Tps53667 => {
                self.write_byte(MFR_20_PHASES, c.phases - 1)?;
                self.write_byte(MFR_24_PHASE_ENABLE, 0x00)?;
                let max = volts_to_vid(f32::from(VOUT_MAX_MV) / 1000.0);
                self.write_word(pm::VOUT_MAX, u16::from(max))?;
                self.write_byte(MFR_00_PER_PHASE_OC, PER_PHASE_OC_45A)?;
                self.write_byte(MFR_16_VIN_UVLO, VIN_UVLO_6V)?;
                self.write_word(MFR_19, MFR_19_VALUE)?;
            }
        }
        self.write_linear11(pm::OT_WARN_LIMIT, OT_WARN_C)?;
        self.write_linear11(pm::OT_FAULT_LIMIT, OT_FAULT_C)?;
        let ifault = f32::from(c.ifault);
        self.write_linear11(pm::IOUT_OC_WARN_LIMIT, ifault)?;
        self.write_linear11(pm::IOUT_OC_FAULT_LIMIT, ifault)?;
        if c.variant == Variant::Tps53667 {
            self.write_linear11(pm::IIN_OC_WARN_LIMIT, IIN_OC_WARN_A)?;
            self.write_linear11(pm::IIN_OC_FAULT_LIMIT, IIN_OC_FAULT_A)?;
        }
        Ok(())
    }

    /// Command the output voltage as a VID. Outside 0.8–1.4 V nothing
    /// is written; 0 V is not a VID write either (the EN pin does that),
    /// so it is refused too. Returns the VID written.
    pub fn set_vout(&mut self, volts: f32) -> Result<u8, Error<I::Error>> {
        let mv = (volts * 1000.0 + 0.5) as u32;
        if volts <= 0.0 || mv < u32::from(VOUT_MIN_MV) || mv > u32::from(VOUT_MAX_MV) {
            return Err(Error::OutOfRange);
        }
        let vid = volts_to_vid(volts);
        self.write_word(pm::VOUT_COMMAND, u16::from(vid))?;
        Ok(vid)
    }

    /// The VID VOUT_COMMAND holds; 0x97 means the part has reset to its
    /// factory default since the last `set_vout`.
    pub fn vout_vid(&mut self) -> Result<u8, Error<I::Error>> {
        Ok(self.read_word(pm::VOUT_COMMAND)? as u8)
    }

    /// Output voltage, V (MFR_SPECIFIC_04, 1 LSB = 1/512 V).
    pub fn vout(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::ulinear16_to_f32(self.read_word(MFR_04_VOUT)?, -9))
    }

    /// Input voltage, V.
    pub fn vin(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::linear11_to_f32(self.read_word(pm::READ_VIN)?))
    }

    /// Input current, A.
    pub fn iin(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::linear11_to_f32(self.read_word(pm::READ_IIN)?))
    }

    /// Output current, A.
    pub fn iout(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::linear11_to_f32(self.read_word(pm::READ_IOUT)?))
    }

    /// Input power, W.
    pub fn pin(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::linear11_to_f32(self.read_word(pm::READ_PIN)?))
    }

    /// Output power, W.
    pub fn pout(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::linear11_to_f32(self.read_word(pm::READ_POUT)?))
    }

    /// Controller temperature, °C.
    pub fn temperature_c(&mut self) -> Result<i32, Error<I::Error>> {
        Ok(pm::linear11_to_i32(self.read_word(pm::READ_TEMPERATURE_1)?))
    }

    /// STATUS_BYTE with bit 1 (CML) masked, the "weird coms error"
    /// NerdOS suppresses.
    pub fn status_byte(&mut self) -> Result<u8, Error<I::Error>> {
        Ok(self.read_byte(pm::STATUS_BYTE)? & !0x02)
    }

    /// STATUS_WORD with bit 1 (CML) masked, as [`status_byte`](Self::status_byte).
    pub fn status_word(&mut self) -> Result<u16, Error<I::Error>> {
        Ok(self.read_word(pm::STATUS_WORD)? & !0x0002)
    }

    /// One of the STATUS_* byte registers, unmasked, for a fault snapshot.
    pub fn status_register(&mut self, cmd: u8) -> Result<u8, Error<I::Error>> {
        self.read_byte(cmd)
    }

    /// The detail status registers, unmasked.
    pub fn status(&mut self) -> Result<Status, Error<I::Error>> {
        Ok(Status {
            byte: self.status_byte()?,
            word: self.status_word()?,
            vout: self.read_byte(pm::STATUS_VOUT)?,
            iout: self.read_byte(pm::STATUS_IOUT)?,
            input: self.read_byte(pm::STATUS_INPUT)?,
            temperature: self.read_byte(pm::STATUS_TEMPERATURE)?,
            mfr_specific: self.read_byte(pm::STATUS_MFR_SPECIFIC)?,
        })
    }

    /// Clear latched faults.
    pub fn clear_faults(&mut self) -> Result<(), Error<I::Error>> {
        self.write_command(pm::CLEAR_FAULTS)
    }
}

/// The status registers in one read; `byte` and `word` have CML masked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Status {
    /// STATUS_BYTE, CML masked.
    pub byte: u8,
    /// STATUS_WORD, CML masked.
    pub word: u16,
    /// STATUS_VOUT.
    pub vout: u8,
    /// STATUS_IOUT.
    pub iout: u8,
    /// STATUS_INPUT.
    pub input: u8,
    /// STATUS_TEMPERATURE.
    pub temperature: u8,
    /// STATUS_MFR_SPECIFIC.
    pub mfr_specific: u8,
}

impl Status {
    /// True when any register reports something.
    pub fn any(&self) -> bool {
        self.byte != 0
            || self.word != 0
            || self.vout != 0
            || self.iout != 0
            || self.input != 0
            || self.temperature != 0
            || self.mfr_specific != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    fn code_ok() -> Tx {
        Tx::WriteRead(ADDR, vec![MFR_44_DEVICE_CODE], vec![0xF0, 0x01])
    }
    fn code_667() -> Tx {
        Tx::WriteRead(ADDR, vec![MFR_44_DEVICE_CODE], vec![0xF8, 0x01])
    }
    fn cmd(c: u8) -> Tx {
        Tx::Write(ADDR, vec![c])
    }
    fn byte(c: u8, v: u8) -> Tx {
        Tx::Write(ADDR, vec![c, v])
    }
    fn word(c: u8, v: u16) -> Tx {
        let [lo, hi] = v.to_le_bytes();
        Tx::Write(ADDR, vec![c, lo, hi])
    }

    // LINEAR11 by hand, finest exponent: 95 = 760·2⁻³, 125 = 1000·2⁻³,
    // 160 = 640·2⁻², 235 = 940·2⁻², 240 = 960·2⁻², 25 = 800·2⁻⁵,
    // 28 = 896·2⁻⁵.
    const L11_95: u16 = 0xEAF8;
    const L11_125: u16 = 0xEBE8;
    const L11_160: u16 = 0xF280;
    const L11_235: u16 = 0xF3AC;
    const L11_240: u16 = 0xF3C0;
    const L11_25: u16 = 0xDB20;
    const L11_28: u16 = 0xDB80;

    #[test]
    fn linear11_constants_are_what_the_encoder_produces() {
        assert_eq!(f32_to_linear11(95.0), L11_95);
        assert_eq!(f32_to_linear11(125.0), L11_125);
        assert_eq!(f32_to_linear11(160.0), L11_160);
        assert_eq!(f32_to_linear11(235.0), L11_235);
        assert_eq!(f32_to_linear11(240.0), L11_240);
        assert_eq!(f32_to_linear11(25.0), L11_25);
        assert_eq!(f32_to_linear11(28.0), L11_28);
        for (w, v) in [
            (L11_95, 95.0),
            (L11_125, 125.0),
            (L11_160, 160.0),
            (L11_240, 240.0),
        ] {
            assert_eq!(pm::linear11_to_f32(w), v);
        }
    }

    #[test]
    fn init_tps53647_three_phase_writes_the_nerdos_sequence_exactly() {
        let script = vec![
            code_ok(),
            cmd(pm::CLEAR_FAULTS),
            cmd(pm::RESTORE_DEFAULT_ALL),
            byte(pm::ON_OFF_CONFIG, 0x17),
            byte(MFR_12_FREQUENCY, 0x20),
            byte(MFR_10_IMAX, 90),
            byte(MFR_13_MODE, 0x89),
            byte(pm::ON_OFF_CONFIG, 0x17),
            byte(MFR_12_FREQUENCY, 0x20),
            byte(MFR_20_PHASES, 2),
            word(pm::OT_WARN_LIMIT, L11_95),
            word(pm::OT_FAULT_LIMIT, L11_125),
            word(pm::IOUT_OC_WARN_LIMIT, L11_95),
            word(pm::IOUT_OC_FAULT_LIMIT, L11_95),
        ];
        let n = script.len();
        let mut vr = Tps53647::new(Bus::new(script), Config::NERDQAXE_PLUSPLUS);
        vr.init().unwrap();
        let bus = vr.release();
        assert!(bus.script.is_empty(), "sequence incomplete");
        assert_eq!(bus.log.len(), n, "nothing beyond the sequence");
    }

    #[test]
    fn init_tps53667_six_phase_adds_the_667_registers() {
        let script = vec![
            code_667(),
            cmd(pm::CLEAR_FAULTS),
            cmd(pm::RESTORE_DEFAULT_ALL),
            byte(pm::ON_OFF_CONFIG, 0x17),
            byte(MFR_12_FREQUENCY, 0x20),
            byte(MFR_10_IMAX, 240),
            byte(MFR_13_MODE, 0x89),
            byte(MFR_20_PHASES, 5),
            byte(MFR_24_PHASE_ENABLE, 0x00),
            // VOUT_MAX 1.4 V = VID 231 (0xE7); NerdOS's truncation would
            // give 0xE6 = 1.395 V.
            word(pm::VOUT_MAX, 0x00E7),
            byte(MFR_00_PER_PHASE_OC, 0x07),
            byte(MFR_16_VIN_UVLO, 0x01),
            word(MFR_19, 0x0003),
            word(pm::OT_WARN_LIMIT, L11_95),
            word(pm::OT_FAULT_LIMIT, L11_125),
            word(pm::IOUT_OC_WARN_LIMIT, L11_235),
            word(pm::IOUT_OC_FAULT_LIMIT, L11_235),
            word(pm::IIN_OC_WARN_LIMIT, L11_25),
            word(pm::IIN_OC_FAULT_LIMIT, L11_28),
        ];
        let n = script.len();
        let mut vr = Tps53647::new(Bus::new(script), Config::NERDOCTAXE_GAMMA_6P);
        vr.init().unwrap();
        let bus = vr.release();
        assert!(bus.script.is_empty(), "sequence incomplete");
        assert_eq!(bus.log.len(), n);
    }

    #[test]
    fn init_four_phase_647_writes_phases_three_and_the_160a_limits() {
        let mut vr = Tps53647::new(Bus::sink(), Config::Q1370);
        // A sink answers the device code with zeros: wrong device.
        assert_eq!(vr.init(), Err(Error::WrongDevice));
        let bus = Bus::new(vec![code_ok()]);
        let mut vr = Tps53647::new(bus, Config::Q1370);
        vr.init().unwrap();
        let log = vr.release().log;
        assert!(log.contains(&byte(MFR_20_PHASES, 3)));
        assert!(log.contains(&byte(MFR_10_IMAX, 123)));
        assert!(log.contains(&word(pm::IOUT_OC_FAULT_LIMIT, L11_160)));
        assert!(!log
            .iter()
            .any(|t| matches!(t, Tx::Write(_, b) if b[0] == MFR_24_PHASE_ENABLE)));
    }

    #[test]
    fn each_part_is_checked_against_its_own_code() {
        // A TPS53667 answering the 647's code, and the other way round,
        // is refused before anything is written.
        let mut vr = Tps53647::new(Bus::new(vec![code_ok()]), Config::NERDOCTAXE_GAMMA_6P);
        assert_eq!(vr.init(), Err(Error::WrongDevice));
        let mut vr = Tps53647::new(Bus::new(vec![code_667()]), Config::NERDOCTAXE_GAMMA_4P);
        assert_eq!(vr.init(), Err(Error::WrongDevice));
    }

    #[test]
    fn wrong_device_code_writes_nothing() {
        let bus = Bus::new(vec![Tx::WriteRead(
            ADDR,
            vec![MFR_44_DEVICE_CODE],
            vec![0x24, 0x00],
        )]);
        let mut vr = Tps53647::new(bus, Config::NERDQAXE_PLUSPLUS);
        assert_eq!(vr.init(), Err(Error::WrongDevice));
        assert_eq!(vr.last_command(), MFR_44_DEVICE_CODE);
        let log = vr.release().log;
        assert_eq!(log.len(), 1);
        assert!(matches!(log[0], Tx::WriteRead(..)));
    }

    #[test]
    fn bad_phase_count_is_refused_before_the_bus() {
        let cfg = Config {
            phases: 7,
            ..Config::Q1373_6P
        };
        let mut vr = Tps53647::new(Bus::sink(), cfg);
        assert_eq!(vr.init(), Err(Error::OutOfRange));
        assert!(vr.release().log.is_empty());
        let cfg = Config {
            phases: 0,
            ..Config::Q1370
        };
        assert_eq!(
            Tps53647::new(Bus::sink(), cfg).init(),
            Err(Error::OutOfRange)
        );
    }

    #[test]
    fn vid_rounds_to_the_step_and_round_trips_every_5mv_point() {
        // 1.060 V: (1060 − 250) / 5 + 1 = 163. Truncating float maths
        // gives 162 (1.055 V), the NerdOS slip.
        assert_eq!(volts_to_vid(1.060), 163);
        assert_eq!(volts_to_vid(1.150), 181);
        assert_eq!(volts_to_vid(0.800), 111);
        assert_eq!(volts_to_vid(1.400), 231);
        assert_eq!(volts_to_vid(0.25), 1);
        assert_eq!(volts_to_vid(0.0), 0);
        assert_eq!(vid_to_volts(0), 0.0);
        assert_eq!(vid_to_volts(1), 0.25);
        assert_eq!(vid_to_volts(0x97), 1.0);
        for mv in (800..=1400).step_by(5) {
            let v = mv as f32 / 1000.0;
            let vid = volts_to_vid(v);
            let back = (vid_to_volts(vid) * 1000.0 + 0.5) as u32;
            let expect = if mv == 1000 { 995 } else { mv };
            assert_eq!(back, expect, "{mv} mV -> vid {vid}");
        }
        // Off-grid values round to the nearest step.
        assert_eq!(volts_to_vid(1.062), 163);
        assert_eq!(volts_to_vid(1.063), 164);
    }

    #[test]
    fn factory_vid_is_never_written() {
        assert_eq!(volts_to_vid(1.0), 0x96);
        let mut vr = Tps53647::new(Bus::sink(), Config::NERDQAXE_PLUSPLUS);
        assert_eq!(vr.set_vout(1.0), Ok(0x96));
        assert_eq!(vr.set_vout(1.06), Ok(0xA3));
        let log = vr.release().log;
        assert_eq!(log[0], word(pm::VOUT_COMMAND, 0x0096));
        assert_eq!(log[1], word(pm::VOUT_COMMAND, 0x00A3));
    }

    #[test]
    fn set_vout_refuses_outside_the_window_and_zero() {
        let mut vr = Tps53647::new(Bus::sink(), Config::NERDQAXE_PLUSPLUS);
        assert_eq!(vr.set_vout(0.799), Err(Error::OutOfRange));
        assert_eq!(vr.set_vout(1.401), Err(Error::OutOfRange));
        assert_eq!(vr.set_vout(0.0), Err(Error::OutOfRange));
        assert_eq!(vr.set_vout(0.8), Ok(111));
        assert_eq!(vr.set_vout(1.4), Ok(231));
        assert_eq!(vr.release().log.len(), 2);
    }

    #[test]
    fn vout_readback_uses_exponent_minus_nine() {
        // 1.150 V × 512 = 588.8 → 589 = 0x024D.
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![MFR_04_VOUT], vec![0x4D, 0x02]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_COMMAND], vec![0x97, 0x00]),
            Tx::WriteRead(ADDR, vec![pm::READ_TEMPERATURE_1], vec![62, 0]),
            Tx::WriteRead(ADDR, vec![pm::READ_IOUT], vec![0x80, 0xF2]),
            Tx::WriteRead(ADDR, vec![pm::READ_PIN], vec![0xB0, 0xE9]),
        ]);
        let mut vr = Tps53647::new(bus, Config::NERDQAXE_PLUSPLUS);
        assert!((vr.vout().unwrap() - 1.150).abs() < 0.002);
        assert_eq!(vr.vout_vid().unwrap(), 0x97, "reset tell-tale readable");
        assert_eq!(vr.temperature_c().unwrap(), 62);
        assert_eq!(vr.iout().unwrap(), 160.0);
        // 0xE9B0: exponent −3, mantissa 432 → 54 W.
        assert_eq!(vr.pin().unwrap(), 54.0);
    }

    #[test]
    fn status_masks_the_cml_bit_only() {
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![pm::STATUS_BYTE], vec![0x02]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_WORD], vec![0x02, 0x00]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_VOUT], vec![0x00]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_IOUT], vec![0x00]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_INPUT], vec![0x00]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_TEMPERATURE], vec![0x00]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_MFR_SPECIFIC], vec![0x00]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_BYTE], vec![0x42]),
            Tx::WriteRead(ADDR, vec![pm::STATUS_WORD], vec![0x42, 0x08]),
        ]);
        let mut vr = Tps53647::new(bus, Config::NERDQAXE_PLUSPLUS);
        let s = vr.status().unwrap();
        assert_eq!(s, Status::default());
        assert!(!s.any());
        assert_eq!(vr.status_byte().unwrap(), 0x40);
        assert_eq!(vr.status_word().unwrap(), 0x0840);
    }

    #[test]
    fn clear_faults_is_a_bare_command() {
        let mut vr = Tps53647::new(Bus::sink(), Config::Q1370);
        vr.clear_faults().unwrap();
        assert_eq!(vr.last_command(), pm::CLEAR_FAULTS);
        assert_eq!(vr.release().log, vec![cmd(pm::CLEAR_FAULTS)]);
    }

    #[test]
    fn device_code_decides_the_part_and_the_strap_the_phases() {
        // Q1373, strap high (four-phase population) but a TPS53667 answers:
        // NerdOS q1373.cpp: 667, 4 phases, IMAX 179 (Rmon 33.2 k), IFAULT 4 × 40.
        let (c, note) = resolve_by_code(DEVICE_CODE_667, Config::Q1373_4P, Config::Q1373_6P);
        assert_eq!(
            c,
            Config {
                variant: Variant::Tps53667,
                phases: 4,
                imax: 179,
                ifault: 160,
                ..Config::Q1373_6P
            }
        );
        assert!(note.is_some());
        // Strap low (six-phase) but a TPS53647 answers: four phases, 647 limits.
        let (c, note) = resolve_by_code(DEVICE_CODE, Config::Q1373_6P, Config::Q1373_4P);
        assert_eq!(c, Config::Q1373_4P);
        assert!(note.is_some());
        // Strap and code agree: unchanged, no note.
        assert_eq!(
            resolve_by_code(DEVICE_CODE_667, Config::Q1373_6P, Config::Q1373_4P),
            (Config::Q1373_6P, None)
        );
        assert_eq!(
            resolve_by_code(DEVICE_CODE, Config::Q1373_4P, Config::Q1373_6P),
            (Config::Q1373_4P, None)
        );
        // Unknown code: the strap's population stays, init refuses it.
        let (c, note) = resolve_by_code(0x0000, Config::Q1373_4P, Config::Q1373_6P);
        assert_eq!(c, Config::Q1373_4P);
        assert!(note.is_some());
        let mut vr = Tps53647::new(
            Bus::new(vec![Tx::WriteRead(
                ADDR,
                vec![MFR_44_DEVICE_CODE],
                vec![0, 0],
            )]),
            c,
        );
        assert_eq!(vr.init(), Err(Error::WrongDevice));
    }

    #[test]
    fn board_configs_match_nerdos() {
        assert_eq!(Config::NERDQAXE_PLUS.imax, 60);
        assert_eq!(Config::NERDQAXE_PLUS.ifault, 55);
        assert_eq!(Config::NERDOCTAXE_GAMMA_6P.variant, Variant::Tps53667);
        assert_eq!(Config::NERDOCTAXE_GAMMA_4P.variant, Variant::Tps53647);
        // Q1373 IMAX from the Rmon formula: 0.85 / R / 5e-3 × 35000.
        let imax = |r: f32| (0.85 / r / 5.0e-3 * 35000.0 + 0.5) as u8;
        assert_eq!(imax(48_700.0), Config::Q1373_4P.imax);
        assert_eq!(imax(33_200.0), Config::Q1373_6P.imax);
        assert_eq!(Config::Q1373_6P.ifault, 6 * 40);
        assert!(
            Config::Q1370.ifault > u16::from(Config::Q1370.imax),
            "as NerdOS has it"
        );
    }
}

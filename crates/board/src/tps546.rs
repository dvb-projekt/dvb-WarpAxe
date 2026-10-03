//! TPS546D24A/S: the core-voltage regulator on the Bitaxe, PMBus over
//! I²C at 0x24. Init writes the whole operating window the way
//! ESP-Miner's `TPS546_init` + `TPS546_write_entire_config` do, so a
//! board that runs under ESP-Miner runs under the same limits here.

use crate::i2c::{Error, I2c};
use crate::pmbus::{self as pm, f32_to_linear11, f32_to_ulinear16, i32_to_linear11};

/// I²C address of the TPS546.
pub const ADDR: u8 = 0x24;
/// SMBus alert response address: reading it makes an alerting device
/// answer with its own address and drop the alert.
pub const ALERT_RESPONSE_ADDR: u8 = 0x0C;
const OPERATION_OFF: u8 = 0x00;
const OPERATION_ON: u8 = 0x80;
/// ON_OFF_CONFIG: DELAY | POLARITY | CMD | PU — the OPERATION command
/// starts and stops conversion, the CONTROL pin does not.
const ON_OFF_CONFIG: u8 = 0x01 | 0x02 | 0x08 | 0x10;
const DEVICE_ID_D24A: [u8; 6] = [0x54, 0x49, 0x54, 0x6D, 0x24, 0x41];
const DEVICE_ID_D24S: [u8; 6] = [0x54, 0x49, 0x54, 0x6D, 0x24, 0x62];
/// A third id NerdOS's Rev 7 driver has seen on TPS546D24A parts.
const DEVICE_ID_D24A_ALT: [u8; 6] = [0x54, 0x49, 0x54, 0x6B, 0x24, 0x41];

/// The operating window written at init. Volts, amps, kHz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Input voltage at which conversion may start, V.
    pub vin_on: f32,
    /// Input voltage below which conversion stops, V.
    pub vin_off: f32,
    /// 0 = do not write the warn limit.
    pub vin_uv_warn: f32,
    /// Input over-voltage fault limit, V.
    pub vin_ov_fault: f32,
    /// VOUT_SCALE_LOOP: the feedback divider ratio of the board.
    pub scale_loop: f32,
    /// Lowest output the driver will command, V.
    pub vout_min: f32,
    /// Highest output the driver will command, V.
    pub vout_max: f32,
    /// Output voltage at init (the chip's reset default is below it).
    pub vout_command: f32,
    /// Output over-current warning, A.
    pub iout_oc_warn: f32,
    /// Output over-current fault, A.
    pub iout_oc_fault: f32,
    /// Switching frequency, kHz.
    pub frequency_khz: i32,
    /// PMBus PHASE (0 = single).
    pub phase: u8,
    /// Phase stacking (0 = single).
    pub stack_config: u16,
    /// SYNC_CONFIG register value.
    pub sync_config: u8,
    /// COMPENSATION_CONFIG block, only written when set.
    pub compensation: Option<[u8; 5]>,
    /// PIN_DETECT_OVERRIDE, written when set (ESP-Miner: 0xFFFF; the
    /// BitMaker NerdAxe driver: 0x0000).
    pub pin_detect_override: Option<u16>,
    /// Write STACK_CONFIG, SYNC_CONFIG and PHASE at init (ESP-Miner
    /// does; the BitMaker NerdAxe driver leaves the part's own values).
    pub write_phase_config: bool,
}

impl Config {
    /// ESP-Miner `TPS546_CONFIG_DEFAULT`: the 5 V single-chip boards
    /// (Bitaxe Gamma 60x). Output window 1–2 V, 1.2 V at init.
    pub const BITAXE_5V: Config = Config {
        vin_on: 4.8,
        vin_off: 4.5,
        vin_uv_warn: 0.0,
        vin_ov_fault: 6.5,
        scale_loop: 0.25,
        vout_min: 1.0,
        vout_max: 2.0,
        vout_command: 1.2,
        iout_oc_warn: 25.0,
        iout_oc_fault: 30.0,
        frequency_khz: 650,
        phase: 0x00,
        stack_config: 0x0000,
        sync_config: 0x10,
        compensation: None,
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
    };

    /// NerdOS `drivers/nerdaxe/TPS546` as the NerdAxe Gaia sets it up
    /// (BitMaker fork, `nerdaxegaia.cpp` ae509770): the 5 V driver
    /// defaults with the 12 V input window (on 10.5, off 9.5, UV warn
    /// 10.5, OV fault 14), IOUT 28/33 A, 400 kHz after init, output
    /// window 0.8–3 V, PIN_DETECT_OVERRIDE 0x0000, and no STACK / SYNC /
    /// PHASE writes (`write_entire_config`, read again 2026-09-29).
    pub const NERDAXE_GAIA: Config = Config {
        vin_on: 10.5,
        vin_off: 9.5,
        vin_uv_warn: 10.5,
        vin_ov_fault: 14.0,
        scale_loop: 0.25,
        vout_min: 0.8,
        vout_max: 3.0,
        vout_command: 1.2,
        iout_oc_warn: 28.0,
        iout_oc_fault: 33.0,
        frequency_khz: 400,
        phase: 0x00,
        stack_config: 0x0000,
        sync_config: 0x10,
        compensation: None,
        pin_detect_override: Some(0x0000),
        write_phase_config: false,
    };

    /// BitForge Nano: one TPS546D24A on the 12 V rail feeding two BM1370
    /// in parallel (schematic, WantClueTechnologies/BitForge-Nano; the
    /// sense lines go straight to the core rail, no external divider).
    /// ForgeOS is closed; what it left in the running part on the first
    /// start (2026-09-29, .44): VOUT_COMMAND 1.148 V, VOUT_MAX 2 V,
    /// PHASE 0, ON_OFF_CONFIG 0x1F. The part's NVM after a power cycle:
    /// IOUT OC warn 40 A / fault 52 A, VIN OV fault 21 V, VIN on/off
    /// 2.75/2.5 V, 900 kHz, scale loop 0.5, OT 125/150 °C. Taken here:
    /// the NVM's 52 A fault limit (the warning raised to 45 A, see
    /// below), the Gaia's proven 12 V input window
    /// (on 10.5 / off 9.5, UV warn 10.5, OV fault 14.8), ESP-Miner's
    /// 650 kHz, scale loop 0.25 and 1–2 V window (proven on the same
    /// part with direct sensing), our OT 105/145 °C and the shut-down OC
    /// response.
    pub const BITFORGE_NANO: Config = Config {
        vin_on: 10.5,
        vin_off: 9.5,
        vin_uv_warn: 10.5,
        vin_ov_fault: 14.8,
        scale_loop: 0.25,
        vout_min: 1.0,
        vout_max: 2.0,
        vout_command: 1.2,
        // The NVM says 40 A; tuners stop short of the warning line, so
        // at 40 A the Max preset (625 MHz / 1160 mV, ~38 A) was out of
        // reach. 45 A lets it run; the part still shuts down at 52 A.
        iout_oc_warn: 45.0,
        iout_oc_fault: 52.0,
        frequency_khz: 650,
        phase: 0x00,
        stack_config: 0x0000,
        sync_config: 0x10,
        compensation: None,
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
    };

    /// ESP-Miner `TPS546_CONFIG_GAMMA_TURBO` (Bitaxe Gamma Turbo, 801):
    /// two stacked TPS546 on 12 V, one voltage domain.
    pub const GAMMA_TURBO: Config = Config {
        vin_on: 11.0,
        vin_off: 10.5,
        vin_uv_warn: 11.0,
        vin_ov_fault: 14.8,
        scale_loop: 0.25,
        vout_min: 1.0,
        vout_max: 3.0,
        vout_command: 1.2,
        iout_oc_warn: 50.0,
        iout_oc_fault: 55.0,
        frequency_khz: 650,
        phase: 0xFF,
        stack_config: 0x0001,
        sync_config: 0xD0,
        compensation: Some([0x12, 0x34, 0x42, 0x21, 0x04]),
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
    };

    /// ESP-Miner `TPS546_CONFIG_NAJA_DUO` (Bitaxe Naja Duo, 1201): two
    /// stacked phases, two chip voltage domains in series (VOUT = 2 × core).
    pub const NAJA_DUO: Config = Config {
        vin_on: 11.0,
        vin_off: 10.5,
        vin_uv_warn: 11.0,
        vin_ov_fault: 14.0,
        scale_loop: 0.125,
        vout_min: 1.8,
        vout_max: 3.0,
        vout_command: 2.2,
        iout_oc_warn: 50.0,
        iout_oc_fault: 55.0,
        frequency_khz: 650,
        phase: 0xFF,
        stack_config: 0x0001,
        sync_config: 0xD0,
        compensation: Some([0x12, 0x34, 0x42, 0x25, 0x04]),
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
    };

    /// ESP-Miner `TPS546_CONFIG_GAMMA_HEX` (Bitaxe Gamma Hex, 1300): four
    /// stacked phases, two voltage domains.
    pub const GAMMA_HEX: Config = Config {
        vin_on: 11.0,
        vin_off: 10.5,
        vin_uv_warn: 11.0,
        vin_ov_fault: 14.0,
        scale_loop: 0.125,
        vout_min: 2.0,
        vout_max: 3.0,
        vout_command: 2.4,
        iout_oc_warn: 150.0,
        iout_oc_fault: 160.0,
        frequency_khz: 650,
        phase: 0xFF,
        stack_config: 0x0003,
        sync_config: 0xD0,
        compensation: Some([0x02, 0x64, 0x82, 0x11, 0x06]),
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
    };

    /// NerdOS `drivers/rev7/TPS546` for the NerdQAxe++ Rev 7: its
    /// `create_dual_config` (12 V input window, two phases stacked,
    /// multi-phase sync, the compensation block) with the output window
    /// raised to 2–3 V and 2.4 V at init, because the four chips sit in
    /// two series voltage domains (VOUT = 2 × core).
    pub const NERDQAXE_PLUSPLUS_REV7: Config = Config {
        vin_on: 11.0,
        vin_off: 10.5,
        vin_uv_warn: 11.0,
        vin_ov_fault: 14.0,
        scale_loop: 0.125,
        vout_min: 2.0,
        vout_max: 3.0,
        vout_command: 2.4,
        iout_oc_warn: 90.0,
        iout_oc_fault: 100.0,
        frequency_khz: 650,
        phase: 0xFF,
        stack_config: 0x0001,
        sync_config: 0xD0,
        compensation: Some([0x12, 0x34, 0x42, 0x25, 0x04]),
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
    };
}

// Fixed limits, ESP-Miner TPS546.h.
const VIN_OV_FAULT_RESPONSE: u8 = 0xB7;
const IOUT_OC_FAULT_RESPONSE: u8 = 0xC0; // shut down, no retries
const OT_WARN_C: i32 = 105;
const OT_FAULT_C: i32 = 145;
const OT_FAULT_RESPONSE: u8 = 0xFF; // wait for cooling, retry
const TON_RISE_MS: i32 = 3;
const TON_MAX_FAULT_RESPONSE: u8 = 0x3B;
// VOUT fault, warn and margin limits. The TPS546 takes them as ratios
// of VOUT_COMMAND (1.25 = 125 %), written in the VOUT_MODE format like a
// voltage; a limit written as absolute volts is out of range and the
// part NACKs it (found on a Gamma: 0x40 refused, readback 640 = 1.25).
const OV_FAULT: f32 = 1.25;
const OV_WARN: f32 = 1.16;
const MARGIN_HIGH: f32 = 1.10;
const MARGIN_LOW: f32 = 0.90;
const UV_WARN: f32 = 0.90;
const UV_FAULT: f32 = 0.75;

/// The driver, owning the bus.
pub struct Tps546<I> {
    bus: I,
    cfg: Config,
    /// From VOUT_MODE at init; ULINEAR16 exponent.
    vout_exp: i32,
    /// The last PMBus command sent, for a failure report.
    last_cmd: u8,
}

impl<I: I2c> Tps546<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, cfg: Config) -> Self {
        Tps546 {
            bus,
            cfg,
            vout_exp: -9,
            last_cmd: 0,
        }
    }

    /// The operating window this driver writes at init.
    pub fn config(&self) -> Config {
        self.cfg
    }

    /// Give the bus back.
    pub fn release(self) -> I {
        self.bus
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

    fn write_byte(&mut self, cmd: u8, v: u8) -> Result<(), Error<I::Error>> {
        self.last_cmd = cmd;
        Ok(self.bus.write(ADDR, &[cmd, v])?)
    }

    fn write_word(&mut self, cmd: u8, v: u16) -> Result<(), Error<I::Error>> {
        self.last_cmd = cmd;
        let [lo, hi] = v.to_le_bytes();
        Ok(self.bus.write(ADDR, &[cmd, lo, hi])?)
    }

    fn write_ulinear16(&mut self, cmd: u8, volts: f32) -> Result<(), Error<I::Error>> {
        self.write_word(cmd, f32_to_ulinear16(volts, self.vout_exp))
    }

    /// Read the SMBus alert response address: a device holding ALERT
    /// answers with its address and releases the line. ESP-Miner does
    /// this before the id check, then waits 15 ms; a NACK (nobody
    /// alerting) is the normal answer and comes back as the bus error.
    pub fn alert_response(&mut self) -> Result<u8, Error<I::Error>> {
        let mut b = [0u8; 1];
        self.bus.read(ALERT_RESPONSE_ADDR, &mut b)?;
        Ok(b[0])
    }

    /// Raw word read, for diagnostics.
    pub fn read_register(&mut self, cmd: u8) -> Result<u16, Error<I::Error>> {
        self.read_word(cmd)
    }

    /// Raw word write, for diagnostics.
    pub fn write_register(&mut self, cmd: u8, v: u16) -> Result<(), Error<I::Error>> {
        self.write_word(cmd, v)
    }

    /// IC_DEVICE_ID as a block read: six bytes after the length byte.
    pub fn device_id(&mut self) -> Result<[u8; 6], Error<I::Error>> {
        let mut id = [0u8; 7];
        self.bus.write_read(ADDR, &[pm::IC_DEVICE_ID], &mut id)?;
        if id[0] != 6 {
            return Err(Error::BadLength);
        }
        let mut out = [0u8; 6];
        out.copy_from_slice(&id[1..]);
        Ok(out)
    }

    /// VOUT_MODE, the one-byte register every PMBus part answers.
    pub fn vout_mode(&mut self) -> Result<u8, Error<I::Error>> {
        self.read_byte(pm::VOUT_MODE)
    }

    /// Check the device id, stop conversion, write the whole config.
    /// Conversion stays off; [`set_vout`](Self::set_vout) turns it on.
    /// Returns STATUS_WORD after the write.
    pub fn init(&mut self) -> Result<u16, Error<I::Error>> {
        let mut id = [0u8; 7];
        self.bus.write_read(ADDR, &[pm::IC_DEVICE_ID], &mut id)?;
        if id[0] != 6 {
            return Err(Error::BadLength);
        }
        if id[1..] != DEVICE_ID_D24A && id[1..] != DEVICE_ID_D24S && id[1..] != DEVICE_ID_D24A_ALT {
            return Err(Error::WrongDevice);
        }
        self.write_byte(pm::OPERATION, OPERATION_OFF)?;
        self.write_byte(pm::ON_OFF_CONFIG, ON_OFF_CONFIG)?;
        let mode = self.read_byte(pm::VOUT_MODE)?;
        self.vout_exp = pm::vout_mode_exponent(mode);

        let c = self.cfg;
        if c.write_phase_config {
            self.write_word(pm::STACK_CONFIG, c.stack_config)?;
            self.write_byte(pm::SYNC_CONFIG, c.sync_config)?;
            self.write_byte(pm::PHASE, c.phase)?;
        }
        self.write_word(pm::FREQUENCY_SWITCH, i32_to_linear11(c.frequency_khz))?;
        if let Some(comp) = c.compensation {
            let mut block = [0u8; 7];
            block[0] = pm::COMPENSATION_CONFIG;
            block[1] = 5;
            block[2..].copy_from_slice(&comp);
            self.bus.write(ADDR, &block)?;
        }
        if c.vin_uv_warn > 0.0 {
            self.write_word(pm::VIN_UV_WARN_LIMIT, f32_to_linear11(c.vin_uv_warn))?;
        }
        self.write_word(pm::VIN_ON, f32_to_linear11(c.vin_on))?;
        self.write_word(pm::VIN_OFF, f32_to_linear11(c.vin_off))?;
        self.write_word(pm::VIN_OV_FAULT_LIMIT, f32_to_linear11(c.vin_ov_fault))?;
        self.write_byte(pm::VIN_OV_FAULT_RESPONSE, VIN_OV_FAULT_RESPONSE)?;
        self.write_word(pm::VOUT_SCALE_LOOP, f32_to_linear11(c.scale_loop))?;
        self.write_ulinear16(pm::VOUT_COMMAND, c.vout_command)?;
        self.write_ulinear16(pm::VOUT_MAX, c.vout_max)?;
        self.write_ulinear16(pm::VOUT_MIN, c.vout_min)?;
        self.write_vout_limits()?;
        self.write_word(pm::IOUT_OC_WARN_LIMIT, f32_to_linear11(c.iout_oc_warn))?;
        self.write_word(pm::IOUT_OC_FAULT_LIMIT, f32_to_linear11(c.iout_oc_fault))?;
        self.write_byte(pm::IOUT_OC_FAULT_RESPONSE, IOUT_OC_FAULT_RESPONSE)?;
        self.write_word(pm::OT_WARN_LIMIT, i32_to_linear11(OT_WARN_C))?;
        self.write_word(pm::OT_FAULT_LIMIT, i32_to_linear11(OT_FAULT_C))?;
        self.write_byte(pm::OT_FAULT_RESPONSE, OT_FAULT_RESPONSE)?;
        self.write_word(pm::TON_DELAY, 0)?;
        self.write_word(pm::TON_RISE, i32_to_linear11(TON_RISE_MS))?;
        self.write_word(pm::TON_MAX_FAULT_LIMIT, 0)?;
        self.write_byte(pm::TON_MAX_FAULT_RESPONSE, TON_MAX_FAULT_RESPONSE)?;
        self.write_word(pm::TOFF_DELAY, 0)?;
        self.write_word(pm::TOFF_FALL, 0)?;
        if let Some(v) = c.pin_detect_override {
            self.write_word(pm::PIN_DETECT_OVERRIDE, v)?;
        }
        self.read_word(pm::STATUS_WORD)
    }

    /// The fault/warn/margin window, as ratios of VOUT_COMMAND; the
    /// part scales them itself, so they are rewritten unchanged after a
    /// new VOUT_COMMAND (as ESP-Miner does).
    fn write_vout_limits(&mut self) -> Result<(), Error<I::Error>> {
        self.write_ulinear16(pm::VOUT_OV_FAULT_LIMIT, OV_FAULT)?;
        self.write_ulinear16(pm::VOUT_OV_WARN_LIMIT, OV_WARN)?;
        self.write_ulinear16(pm::VOUT_MARGIN_HIGH, MARGIN_HIGH)?;
        self.write_ulinear16(pm::VOUT_MARGIN_LOW, MARGIN_LOW)?;
        self.write_ulinear16(pm::VOUT_UV_WARN_LIMIT, UV_WARN)?;
        self.write_ulinear16(pm::VOUT_UV_FAULT_LIMIT, UV_FAULT)
    }

    /// Set the output and turn conversion on; 0 turns it off. Outside
    /// the configured window nothing is written.
    pub fn set_vout(&mut self, volts: f32) -> Result<(), Error<I::Error>> {
        if volts == 0.0 {
            return self.write_byte(pm::OPERATION, OPERATION_OFF);
        }
        if volts < self.cfg.vout_min || volts > self.cfg.vout_max {
            return Err(Error::OutOfRange);
        }
        self.write_ulinear16(pm::VOUT_COMMAND, volts)?;
        self.write_vout_limits()?;
        self.write_byte(pm::OPERATION, OPERATION_ON)
    }

    /// Whether the output is commanded on (OPERATION bit 7), whatever
    /// set it: the part keeps OPERATION through an ESP reset.
    pub fn is_on(&mut self) -> Result<bool, Error<I::Error>> {
        Ok(self.read_byte(pm::OPERATION)? & OPERATION_ON != 0)
    }

    /// The output off by command, nothing else touched: usable before
    /// [`init`](Self::init), on a part in whatever state a reset left it.
    pub fn off(&mut self) -> Result<(), Error<I::Error>> {
        self.write_byte(pm::OPERATION, OPERATION_OFF)
    }

    /// Input voltage, V.
    pub fn vin(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::linear11_to_f32(self.read_word(pm::READ_VIN)?))
    }

    /// Output voltage, V.
    pub fn vout(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::ulinear16_to_f32(
            self.read_word(pm::READ_VOUT)?,
            self.vout_exp,
        ))
    }

    /// Output current, A.
    pub fn iout(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(pm::linear11_to_f32(self.read_word(pm::READ_IOUT)?))
    }

    /// Die temperature, °C.
    pub fn temperature_c(&mut self) -> Result<i32, Error<I::Error>> {
        Ok(pm::linear11_to_i32(self.read_word(pm::READ_TEMPERATURE_1)?))
    }

    /// STATUS_WORD as the device reports it.
    pub fn status_word(&mut self) -> Result<u16, Error<I::Error>> {
        self.read_word(pm::STATUS_WORD)
    }

    /// One of the STATUS_* byte registers (STATUS_VOUT, _IOUT, _INPUT,
    /// _TEMPERATURE, _CML, _MFR_SPECIFIC), for a fault snapshot.
    pub fn status_register(&mut self, cmd: u8) -> Result<u8, Error<I::Error>> {
        self.read_byte(cmd)
    }

    /// Clear latched faults.
    pub fn clear_faults(&mut self) -> Result<(), Error<I::Error>> {
        Ok(self.bus.write(ADDR, &[pm::CLEAR_FAULTS])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    fn id_ok() -> Tx {
        let mut a = vec![6u8];
        a.extend_from_slice(&DEVICE_ID_D24A);
        Tx::WriteRead(ADDR, vec![pm::IC_DEVICE_ID], a)
    }

    #[test]
    fn is_on_reads_operation_and_off_writes_it_alone() {
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![pm::OPERATION], vec![0x80]),
            Tx::Write(ADDR, vec![pm::OPERATION, 0x00]),
            Tx::WriteRead(ADDR, vec![pm::OPERATION], vec![0x00]),
        ]);
        let mut vr = Tps546::new(bus, Config::BITAXE_5V);
        assert_eq!(vr.is_on(), Ok(true));
        assert_eq!(vr.off(), Ok(()));
        assert_eq!(vr.is_on(), Ok(false));
        let log = vr.release().log;
        assert_eq!(log.len(), 3, "off touches nothing but OPERATION");
        assert_eq!(log[1], Tx::Write(ADDR, vec![pm::OPERATION, 0x00]));
    }

    #[test]
    fn init_checks_the_device_id_first() {
        let mut bad = vec![6u8];
        bad.extend_from_slice(b"XXXXXX");
        let bus = Bus::new(vec![Tx::WriteRead(ADDR, vec![pm::IC_DEVICE_ID], bad)]);
        let mut vr = Tps546::new(bus, Config::BITAXE_5V);
        assert_eq!(vr.init(), Err(Error::WrongDevice));
        assert_eq!(
            vr.release().log.len(),
            1,
            "nothing written after a wrong id"
        );
    }

    #[test]
    fn init_stops_conversion_and_writes_the_window() {
        let bus = Bus::new(vec![
            id_ok(),
            Tx::Write(ADDR, vec![pm::OPERATION, 0x00]),
            Tx::Write(ADDR, vec![pm::ON_OFF_CONFIG, 0x1B]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_MODE], vec![0x17]),
        ]);
        let mut vr = Tps546::new(bus, Config::BITAXE_5V);
        vr.init().unwrap();
        let log = vr.release().log;
        let find = |cmd: u8| {
            log.iter().find_map(|t| match t {
                Tx::Write(_, b) if b[0] == cmd => Some(b[1..].to_vec()),
                _ => None,
            })
        };
        // VOUT_COMMAND 1.2 V at exponent -9 = 614 = 0x0266.
        assert_eq!(find(pm::VOUT_COMMAND), Some(vec![0x66, 0x02]));
        // VIN_ON 4.8 V as LINEAR11 0xCA66.
        assert_eq!(find(pm::VIN_ON), Some(vec![0x66, 0xCA]));
        // OV fault ratio 1.25 = 640 = 0x0280, what a Gamma reads back.
        assert_eq!(find(pm::VOUT_OV_FAULT_LIMIT), Some(vec![0x80, 0x02]));
        assert_eq!(find(pm::OT_FAULT_LIMIT), Some(vec![145, 0]));
        assert_eq!(find(pm::IOUT_OC_FAULT_RESPONSE), Some(vec![0xC0]));
        assert_eq!(find(pm::PIN_DETECT_OVERRIDE), Some(vec![0xFF, 0xFF]));
        assert!(
            find(pm::COMPENSATION_CONFIG).is_none(),
            "no block without config"
        );
        assert!(
            find(pm::VIN_UV_WARN_LIMIT).is_none(),
            "warn limit 0 = not written"
        );
        // Conversion is still off: no OPERATION_ON in the log.
        assert!(!log.contains(&Tx::Write(ADDR, vec![pm::OPERATION, 0x80])));
    }

    #[test]
    fn rev7_config_writes_the_dual_phase_registers_and_accepts_the_third_id() {
        let mut a = vec![6u8];
        a.extend_from_slice(&DEVICE_ID_D24A_ALT);
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![pm::IC_DEVICE_ID], a),
            Tx::Write(ADDR, vec![pm::OPERATION, 0x00]),
            Tx::Write(ADDR, vec![pm::ON_OFF_CONFIG, 0x1B]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_MODE], vec![0x17]),
        ]);
        let mut vr = Tps546::new(bus, Config::NERDQAXE_PLUSPLUS_REV7);
        vr.init().unwrap();
        let log = vr.release().log;
        let find = |cmd: u8| {
            log.iter().find_map(|t| match t {
                Tx::Write(_, b) if b[0] == cmd => Some(b[1..].to_vec()),
                _ => None,
            })
        };
        assert_eq!(find(pm::STACK_CONFIG), Some(vec![0x01, 0x00]));
        assert_eq!(find(pm::SYNC_CONFIG), Some(vec![0xD0]));
        assert_eq!(find(pm::PHASE), Some(vec![0xFF]));
        assert_eq!(
            find(pm::COMPENSATION_CONFIG),
            Some(vec![5, 0x12, 0x34, 0x42, 0x25, 0x04])
        );
        // VIN_ON 11 V = 704 · 2⁻⁶ = 0xD2C0; VIN_UV_WARN written (11 V).
        assert_eq!(find(pm::VIN_ON), Some(vec![0xC0, 0xD2]));
        assert_eq!(find(pm::VIN_UV_WARN_LIMIT), Some(vec![0xC0, 0xD2]));
        // VOUT_COMMAND 2.4 V × 512 = 1228 = 0x04CC; VOUT_MIN 2 V = 0x0400.
        assert_eq!(find(pm::VOUT_COMMAND), Some(vec![0xCC, 0x04]));
        assert_eq!(find(pm::VOUT_MIN), Some(vec![0x00, 0x04]));
        // IOUT fault 100 A = 800 · 2⁻³ = 0xEB20.
        assert_eq!(find(pm::IOUT_OC_FAULT_LIMIT), Some(vec![0x20, 0xEB]));
        // The output window is the raised one: 1.2 V (one domain) refused.
        let mut vr = Tps546::new(Bus::sink(), Config::NERDQAXE_PLUSPLUS_REV7);
        assert_eq!(vr.set_vout(1.2), Err(Error::OutOfRange));
        assert!(vr.set_vout(2.4).is_ok());
    }

    #[test]
    fn gaia_config_writes_the_12v_window_and_no_pin_detect_override() {
        let bus = Bus::new(vec![
            id_ok(),
            Tx::Write(ADDR, vec![pm::OPERATION, 0x00]),
            Tx::Write(ADDR, vec![pm::ON_OFF_CONFIG, 0x1B]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_MODE], vec![0x17]),
        ]);
        let mut vr = Tps546::new(bus, Config::NERDAXE_GAIA);
        vr.init().unwrap();
        let log = vr.release().log;
        let find = |cmd: u8| {
            log.iter().find_map(|t| match t {
                Tx::Write(_, b) if b[0] == cmd => Some(b[1..].to_vec()),
                _ => None,
            })
        };
        // VIN_ON 10.5 V = 672 · 2⁻⁶ = 0xD2A0; VIN_OV_FAULT 14 V = 896 · 2⁻⁶ = 0xD380.
        assert_eq!(find(pm::VIN_ON), Some(vec![0xA0, 0xD2]));
        assert_eq!(find(pm::VIN_UV_WARN_LIMIT), Some(vec![0xA0, 0xD2]));
        assert_eq!(find(pm::VIN_OV_FAULT_LIMIT), Some(vec![0x80, 0xD3]));
        // IOUT fault 33 A = 528 · 2⁻⁴ = 0xE210; 400 kHz = 0x0190.
        assert_eq!(find(pm::IOUT_OC_FAULT_LIMIT), Some(vec![0x10, 0xE2]));
        assert_eq!(find(pm::FREQUENCY_SWITCH), Some(vec![0x90, 0x01]));
        // VOUT_MIN 0.8 V × 512 = 409 = 0x0199; VOUT_MAX 3 V = 0x0600.
        assert_eq!(find(pm::VOUT_MIN), Some(vec![0x99, 0x01]));
        assert_eq!(find(pm::VOUT_MAX), Some(vec![0x00, 0x06]));
        assert_eq!(
            find(pm::PIN_DETECT_OVERRIDE),
            Some(vec![0x00, 0x00]),
            "BitMaker writes 0x0000"
        );
        assert!(
            find(pm::STACK_CONFIG).is_none()
                && find(pm::SYNC_CONFIG).is_none()
                && find(pm::PHASE).is_none(),
            "BitMaker leaves the phase registers"
        );
        assert!(find(pm::COMPENSATION_CONFIG).is_none());
        // 0.94 V core is inside the window; 0.5 V is not.
        let mut vr = Tps546::new(Bus::sink(), Config::NERDAXE_GAIA);
        assert!(vr.set_vout(0.94).is_ok());
        assert_eq!(vr.set_vout(0.5), Err(Error::OutOfRange));
    }

    #[test]
    fn set_vout_writes_command_limits_and_turns_on() {
        let mut vr = Tps546::new(Bus::sink(), Config::BITAXE_5V);
        vr.set_vout(1.15).unwrap();
        let log = vr.release().log;
        // 1.15 × 512 = 588.8 → 588 = 0x024C
        assert_eq!(log[0], Tx::Write(ADDR, vec![pm::VOUT_COMMAND, 0x4C, 0x02]));
        assert_eq!(log.len(), 8);
        assert_eq!(log[7], Tx::Write(ADDR, vec![pm::OPERATION, 0x80]));
    }

    #[test]
    fn set_vout_refuses_outside_the_window_and_zero_turns_off() {
        let mut vr = Tps546::new(Bus::sink(), Config::BITAXE_5V);
        assert_eq!(vr.set_vout(2.5), Err(Error::OutOfRange));
        assert_eq!(vr.set_vout(0.5), Err(Error::OutOfRange));
        vr.set_vout(0.0).unwrap();
        let log = vr.release().log;
        assert_eq!(log, vec![Tx::Write(ADDR, vec![pm::OPERATION, 0x00])]);
    }

    #[test]
    fn reads_decode_the_pmbus_formats() {
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![pm::READ_VIN], vec![0x66, 0xCA]),
            Tx::WriteRead(ADDR, vec![pm::READ_VOUT], vec![0x66, 0x02]),
            Tx::WriteRead(ADDR, vec![pm::READ_IOUT], vec![0x00, 0x00]),
            Tx::WriteRead(ADDR, vec![pm::READ_TEMPERATURE_1], vec![62, 0]),
        ]);
        let mut vr = Tps546::new(bus, Config::BITAXE_5V);
        assert!((vr.vin().unwrap() - 4.8).abs() < 0.01);
        assert!((vr.vout().unwrap() - 1.2).abs() < 0.01);
        assert_eq!(vr.iout().unwrap(), 0.0);
        assert_eq!(vr.temperature_c().unwrap(), 62);
    }
}

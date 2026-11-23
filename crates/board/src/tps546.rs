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
    /// The self-test reads every phase's current on its own
    /// ([`Tps546::phase_currents`]) and wants each above a floor: a
    /// stacked part whose slave phase is dead shows nothing in the
    /// summed telemetry. ESP-Miner does this on every stacked part
    /// (`TPS546_check_phase_currents`, 1 A); here on the boards that have
    /// not run yet (Naja Duo, Gamma Hex), not on the Gamma Turbo and the
    /// Rev 7, whose self-test is proven as it is.
    pub phase_check: bool,
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
        phase_check: false,
    };

    /// NerdOS `drivers/nerdaxe/TPS546` as the NerdAxe Gaia sets it up
    /// (BitMaker fork, `nerdaxegaia.cpp` ae509770): the 5 V driver
    /// defaults with the 12 V input window (on 10.5, off 9.5, UV warn
    /// 10.5, OV fault 14), IOUT 28/33 A, 400 kHz after init, output
    /// floor 0.8 V, PIN_DETECT_OVERRIDE 0x0000, and no STACK / SYNC /
    /// PHASE writes (`write_entire_config`, read again 2026-09-29).
    ///
    /// One deliberate departure (v0.1.16): VOUT_SCALE_LOOP 0.5 instead
    /// of BitMaker's 0.25. The part's reference DAC regulates cleanly
    /// from 0.25 to 0.75 V, so its recommended output range is
    /// 0.25 V / VSL to 0.75 V / VSL (TPS546D24A, SLUSDN0A table 7-1):
    /// 1–3 V at 0.25, 0.5–1.5 V at 0.5. The Gaia runs its BM1373 at
    /// 800–970 mV, below the clean range at 0.25, where TI promises
    /// neither regulation accuracy nor the UV warning/fault (section
    /// 7.3.6 note) — the shape of the unexplained core-voltage drop at
    /// 440 MHz / 970 mV. At 0.5 the whole running range is inside the
    /// clean one; VOUT_MAX becomes 1.4 V, the top of the DAC's valid
    /// VOUT_COMMAND range at that divider (table 7-34). The VSEL strap
    /// table (7-13) gives VSL 0.5 for every boot voltage from 0.5 to
    /// 1.2 V, so a board designed around a ~1 V core most likely strapped
    /// 0.5 and had its MSEL1 compensation chosen for it; the loop gain
    /// scales with VSL (equations 6/7), which is why the compensation
    /// block stays untouched and the pre-init registers are logged at
    /// boot ([`Tps546::window`]) for the check on the board.
    pub const NERDAXE_GAIA: Config = Config {
        vin_on: 10.5,
        vin_off: 9.5,
        vin_uv_warn: 10.5,
        vin_ov_fault: 14.0,
        scale_loop: 0.5,
        vout_min: 0.8,
        vout_max: 1.4,
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
        phase_check: false,
    };

    /// BitForge Nano: one TPS546D24A on the 12 V rail feeding two BM1370
    /// in parallel (schematic, WantClueTechnologies/BitForge-Nano; the
    /// sense lines go straight to the core rail, no external divider).
    /// ForgeOS v1.7 (the Nano's stock firmware, open source:
    /// github.com/wantclue/forge-os, `main/power/vcore.c:17-28`) writes
    /// VIN OV fault 13.5 V and IOUT OC warn 38 A / fault 40 A on every
    /// start; the TPS546D24A is a 40 A part. Taken from there: those
    /// three limits. Kept from before: the Gaia's proven 12 V input
    /// window (on 10.5 / off 9.5, UV warn 10.5), ESP-Miner's 650 kHz,
    /// scale loop 0.25 and 1–2 V window (proven on the same part with
    /// direct sensing), our OT 105/145 °C and the shut-down OC response.
    /// (Until v0.1.35 WarpAxe ran 45 / 52 A from the part's NVM and
    /// 14.8 V, before ForgeOS's source was looked at; 2026-10-23.)
    pub const BITFORGE_NANO: Config = Config {
        vin_on: 10.5,
        vin_off: 9.5,
        vin_uv_warn: 10.5,
        vin_ov_fault: 13.5,
        scale_loop: 0.25,
        vout_min: 1.0,
        vout_max: 2.0,
        vout_command: 1.2,
        // As the manufacturer. The Max preset (625 MHz / 1160 mV) was
        // measured at 37.1 A on average, 37.6 A at most (WarpTune's
        // journal on .44): just under the warning, so it may touch it and,
        // rarely, trip the fault, handled as an overheat stop; ForgeOS's
        // own 636 MHz preset runs under the same 40 A (docs/08).
        iout_oc_warn: 38.0,
        iout_oc_fault: 40.0,
        frequency_khz: 650,
        phase: 0x00,
        stack_config: 0x0000,
        sync_config: 0x10,
        compensation: None,
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
        phase_check: false,
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
        phase_check: false,
    };

    /// ESP-Miner `TPS546_CONFIG_NAJA_DUO` (Bitaxe Naja Duo, 1201): two
    /// stacked phases, two chip voltage domains in series (VOUT = 2 × core).
    /// Current limits as on ESP-Miner master (f5e74b1, #1992; in no
    /// release tag yet): 61/68 A, up from 50/55 A, for the maker's
    /// 600 MHz overclock.
    pub const NAJA_DUO: Config = Config {
        vin_on: 11.0,
        vin_off: 10.5,
        vin_uv_warn: 11.0,
        vin_ov_fault: 14.0,
        scale_loop: 0.125,
        vout_min: 1.8,
        vout_max: 3.0,
        vout_command: 2.2,
        iout_oc_warn: 61.0,
        iout_oc_fault: 68.0,
        frequency_khz: 650,
        phase: 0xFF,
        stack_config: 0x0001,
        sync_config: 0xD0,
        compensation: Some([0x12, 0x34, 0x42, 0x25, 0x04]),
        pin_detect_override: Some(0xFFFF),
        write_phase_config: true,
        phase_check: true,
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
        phase_check: true,
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
        phase_check: false,
    };
}

/// The reference DAC's clean range, V (TPS546D24A SLUSDN0A, "Reference
/// voltage range" and table 7-1): VOUT_COMMAND × VOUT_SCALE_LOOP must
/// stay inside it, or TI promises neither regulation accuracy nor the
/// VOUT UV warning and fault.
pub const REF_MIN_V: f32 = 0.25;
/// Upper end of the reference range, V.
pub const REF_MAX_V: f32 = 0.75;

impl Config {
    /// Lowest output the part regulates cleanly at this divider, V:
    /// 0.25 V / VOUT_SCALE_LOOP.
    pub fn clean_floor(&self) -> f32 {
        REF_MIN_V / self.scale_loop
    }

    /// Highest output the part regulates cleanly at this divider, V:
    /// 0.75 V / VOUT_SCALE_LOOP.
    pub fn clean_ceiling(&self) -> f32 {
        REF_MAX_V / self.scale_loop
    }

    /// Lowest output the driver commands, V: the configured floor, never
    /// below the clean range of the divider.
    pub fn min_vout(&self) -> f32 {
        if self.vout_min > self.clean_floor() {
            self.vout_min
        } else {
            self.clean_floor()
        }
    }

    /// Highest output the driver commands, V: the configured ceiling,
    /// never above the clean range of the divider.
    pub fn max_vout(&self) -> f32 {
        if self.vout_max < self.clean_ceiling() {
            self.vout_max
        } else {
            self.clean_ceiling()
        }
    }
}

// Fixed limits, ESP-Miner TPS546.h.
const VIN_OV_FAULT_RESPONSE: u8 = 0xB7;
const IOUT_OC_FAULT_RESPONSE: u8 = 0xC0; // shut down, no retries
const OT_WARN_C: i32 = 105;
const OT_FAULT_C: i32 = 145;
const OT_FAULT_RESPONSE: u8 = 0xFF; // wait for cooling, retry
const TON_RISE_MS: i32 = 3;
const TON_MAX_FAULT_RESPONSE: u8 = 0x3B;
// VOUT fault, warn and margin limits, ESP-Miner's values written the way
// ESP-Miner writes them: in the VOUT_MODE format like a voltage. The
// part's comparators are percentages of VOUT_COMMAND (OV fault
// 105–140 %, UV fault 60–95 %, 2.5 % steps; TPS546D24A SLUSDN0A
// 7.6.33 / 7.6.37), a written value is mapped to one at write time and
// a value that maps outside the window is NACKed (found on a Gamma:
// 0x40 refused, readback 640 = 1.25). Below 0.95 V output the 0.9 UV
// warn value maps above 95 % and is refused, so a limit write never
// fails [`Tps546::set_vout`]; it is remembered in
// [`Tps546::limit_fault`] instead. Whether to write true ratios is a
// change for every board and a question for the bench, not for v0.1.16.
const OV_FAULT: f32 = 1.25;
const OV_WARN: f32 = 1.16;
const MARGIN_HIGH: f32 = 1.10;
const MARGIN_LOW: f32 = 0.90;
const UV_WARN: f32 = 0.90;
const UV_FAULT: f32 = 0.75;

/// The output window a part holds, as [`Tps546::window`] reads it. Volts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    /// VOUT_SCALE_LOOP, the feedback divider.
    pub scale_loop: Option<f32>,
    /// VOUT_COMMAND.
    pub vout_command: Option<f32>,
    /// VOUT_MIN.
    pub vout_min: Option<f32>,
    /// VOUT_MAX.
    pub vout_max: Option<f32>,
    /// COMPENSATION_CONFIG, the five loop bytes.
    pub compensation: Option<[u8; 5]>,
}

/// What a TPS546 holds instead of the firmware's settings
/// ([`Tps546::settings_lost`]). Volts, and the divider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lost {
    /// VOUT_COMMAND as read.
    pub vout_command: f32,
    /// VOUT_SCALE_LOOP as read.
    pub scale_loop: f32,
}

/// The judgement of [`Tps546::settings_lost`]: VOUT_COMMAND more than two
/// encoding steps from `want_vout`, or VOUT_SCALE_LOOP not the config's.
pub fn lost(cfg: &Config, vout_exp: i32, want_vout: f32, cmd: u16, scale: f32) -> Option<Lost> {
    let want = f32_to_ulinear16(want_vout, vout_exp);
    let off = cmd.abs_diff(want) > 2;
    let rescaled = (scale - cfg.scale_loop).abs() > 0.001;
    (off || rescaled).then(|| Lost {
        vout_command: pm::ulinear16_to_f32(cmd, vout_exp),
        scale_loop: scale,
    })
}

/// The driver, owning the bus.
pub struct Tps546<I> {
    bus: I,
    cfg: Config,
    /// From VOUT_MODE at init; ULINEAR16 exponent.
    vout_exp: i32,
    /// The last PMBus command sent, for a failure report.
    last_cmd: u8,
    /// The first VOUT limit register the part refused in the last
    /// [`set_vout`](Self::set_vout) or [`init`](Self::init), if any.
    limit_fault: Option<u8>,
}

impl<I: I2c> Tps546<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, cfg: Config) -> Self {
        Tps546 {
            bus,
            cfg,
            vout_exp: -9,
            last_cmd: 0,
            limit_fault: None,
        }
    }

    /// The first VOUT fault / warn / margin register the part refused
    /// during the last [`set_vout`](Self::set_vout) or
    /// [`init`](Self::init); `None` when it took them all. The output
    /// voltage itself was commanded either way.
    pub fn limit_fault(&self) -> Option<u8> {
        self.limit_fault
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

    /// COMPENSATION_CONFIG (USER_DATA_01), the five-byte loop block.
    pub fn compensation(&mut self) -> Result<[u8; 5], Error<I::Error>> {
        self.last_cmd = pm::COMPENSATION_CONFIG;
        let mut b = [0u8; 6];
        self.bus
            .write_read(ADDR, &[pm::COMPENSATION_CONFIG], &mut b)?;
        if b[0] != 5 {
            return Err(Error::BadLength);
        }
        let mut out = [0u8; 5];
        out.copy_from_slice(&b[1..]);
        Ok(out)
    }

    /// The output window the part holds right now: VOUT_SCALE_LOOP,
    /// VOUT_COMMAND / MIN / MAX and the compensation block. Read before
    /// [`init`](Self::init) on a cold start, it shows what the VSEL /
    /// MSEL1 straps or the NVM programmed, which is what the board's
    /// compensation was chosen for. Uses the VOUT_MODE exponent the part
    /// reports, so it works before `init` too; a register that fails to
    /// read is `None`.
    pub fn window(&mut self) -> Window {
        let exp = match self.read_byte(pm::VOUT_MODE) {
            Ok(m) => pm::vout_mode_exponent(m),
            Err(_) => self.vout_exp,
        };
        let scale_loop = self
            .read_word(pm::VOUT_SCALE_LOOP)
            .ok()
            .map(pm::linear11_to_f32);
        let mut volts = |cmd: u8| {
            self.read_word(cmd)
                .ok()
                .map(|w| pm::ulinear16_to_f32(w, exp))
        };
        let vout_command = volts(pm::VOUT_COMMAND);
        let vout_min = volts(pm::VOUT_MIN);
        let vout_max = volts(pm::VOUT_MAX);
        Window {
            scale_loop,
            vout_command,
            vout_min,
            vout_max,
            compensation: self.compensation().ok(),
        }
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
        self.write_vout_limits();
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

    /// The fault/warn/margin window, rewritten unchanged after every new
    /// VOUT_COMMAND (as ESP-Miner does). Every register is tried; the
    /// first one the part refuses goes to [`limit_fault`](Self::limit_fault).
    fn write_vout_limits(&mut self) {
        self.limit_fault = None;
        for (cmd, value) in [
            (pm::VOUT_OV_FAULT_LIMIT, OV_FAULT),
            (pm::VOUT_OV_WARN_LIMIT, OV_WARN),
            (pm::VOUT_MARGIN_HIGH, MARGIN_HIGH),
            (pm::VOUT_MARGIN_LOW, MARGIN_LOW),
            (pm::VOUT_UV_WARN_LIMIT, UV_WARN),
            (pm::VOUT_UV_FAULT_LIMIT, UV_FAULT),
        ] {
            if self.write_ulinear16(cmd, value).is_err() && self.limit_fault.is_none() {
                self.limit_fault = Some(cmd);
            }
        }
    }

    /// Set the output and turn conversion on; 0 turns it off. Outside
    /// the configured window, or outside the part's clean range at the
    /// configured divider ([`Config::min_vout`], [`Config::max_vout`]),
    /// nothing is written. An error means the voltage was not commanded
    /// (or conversion not turned on); a limit register the part refused
    /// is not one, see [`limit_fault`](Self::limit_fault).
    pub fn set_vout(&mut self, volts: f32) -> Result<(), Error<I::Error>> {
        if volts == 0.0 {
            return self.write_byte(pm::OPERATION, OPERATION_OFF);
        }
        if volts < self.cfg.min_vout() || volts > self.cfg.max_vout() {
            return Err(Error::OutOfRange);
        }
        self.write_ulinear16(pm::VOUT_COMMAND, volts)?;
        self.write_vout_limits();
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

    /// Whether the part still holds what [`init`](Self::init) and
    /// [`set_vout`](Self::set_vout) wrote: VOUT_COMMAND at `want_vout`
    /// (within two steps of its ULINEAR16 encoding) and VOUT_SCALE_LOOP at
    /// the config's divider. A TPS546 that resets on its own (a brown-out
    /// of its own supply) comes back from its NVM: on the Zyber 8G that
    /// is 4.60 V (1150 mV per chip) and a 6.0 V ceiling, while the
    /// firmware still believes its last value was applied. `Ok(None)`:
    /// both as written.
    pub fn settings_lost(&mut self, want_vout: f32) -> Result<Option<Lost>, Error<I::Error>> {
        let cmd = self.read_word(pm::VOUT_COMMAND)?;
        let scale = pm::linear11_to_f32(self.read_word(pm::VOUT_SCALE_LOOP)?);
        Ok(lost(&self.cfg, self.vout_exp, want_vout, cmd, scale))
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

    /// Phases the part drives: STACK_CONFIG's low three bits plus one,
    /// as ESP-Miner counts them (`TPS546_get_phase_count`); 1 for a part
    /// alone or one whose phase registers this driver does not write.
    pub fn phases(&self) -> u8 {
        if self.cfg.write_phase_config {
            (self.cfg.stack_config & 0x07) as u8 + 1
        } else {
            1
        }
    }

    /// The self-test's phase check is wanted on this board
    /// ([`Config::phase_check`]) and the part has more than one phase.
    pub fn checks_phases(&self) -> bool {
        self.cfg.phase_check && self.phases() > 1
    }

    /// Each phase's output current on its own, A: PHASE set to the
    /// phase, READ_IOUT, for every phase in turn, then PHASE back to the
    /// configured page (0xFF, the sum) whatever happened, as ESP-Miner's
    /// `TPS546_check_phase_currents` does. The telemetry page is all
    /// this moves; conversion is untouched. An error names the command
    /// that failed ([`last_command`](Self::last_command)); the page is
    /// restored before it is returned, and a failed restore wins over
    /// the read's error.
    pub fn phase_currents(&mut self) -> Result<PhaseCurrents, Error<I::Error>> {
        let count = self.phases().min(MAX_PHASES as u8);
        let mut out = PhaseCurrents {
            count,
            amps: [0.0; MAX_PHASES],
        };
        let mut result = Ok(());
        for phase in 0..count {
            match self
                .write_byte(pm::PHASE, phase)
                .and_then(|()| self.read_word(pm::READ_IOUT))
            {
                Ok(raw) => out.amps[usize::from(phase)] = pm::linear11_to_f32(raw),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        self.write_byte(pm::PHASE, self.cfg.phase)?;
        result.map(|()| out)
    }
}

/// Phases a stacked TPS546 can report (STACK_CONFIG's three bits).
pub const MAX_PHASES: usize = 8;

/// One current per phase of a stacked regulator
/// ([`Tps546::phase_currents`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseCurrents {
    /// Phases read.
    pub count: u8,
    /// Each phase's output current, A; only the first `count` are set.
    pub amps: [f32; MAX_PHASES],
}

impl PhaseCurrents {
    /// The currents, one per phase.
    pub fn amps(&self) -> &[f32] {
        &self.amps[..usize::from(self.count).min(MAX_PHASES)]
    }

    /// The weakest phase: its index and current.
    pub fn weakest(&self) -> Option<(u8, f32)> {
        self.amps()
            .iter()
            .enumerate()
            .map(|(i, a)| (i as u8, *a))
            .reduce(|w, c| if c.1 < w.1 { c } else { w })
    }

    /// All phases together, A.
    pub fn total(&self) -> f32 {
        self.amps().iter().sum()
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
    fn settings_lost_sees_the_nvm_values_after_a_reset() {
        let cfg = Config::NERDQAXE_PLUSPLUS_REV7;
        let exp = -9;
        let want = 2.4; // 1200 mV per chip, two in series
        let written = f32_to_ulinear16(want, exp);
        assert_eq!(lost(&cfg, exp, want, written, cfg.scale_loop), None);
        assert_eq!(lost(&cfg, exp, want, written + 2, cfg.scale_loop), None);
        // Back from an NVM value of 1150 mV per chip.
        let nvm = f32_to_ulinear16(2.3, exp);
        let l = lost(&cfg, exp, want, nvm, cfg.scale_loop).expect("lost");
        assert!((l.vout_command - 2.3).abs() < 0.002);
        // The divider alone gives it away too.
        assert!(lost(&cfg, exp, want, written, 0.25).is_some());
    }

    #[test]
    fn settings_lost_reads_the_command_and_the_divider() {
        let exp = -9;
        let cfg = Config::NERDQAXE_PLUSPLUS_REV7;
        let w = f32_to_ulinear16(2.4, exp).to_le_bytes().to_vec();
        let s = f32_to_linear11(cfg.scale_loop).to_le_bytes().to_vec();
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![pm::VOUT_COMMAND], w),
            Tx::WriteRead(ADDR, vec![pm::VOUT_SCALE_LOOP], s),
        ]);
        let mut vr = Tps546::new(bus, cfg);
        vr.vout_exp = exp;
        assert_eq!(vr.settings_lost(2.4), Ok(None));
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

    /// The Nano's limits are ForgeOS v1.7's (`main/power/vcore.c` 17–28),
    /// pinned here since v0.1.36: OC warn 38 A / fault 40 A on the 40 A
    /// part, VIN OV fault 13.5 V.
    #[test]
    fn nano_config_writes_forgeos_current_and_input_limits() {
        let bus = Bus::new(vec![
            id_ok(),
            Tx::Write(ADDR, vec![pm::OPERATION, 0x00]),
            Tx::Write(ADDR, vec![pm::ON_OFF_CONFIG, 0x1B]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_MODE], vec![0x17]),
        ]);
        let mut vr = Tps546::new(bus, Config::BITFORGE_NANO);
        vr.init().unwrap();
        let log = vr.release().log;
        let find = |cmd: u8| {
            log.iter().find_map(|t| match t {
                Tx::Write(_, b) if b[0] == cmd => Some(b[1..].to_vec()),
                _ => None,
            })
        };
        let word = |v: f32| f32_to_linear11(v).to_le_bytes().to_vec();
        assert_eq!(find(pm::IOUT_OC_WARN_LIMIT), Some(word(38.0)));
        assert_eq!(find(pm::IOUT_OC_FAULT_LIMIT), Some(word(40.0)));
        assert_eq!(find(pm::VIN_OV_FAULT_LIMIT), Some(word(13.5)));
        assert_eq!(find(pm::VIN_ON), Some(word(10.5)));
        assert_eq!(find(pm::VIN_OFF), Some(word(9.5)));
        let c = Config::BITFORGE_NANO;
        assert!(c.iout_oc_warn < c.iout_oc_fault);
        assert_eq!(c.iout_oc_fault, 40.0, "the TPS546D24A is a 40 A part");
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
        // VOUT_MIN 0.8 V × 512 = 409 = 0x0199; VOUT_MAX 1.4 V × 512 = 716 = 0x02CC.
        assert_eq!(find(pm::VOUT_MIN), Some(vec![0x99, 0x01]));
        assert_eq!(find(pm::VOUT_MAX), Some(vec![0xCC, 0x02]));
        // VOUT_SCALE_LOOP 0.5: the divider whose clean range holds 0.8–0.97 V.
        let [lo, hi] = f32_to_linear11(0.5).to_le_bytes();
        assert_eq!(find(pm::VOUT_SCALE_LOOP), Some(vec![lo, hi]));
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
        // 0.94 V core is inside the window; 0.5 V and 1.45 V are not.
        let mut vr = Tps546::new(Bus::sink(), Config::NERDAXE_GAIA);
        assert!(vr.set_vout(0.94).is_ok());
        assert_eq!(vr.set_vout(0.5), Err(Error::OutOfRange));
        assert_eq!(vr.set_vout(1.45), Err(Error::OutOfRange));
    }

    #[test]
    fn every_preset_window_lies_inside_the_clean_range_of_its_divider() {
        for (name, c) in [
            ("bitaxe 5 V", Config::BITAXE_5V),
            ("gaia", Config::NERDAXE_GAIA),
            ("nano", Config::BITFORGE_NANO),
            ("gamma turbo", Config::GAMMA_TURBO),
            ("naja duo", Config::NAJA_DUO),
            ("gamma hex", Config::GAMMA_HEX),
            ("rev 7", Config::NERDQAXE_PLUSPLUS_REV7),
        ] {
            assert!(c.min_vout() >= c.clean_floor(), "{name}");
            assert!(c.max_vout() <= c.clean_ceiling(), "{name}");
            assert!(c.min_vout() >= c.vout_min, "{name}");
            assert!(c.max_vout() <= c.vout_max, "{name}");
            assert!(
                c.vout_command >= c.min_vout(),
                "{name}: init point below the floor"
            );
            assert!(
                c.vout_command <= c.max_vout(),
                "{name}: init point above the ceiling"
            );
        }
        assert_eq!(Config::NERDAXE_GAIA.clean_floor(), 0.5);
        assert_eq!(Config::NERDAXE_GAIA.clean_ceiling(), 1.5);
        assert_eq!(Config::NERDAXE_GAIA.min_vout(), 0.8);
        assert_eq!(Config::NERDAXE_GAIA.max_vout(), 1.4);
        assert_eq!(Config::BITAXE_5V.min_vout(), 1.0);
        // ESP-Miner's Naja Duo floor (1.8 V) sits below the clean 2 V of
        // the 0.125 divider: the driver's floor is the clean one.
        assert_eq!(Config::NAJA_DUO.clean_floor(), 2.0);
        assert_eq!(Config::NAJA_DUO.min_vout(), 2.0);
        assert_eq!(Config::GAMMA_TURBO.max_vout(), 3.0);
    }

    #[test]
    fn set_vout_never_goes_below_the_clean_range_of_the_divider() {
        // The Gaia as BitMaker runs it: a 0.8 V floor on the 0.25
        // divider, whose clean range only starts at 1 V.
        let bitmaker = Config {
            scale_loop: 0.25,
            vout_max: 3.0,
            ..Config::NERDAXE_GAIA
        };
        assert_eq!(bitmaker.min_vout(), 1.0);
        assert_eq!(bitmaker.max_vout(), 3.0);
        let mut vr = Tps546::new(Bus::sink(), bitmaker);
        assert_eq!(vr.set_vout(0.94), Err(Error::OutOfRange));
        assert!(vr.set_vout(1.0).is_ok());
    }

    #[test]
    fn window_reads_the_divider_window_and_compensation() {
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![pm::VOUT_MODE], vec![0x17]),
            // The register default 0xC840 = 64 · 2⁻⁷ = 0.5.
            Tx::WriteRead(ADDR, vec![pm::VOUT_SCALE_LOOP], vec![0x40, 0xC8]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_COMMAND], vec![0x66, 0x02]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_MIN], vec![0x99, 0x01]),
            Tx::WriteRead(ADDR, vec![pm::VOUT_MAX], vec![0x00, 0x06]),
            Tx::WriteRead(
                ADDR,
                vec![pm::COMPENSATION_CONFIG],
                vec![5, 0x22, 0x18, 0xC2, 0x1D, 0x06],
            ),
        ]);
        let mut vr = Tps546::new(bus, Config::NERDAXE_GAIA);
        let w = vr.window();
        assert_eq!(w.scale_loop, Some(0.5));
        assert!((w.vout_command.unwrap() - 1.2).abs() < 0.002);
        assert!((w.vout_min.unwrap() - 0.8).abs() < 0.002);
        assert_eq!(w.vout_max, Some(3.0));
        assert_eq!(w.compensation, Some([0x22, 0x18, 0xC2, 0x1D, 0x06]));
        // Nothing written: a probe changes no register.
        assert!(vr
            .release()
            .log
            .iter()
            .all(|t| matches!(t, Tx::WriteRead(..))));
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
    fn a_refused_limit_register_does_not_refuse_the_voltage() {
        // 0.92 V on a Gaia: the 0.9 UV warn value maps above 95 % of
        // VOUT_COMMAND and the part NACKs it. The voltage is set, the
        // other limits are still written, conversion goes on.
        let w = |cmd: u8, v: f32| {
            let [lo, hi] = f32_to_ulinear16(v, -9).to_le_bytes();
            vec![cmd, lo, hi]
        };
        let bus = Bus::new(vec![
            Tx::Write(ADDR, w(pm::VOUT_COMMAND, 0.92)),
            Tx::Write(ADDR, w(pm::VOUT_OV_FAULT_LIMIT, OV_FAULT)),
            Tx::Write(ADDR, w(pm::VOUT_OV_WARN_LIMIT, OV_WARN)),
            Tx::Write(ADDR, w(pm::VOUT_MARGIN_HIGH, MARGIN_HIGH)),
            Tx::Write(ADDR, w(pm::VOUT_MARGIN_LOW, MARGIN_LOW)),
            Tx::NackWrite(ADDR, w(pm::VOUT_UV_WARN_LIMIT, UV_WARN)),
            Tx::Write(ADDR, w(pm::VOUT_UV_FAULT_LIMIT, UV_FAULT)),
            Tx::Write(ADDR, vec![pm::OPERATION, 0x80]),
        ]);
        let mut vr = Tps546::new(bus, Config::NERDAXE_GAIA);
        assert_eq!(vr.set_vout(0.92), Ok(()));
        assert_eq!(vr.limit_fault(), Some(pm::VOUT_UV_WARN_LIMIT));
        let log = vr.release().log;
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

    /// LINEAR11 words for the phase test: 12.25 A and 12.0 A, as a Naja
    /// Duo at 24 A would page them (exponent −2 = 0b11110 in the top
    /// five bits, mantissa 49 and 48 × 0.25; little-endian on the bus).
    const PHASE0_12_25A: [u8; 2] = [0x31, 0xF0];
    const PHASE1_12A: [u8; 2] = [0x30, 0xF0];

    #[test]
    fn phase_currents_page_each_phase_and_restore_the_sum_page() {
        let bus = Bus::new(vec![
            Tx::Write(ADDR, vec![pm::PHASE, 0x00]),
            Tx::WriteRead(ADDR, vec![pm::READ_IOUT], PHASE0_12_25A.to_vec()),
            Tx::Write(ADDR, vec![pm::PHASE, 0x01]),
            Tx::WriteRead(ADDR, vec![pm::READ_IOUT], PHASE1_12A.to_vec()),
            Tx::Write(ADDR, vec![pm::PHASE, 0xFF]),
        ]);
        let mut vr = Tps546::new(bus, Config::NAJA_DUO);
        assert_eq!(vr.phases(), 2);
        assert!(vr.checks_phases());
        let got = vr.phase_currents().unwrap();
        assert_eq!(got.count, 2);
        assert_eq!(got.amps(), &[12.25, 12.0]);
        assert_eq!(got.weakest(), Some((1, 12.0)));
        assert_eq!(got.total(), 24.25);
        let log = vr.release().log;
        assert_eq!(log.len(), 5, "two pages, two reads, the restore");
        assert_eq!(log[4], Tx::Write(ADDR, vec![pm::PHASE, 0xFF]));
    }

    #[test]
    fn a_failed_phase_read_still_restores_the_page_and_reports_the_command() {
        let bus = Bus::new(vec![
            Tx::Write(ADDR, vec![pm::PHASE, 0x00]),
            Tx::Nack(ADDR, vec![pm::READ_IOUT]),
            Tx::Write(ADDR, vec![pm::PHASE, 0xFF]),
        ]);
        let mut vr = Tps546::new(bus, Config::NAJA_DUO);
        assert!(vr.phase_currents().is_err());
        let log = vr.release().log;
        // Phase 1 was never paged: the restore follows the failure at once.
        assert_eq!(log.len(), 3);
        assert_eq!(log[2], Tx::Write(ADDR, vec![pm::PHASE, 0xFF]));
    }

    /// The Hex pages four phases; a 5 V Bitaxe, the Gaia (phase registers
    /// left to the part) and the proven stacked boards are not checked.
    #[test]
    fn only_the_unrun_stacked_boards_check_their_phases() {
        let phases = |c: Config| Tps546::new(Bus::sink(), c).phases();
        let checks = |c: Config| Tps546::new(Bus::sink(), c).checks_phases();
        assert_eq!(phases(Config::NAJA_DUO), 2);
        assert_eq!(phases(Config::GAMMA_HEX), 4);
        assert_eq!(phases(Config::GAMMA_TURBO), 2);
        assert_eq!(phases(Config::NERDQAXE_PLUSPLUS_REV7), 2);
        assert_eq!(phases(Config::BITAXE_5V), 1);
        assert_eq!(phases(Config::NERDAXE_GAIA), 1);
        assert!(checks(Config::NAJA_DUO));
        assert!(checks(Config::GAMMA_HEX));
        assert!(!checks(Config::GAMMA_TURBO));
        assert!(!checks(Config::NERDQAXE_PLUSPLUS_REV7));
        assert!(!checks(Config::BITAXE_5V));
        assert!(!checks(Config::NERDAXE_GAIA));
        let got = Tps546::new(Bus::sink(), Config::GAMMA_HEX)
            .phase_currents()
            .unwrap();
        assert_eq!(got.count, 4);
        assert_eq!(got.amps(), &[0.0; 4]);
        assert_eq!(got.weakest(), Some((0, 0.0)));
    }
}

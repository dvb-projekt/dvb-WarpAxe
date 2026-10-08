//! The BM1370 — Bitaxe Gamma, BitForge Nano, NerdQAxe++, NerdOCTAXE-γ.
//!
//! Every number here is from ESP-Miner `v2.15.2rc0-17`: `bm1370.c` for
//! the init sequence and register constants, `main/device_config.h` for
//! the core counts and option tables. The init is transcribed step for
//! step from `BM1370_init()`, comments included where the C had them;
//! the ones that say "from S21Pro dump" or "present in the S21 XP init"
//! are register writes whose purpose nobody has documented. They are
//! kept because the chip is known to come up with them.
//!
//! Nothing in this file has been confirmed against hardware by this
//! project yet. The first thing to check on a Gamma is the chip-id
//! reply (`AA 55 13 70 00 00 00 00 00 00 10`, see `crc.rs`); the second
//! is that this sequence, sent byte for byte, brings the chip up.

use super::{ChipProfile, Hcn, InitStep};
use crate::pll::PllSpec;
use crate::regs::{reg, FAST_UART_1M, FAST_UART_1M_BAUD};

const D: InitStep = InitStep::Delay { ms: 10 };

/// `BM1370_init()` up to and including the address assignment and the
/// chain-wide register setup.
const INIT_CHAIN: &[InitStep] = &[
    // set version mask, three times
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::VersionMask,
    // read register 00 on all chips → chip-id replies
    InitStep::ReadAll { reg: reg::CHIP_ID },
    InitStep::CountChips,
    // set version mask
    InitStep::VersionMask,
    // Reg_A8
    InitStep::WriteAll {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x00, 0x00],
    },
    // Misc Control — "from S21Pro dump"
    InitStep::WriteAll {
        reg: reg::MISC_CONTROL,
        value: [0xF0, 0x00, 0xC1, 0x00],
    },
    // chain inactive, then split the address space evenly
    InitStep::ChainInactive,
    InitStep::SetAddress,
    // Core Register Control
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8B, 0x00],
    },
    D,
    // Core Register Control, internal ClockDelayCtrlV2 (value 0x10)
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, 0x10],
    },
    D,
    // "Present in the S21 XP init between core register control and analog mux."
    InitStep::WriteAll {
        reg: reg::TICKET_MASK,
        value: [0x00, 0x00, 0x00, 0xFF],
    },
    D,
    // set difficulty mask (from the profile's default difficulty)
    InitStep::TicketMask,
    D,
    // Analog Mux Control
    InitStep::WriteAll {
        reg: reg::ANALOG_MUX_CONTROL,
        value: [0x00, 0x00, 0x00, 0x03],
    },
    // IO Driver Strength
    InitStep::WriteAll {
        reg: reg::IO_DRIVER_STRENGTH,
        value: [0x00, 0x01, 0x11, 0x11],
    },
    // "Present in the older BM1370 C++ init path. Keep this before per-chip
    // core register setup so all chips share the same internal counter/mux state."
    InitStep::WriteAll {
        reg: reg::PLL3_PARAMETER,
        value: [0x5A, 0xA5, 0x5A, 0xA5],
    },
];

/// The per-chip block of `BM1370_init()`, `GROUP_SINGLE` to each address.
const INIT_PER_CHIP: &[InitStep] = &[
    // Reg_A8
    InitStep::WriteChip {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x01, 0xF0],
    },
    // Misc Control
    InitStep::WriteChip {
        reg: reg::MISC_CONTROL,
        value: [0xF0, 0x00, 0xC1, 0x00],
    },
    // Core Register Control
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8B, 0x00],
    },
    D,
    // Core Register Control, ClockDelayCtrlV2
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, 0x10],
    },
    D,
    // Core Register Control
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x82, 0xAA],
    },
    D,
];

/// The tail of `BM1370_init()` before the frequency ramp.
const INIT_POST: &[InitStep] = &[
    // "Some misc settings?"
    InitStep::WriteAll {
        reg: reg::REG_B9,
        value: [0x00, 0x00, 0x44, 0x80],
    },
    // Analog Mux Control — "rumored to control the temp diode"
    InitStep::WriteAll {
        reg: reg::ANALOG_MUX_CONTROL,
        value: [0x00, 0x00, 0x00, 0x02],
    },
    // "duplicate of first command in series"
    InitStep::WriteAll {
        reg: reg::REG_B9,
        value: [0x00, 0x00, 0x44, 0x80],
    },
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8D, 0xEE],
    },
    D,
    // …then do_frequency_transition() and BM1370_set_nonce_space(): runtime.
];

/// The BM1370 profile.
pub const BM1370: ChipProfile = ChipProfile {
    name: "BM1370",
    chip_id: 0x1370,
    chip_id_reply_len: 11,
    response_len: 11,
    host_midstates: 0,
    chip_id_alias: 0,
    address_stride: 0,
    command_address_shift: 0,
    hcn: Hcn::Computed { errata: 2 * 134 },
    cores: 128,
    small_cores: 2040,
    domains: 4,
    rate_register: None,
    domain_hashrate_scale: 1,
    default_difficulty: 256,
    stock: (525_000_000, 1150),
    frequency_options_mhz: &[400, 490, 525, 550, 600, 625, 690],
    voltage_options_mv: &[1000, 1060, 1100, 1150, 1200, 1250],
    pll: PllSpec::bm1370(),
    job_id_step: 24,
    count_window_ms: 500,
    baud_switch_after_ramp_ms: None,
    job_interval_ms: 0,
    fast_uart_reg: reg::FAST_UART_CONFIGURATION,
    fast_uart: (FAST_UART_1M, FAST_UART_1M_BAUD),
    pll_prewrite: None,
    init_chain: INIT_CHAIN,
    init_per_chip: INIT_PER_CHIP,
    init_post: INIT_POST,
};

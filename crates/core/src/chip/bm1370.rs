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
//! Runs on the Bitaxe Gamma, the BitForge Nano, the NerdQAxe++ and the
//! NerdOctaxe-γ since 2026-09.
//!
//! Two inits, one chip. ESP-Miner's `BM1370_init()` changed in 3a09ea0
//! (2026-08-29, "Bitaxe Color: Naja Duo and Gamma Hex support", #1916):
//! the core register 0x00 ("ClockDelayCtrlV2", written as `3C 80 00 80
//! xx`) went from 0x0C ("from S21Pro dump") to 0x10, and two writes were
//! added, `14 00 00 00 FF` before the ticket mask and `54 00 00 00 03`
//! before the driver strength. Every BM1370 firmware before that, and
//! NerdOS to this day (`components/bm1397/bm1370.cpp`, the firmware the
//! Nerd boards ship with), has 0x0C and neither write. [`BM1370`] is the
//! ESP-Miner init for the Bitaxe boards; [`BM1370_NERD`] is NerdOS's for
//! the Nerd carriers, chosen by the board. Made after the NerdOctaxe-γ
//! (.45) kept bringing up one hash domain of one chip at 3 % of its
//! rate on 0x10 with every configuration register reading the same as
//! on a healthy chip (2026-10-09, `docs/04-Octaxe-Warm-Reboot-Latch.md`).

use super::{ChipProfile, Hcn, InitStep};
use crate::pll::PllSpec;
use crate::regs::{reg, FAST_UART_1M, FAST_UART_1M_BAUD};

const D: InitStep = InitStep::Delay { ms: 10 };

/// Core register 0x00, "ClockDelayCtrlV2": ESP-Miner since 3a09ea0
/// (`BM1370_CLOCK_DELAY_CTRL_VALUE`).
const CLOCK_DELAY_ESP_MINER: u8 = 0x10;
/// The same register as NerdOS writes it, and ESP-Miner before 3a09ea0
/// ("from S21Pro dump"; the S21 dump's 0x18 is commented out in both).
const CLOCK_DELAY_NERDOS: u8 = 0x0C;

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
        value: [0x80, 0x00, 0x80, CLOCK_DELAY_ESP_MINER],
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
        value: [0x80, 0x00, 0x80, CLOCK_DELAY_ESP_MINER],
    },
    D,
    // Core Register Control
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x82, 0xAA],
    },
    D,
];

/// NerdOS `BM1370::init` up to the per-chip loop: [`INIT_CHAIN`] with
/// ClockDelayCtrl 0x0C and without the `14 …FF` and `54 …03` writes.
/// NerdOS has no delays between its writes; ESP-Miner's 10 ms are kept
/// (a pause hurts nothing, and the Bitaxe boards come up with them).
/// NerdOS writes the version mask four times at the start and once more
/// at the very end of its init; the extra ones are left out as noise.
const INIT_CHAIN_NERD: &[InitStep] = &[
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::ReadAll { reg: reg::CHIP_ID },
    InitStep::CountChips,
    InitStep::VersionMask,
    InitStep::WriteAll {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x00, 0x00],
    },
    InitStep::WriteAll {
        reg: reg::MISC_CONTROL,
        value: [0xF0, 0x00, 0xC1, 0x00],
    },
    InitStep::ChainInactive,
    InitStep::SetAddress,
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8B, 0x00],
    },
    D,
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, CLOCK_DELAY_NERDOS],
    },
    D,
    InitStep::TicketMask,
    D,
    InitStep::WriteAll {
        reg: reg::IO_DRIVER_STRENGTH,
        value: [0x00, 0x01, 0x11, 0x11],
    },
    InitStep::WriteAll {
        reg: reg::PLL3_PARAMETER,
        value: [0x5A, 0xA5, 0x5A, 0xA5],
    },
];

/// NerdOS's per-chip block: [`INIT_PER_CHIP`] with ClockDelayCtrl 0x0C.
const INIT_PER_CHIP_NERD: &[InitStep] = &[
    InitStep::WriteChip {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x01, 0xF0],
    },
    InitStep::WriteChip {
        reg: reg::MISC_CONTROL,
        value: [0xF0, 0x00, 0xC1, 0x00],
    },
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8B, 0x00],
    },
    D,
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, CLOCK_DELAY_NERDOS],
    },
    D,
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
    init_source: "ESP-Miner",
    init_chain: INIT_CHAIN,
    init_per_chip: INIT_PER_CHIP,
    init_post: INIT_POST,
};

/// The BM1370 as NerdOS brings it up: the Nerd carriers' init (see the
/// module notes). Everything but the init lists is [`BM1370`].
pub const BM1370_NERD: ChipProfile = ChipProfile {
    init_source: "NerdOS",
    init_chain: INIT_CHAIN_NERD,
    init_per_chip: INIT_PER_CHIP_NERD,
    ..BM1370
};

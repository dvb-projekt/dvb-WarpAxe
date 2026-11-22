//! The BM1368 — NerdQAxe+ (four chips); Bitaxe Supra and Supra Hex.
//!
//! Two inits, transcribed step for step, as for the BM1370:
//!
//! * [`BM1368`]: ESP-Miner `v2.15.4` (d212a0f), `components/asic/bm1368.c`
//!   `BM1368_init()`, core counts and option tables from
//!   `main/device_config.h` (`DEVICE_SUPRA`). The Bitaxe Supra's firmware;
//!   no WarpAxe board uses it yet.
//! * [`BM1368_NERD`]: NerdOS `v1.1.2` (3f73884),
//!   `components/bm1397/bm1368.cpp` `BM1368::init()` and the NerdQAxe+
//!   board (`main/boards/nerdqaxeplus.cpp`): what the NerdQAxe+ ships with.
//!
//! What the BM1368 shares with the BM1370: the 11-byte frames, the 82-byte
//! job with the chip rolling the version itself, job ids in steps of 24,
//! the nonce's address bits (`256 / chips`, 64 on four chips in both
//! references), the PLL register layout and its VCO-scale bit, the four
//! domain counters 0x88–0x8B and the 0x28 switch to 1 MBaud.
//!
//! What differs (ESP-Miner, `bm1368.c` against `bm1370.c`): 80 cores and
//! 1276 small cores (BM1370: 128 / 2040); the PLL feedback divider 144–235
//! (BM1370 160–239); no hash-counting erratum; `MISC_CONTROL` broadcast as
//! `FF 0F C1 00`; core register ClockDelayCtrl 0x18; IO driver strength
//! `02 11 11 11`; no PLL3 write and no tail after the per-chip block;
//! 500 ms after each chip's block (ESP-Miner only).
//!
//! The two references differ from each other in the order of the
//! chain-wide writes (ESP-Miner: chain inactive, all seven broadcasts,
//! then the addresses; NerdOS: `A8` and `18`, chain inactive, the
//! addresses, then the rest), in the per-chip pause (500 ms / none), in
//! the hash counting number (computed / NerdOS's fixed 0x15A4, its
//! `vr_frequency` setting) and in when the UART goes fast (ESP-Miner
//! right after its init, ramp included; NerdOS 500 ms after the ramp,
//! like its BM1373). Never run on hardware under WarpAxe: the profile
//! follows the references until a board has shown otherwise.

use super::{ChipProfile, Hcn, InitStep};
use crate::pll::PllSpec;
use crate::regs::{reg, FAST_UART_1M, FAST_UART_1M_BAUD};

/// ESP-Miner's pause after each chip's block, `vTaskDelay(500)`.
const PER_CHIP_PAUSE: InitStep = InitStep::Delay { ms: 500 };

/// Core register 0x00, ClockDelayCtrl, as both references write it to
/// the BM1368 (`3C ← 80 00 80 18`).
const CLOCK_DELAY: u8 = 0x18;

/// `BM1368_init()` up to and including the addresses.
const INIT_CHAIN: &[InitStep] = &[
    // set version mask, four times
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::VersionMask,
    // read register 00 on all chips → chip-id replies
    InitStep::ReadAll { reg: reg::CHIP_ID },
    InitStep::CountChips,
    InitStep::ChainInactive,
    // init_cmds, broadcast
    InitStep::WriteAll {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x00, 0x00],
    },
    InitStep::WriteAll {
        reg: reg::MISC_CONTROL,
        value: [0xFF, 0x0F, 0xC1, 0x00],
    },
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8B, 0x00],
    },
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, CLOCK_DELAY],
    },
    InitStep::WriteAll {
        reg: reg::TICKET_MASK,
        value: [0x00, 0x00, 0x00, 0xFF],
    },
    // Analog Mux
    InitStep::WriteAll {
        reg: reg::ANALOG_MUX_CONTROL,
        value: [0x00, 0x00, 0x00, 0x03],
    },
    InitStep::WriteAll {
        reg: reg::IO_DRIVER_STRENGTH,
        value: [0x02, 0x11, 0x11, 0x11],
    },
    InitStep::SetAddress,
];

/// The per-chip block both references write, `GROUP_SINGLE` to each
/// address.
macro_rules! per_chip {
    ($($tail:expr),*) => {
        &[
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
            InitStep::WriteChip {
                reg: reg::CORE_REGISTER_CONTROL,
                value: [0x80, 0x00, 0x80, CLOCK_DELAY],
            },
            InitStep::WriteChip {
                reg: reg::CORE_REGISTER_CONTROL,
                value: [0x80, 0x00, 0x82, 0xAA],
            },
            $($tail),*
        ]
    };
}

/// ESP-Miner's per-chip block, 500 ms after each chip.
const INIT_PER_CHIP: &[InitStep] = per_chip!(PER_CHIP_PAUSE);

/// After the per-chip pass: the ticket mask, then
/// `do_frequency_transition()`, `BM1368_set_nonce_space()` and the version
/// mask (runtime), then the caller's `BM1368_set_max_baud()`.
const INIT_POST: &[InitStep] = &[InitStep::TicketMask];

/// NerdOS `BM1368::init` up to the per-chip loop. Its fifth version-mask
/// write after the chip count is kept (it comes before `A8`, as the
/// BM1370's does in ESP-Miner).
const INIT_CHAIN_NERD: &[InitStep] = &[
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::VersionMask,
    InitStep::ReadAll { reg: reg::CHIP_ID },
    InitStep::CountChips,
    InitStep::VersionMask,
    // Reg_A8
    InitStep::WriteAll {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x00, 0x00],
    },
    // Misc Control
    InitStep::WriteAll {
        reg: reg::MISC_CONTROL,
        value: [0xFF, 0x0F, 0xC1, 0x00],
    },
    InitStep::ChainInactive,
    InitStep::SetAddress,
    // Core Register Control
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8B, 0x00],
    },
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, CLOCK_DELAY],
    },
    // setJobDifficultyMask
    InitStep::TicketMask,
    // Analog Mux Control
    InitStep::WriteAll {
        reg: reg::ANALOG_MUX_CONTROL,
        value: [0x00, 0x00, 0x00, 0x03],
    },
    // IO Driver Strength
    InitStep::WriteAll {
        reg: reg::IO_DRIVER_STRENGTH,
        value: [0x02, 0x11, 0x11, 0x11],
    },
];

/// NerdOS's per-chip block: the same five writes, no pause.
const INIT_PER_CHIP_NERD: &[InitStep] = per_chip!();

/// NerdOS has nothing between the per-chip loop and the ramp.
const INIT_POST_NERD: &[InitStep] = &[];

/// The BM1368 as ESP-Miner brings it up (the Bitaxe Supra's init).
pub const BM1368: ChipProfile = ChipProfile {
    name: "BM1368",
    chip_id: 0x1368,
    chip_id_reply_len: 11,
    response_len: 11,
    host_midstates: 0,
    chip_id_alias: 0,
    address_stride: 0,
    command_address_shift: 0,
    // `BM1368_set_nonce_space()`: the BM1370 formula without its erratum.
    hcn: Hcn::Computed { errata: 0 },
    cores: 80,
    small_cores: 1276,
    // 0x88–0x8B, as ESP-Miner reads them (`BM1368_read_registers`).
    domains: 4,
    rate_register: None,
    domain_hashrate_scale: 1,
    default_difficulty: 256,
    stock: (490_000_000, 1166),
    frequency_options_mhz: &[400, 425, 450, 475, 485, 490, 500, 525, 550, 575],
    voltage_options_mv: &[1100, 1150, 1166, 1200, 1250, 1300],
    pll: PllSpec::bm1368(),
    job_id_step: 24,
    // Never seen a reply yet: the full second the references allow.
    count_window_ms: 1000,
    // ESP-Miner ramps inside `BM1368_init()` and switches when it returns.
    baud_switch_after_ramp_ms: Some(0),
    job_interval_ms: 0,
    fast_uart_reg: reg::FAST_UART_CONFIGURATION,
    // `BM1368_set_max_baud()`: `28 ← 11 30 02 00`, 1 MBaud.
    fast_uart: (FAST_UART_1M, FAST_UART_1M_BAUD),
    pll_prewrite: None,
    broadcast_reads: false,
    // Not measured on a BM1368: until a board has shown that a write
    // addressed to one chip reaches that chip alone (register read back,
    // its counter following its clock, as on the BM1370 and BM1373),
    // every chip runs at the chain's clock and the API offers no
    // per-chip offsets.
    single_chip_clock: false,
    init_source: "ESP-Miner",
    init_chain: INIT_CHAIN,
    init_per_chip: INIT_PER_CHIP,
    init_post: INIT_POST,
};

/// The BM1368 as NerdOS brings it up on the NerdQAxe+.
pub const BM1368_NERD: ChipProfile = ChipProfile {
    // `BM1368::setVrFrequency(0x15A4)`, NerdOS's default `vr_frequency`
    // on the NerdQAxe+ (the S21 value, bm13xx-rs).
    hcn: Hcn::Fixed(0x0000_15A4),
    // `NerdQaxePlus::initAsics`: `28 ← 11 30 02 00`, 500 ms, host to
    // 1 MBaud, 500 ms.
    baud_switch_after_ramp_ms: Some(500),
    // `m_asicJobIntervalMs` on the NerdQAxe+.
    job_interval_ms: 1200,
    init_source: "NerdOS",
    init_chain: INIT_CHAIN_NERD,
    init_per_chip: INIT_PER_CHIP_NERD,
    init_post: INIT_POST_NERD,
    ..BM1368
};

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(list: &[InitStep]) -> Vec<(u8, [u8; 4])> {
        list.iter()
            .filter_map(|s| match *s {
                InitStep::WriteAll { reg, value } | InitStep::WriteChip { reg, value } => {
                    Some((reg, value))
                }
                _ => None,
            })
            .collect()
    }

    /// Both inits write the same registers, only in another order and
    /// with other pauses: the per-chip block is identical, and the
    /// chain-wide writes are the same set.
    #[test]
    fn the_two_inits_write_the_same_registers() {
        assert_eq!(writes(INIT_PER_CHIP), writes(INIT_PER_CHIP_NERD));
        let mut esp = writes(INIT_CHAIN);
        // ESP-Miner's `14 ← 00 00 00 FF` is NerdOS's ticket mask step.
        esp.retain(|(r, _)| *r != reg::TICKET_MASK);
        let mut nerd = writes(INIT_CHAIN_NERD);
        esp.sort();
        nerd.sort();
        assert_eq!(esp, nerd);
    }

    /// ESP-Miner waits 500 ms after each chip's block; NerdOS does not.
    #[test]
    fn only_esp_miner_pauses_per_chip() {
        assert_eq!(INIT_PER_CHIP.last(), Some(&PER_CHIP_PAUSE));
        assert!(!INIT_PER_CHIP_NERD
            .iter()
            .any(|s| matches!(s, InitStep::Delay { .. })));
    }

    /// ESP-Miner goes inactive before the broadcasts and addresses
    /// last; NerdOS addresses right after going inactive.
    #[test]
    fn chain_order_follows_each_reference() {
        let pos = |list: &[InitStep], f: fn(&InitStep) -> bool| list.iter().position(f).unwrap();
        let inactive = |s: &InitStep| matches!(s, InitStep::ChainInactive);
        let address = |s: &InitStep| matches!(s, InitStep::SetAddress);
        assert_eq!(pos(INIT_CHAIN, address), INIT_CHAIN.len() - 1);
        assert!(pos(INIT_CHAIN, inactive) < pos(INIT_CHAIN, address));
        assert_eq!(
            pos(INIT_CHAIN_NERD, address),
            pos(INIT_CHAIN_NERD, inactive) + 1
        );
    }

    /// The job ids NerdOS lists: 00, 18, 30, 48, 60, 78, 10, 28, …
    #[test]
    fn job_ids_step_by_24() {
        let mut ids = crate::job::JobIds::with_step(BM1368.job_id_step);
        let seq: Vec<u8> = (0..17).map(|_| ids.advance()).collect();
        assert_eq!(&seq[..8], &[0x18, 0x30, 0x48, 0x60, 0x78, 0x10, 0x28, 0x40]);
        assert_eq!(seq[15], 0x00);
        assert_eq!(seq[16], 0x18);
    }

    /// ESP-Miner's 490 MHz default needs the VCO-scale bit
    /// (`50 C4 02 40`), its 400 MHz option does not (`40 A0 02 40`):
    /// values the research worked out from `pll.c`.
    #[test]
    fn pll_words_match_esp_miner() {
        for (mhz, want) in [
            (400, [0x40, 0xA0, 0x02, 0x40]),
            (490, [0x50, 0xC4, 0x02, 0x40]),
            (500, [0x40, 0xA0, 0x02, 0x30]),
            (575, [0x40, 0xB8, 0x02, 0x30]),
        ] {
            let w = crate::pll::solve_mhz(&BM1368.pll, mhz).unwrap();
            assert_eq!(crate::regs::pll_value(&w, &BM1368.pll), want, "{mhz} MHz");
        }
    }

    /// Every option of both references comes out exact.
    #[test]
    fn options_are_exact() {
        for mhz in BM1368
            .frequency_options_mhz
            .iter()
            .chain(&[400u16, 425, 450, 475, 490, 500, 525, 550, 575])
        {
            let w = crate::pll::solve_mhz(&BM1368.pll, u32::from(*mhz)).unwrap();
            assert_eq!(
                w.frequency_hz(BM1368.pll.xtal_hz),
                u64::from(*mhz) * 1_000_000,
                "{mhz}"
            );
        }
    }

    /// ESP-Miner's nonce space for four chips at 490 MHz:
    /// 2³² / 128 / 4 · 25 / 490 · ½ = 213 995.
    #[test]
    fn computed_hash_counting_number() {
        assert_eq!(
            crate::regs::hash_counting_number(490_000_000, BM1368.cores, 4, 0),
            213_995
        );
    }
}

//! The BM1373 — NerdAxe Gaia (Q1373), four chips on 12 V.
//!
//! The init is NerdOS's `BM1373::init()` (`components/bm1397/bm1373.cpp`,
//! branch `develop`, read 2026-09-24), the firmware that runs on the
//! Gaia today: the BM1370 sequence with the version-rolling mask written
//! *without* the enable bit until the end, a fixed address stride of 16,
//! Misc Control `FF 00 C1 00`, and no extra register writes beyond the
//! per-chip block. Chip counts and option tables are NerdOS's Q1373
//! board class. ESP-Miner's `bm1373.c` agrees on the chip id (0x1372,
//! alias 0x1373), the 9-byte chip-id reply, the stride, the command
//! address shift and the fixed hash counting number, but drives a
//! longer init of its own (3 Mbaud, soft reset, core registers, a
//! domain-end IO strength); that list is the fallback if this one does
//! not bring a chain up.
//!
//! Nothing here has been near a chip. The first things to check on a
//! Gaia: the chip-id reply (`AA 55 13 72 …`, nine bytes), that chips
//! answer single-chip commands at address ÷ 4, and that the chain
//! hashes after this sequence.

use super::{ChipProfile, InitStep};
use crate::pll::PllSpec;
use crate::regs::{reg, FAST_UART_1M, FAST_UART_1M_BAUD};

/// `BM1373::init()` up to the chain-wide register setup.
const INIT_CHAIN: &[InitStep] = &[
    // version rolling mask, pre-enumeration, without the enable bit
    InitStep::VersionMaskNoEnable,
    InitStep::VersionMaskNoEnable,
    InitStep::VersionMaskNoEnable,
    InitStep::VersionMaskNoEnable,
    // count_asics()
    InitStep::ReadAll { reg: reg::CHIP_ID },
    InitStep::CountChips,
    InitStep::VersionMaskNoEnable,
    // Reg_A8
    InitStep::WriteAll {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x00, 0x00],
    },
    // Misc Control
    InitStep::WriteAll {
        reg: reg::MISC_CONTROL,
        value: [0xFF, 0x00, 0xC1, 0x00],
    },
    // chain inactive, addresses 0, 16, 32, …
    InitStep::ChainInactive,
    InitStep::SetAddress,
    // Core Register Control
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, 0x0C],
    },
    // setJobDifficultyMask(difficulty)
    InitStep::TicketMask,
    // IO Driver Strength
    InitStep::WriteAll {
        reg: reg::IO_DRIVER_STRENGTH,
        value: [0x00, 0x01, 0x11, 0x11],
    },
    // PLL3 Parameter
    InitStep::WriteAll {
        reg: reg::PLL3_PARAMETER,
        value: [0x5A, 0xA5, 0x5A, 0xA5],
    },
];

/// The per-chip block, `CMD_WRITE_SINGLE` to each chip.
const INIT_PER_CHIP: &[InitStep] = &[
    InitStep::WriteChip {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0x00, 0x07, 0x01, 0xF0],
    },
    InitStep::WriteChip {
        reg: reg::MISC_CONTROL,
        value: [0xFF, 0x00, 0xC1, 0x00],
    },
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, 0x0C],
    },
    InitStep::WriteChip {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x82, 0xAA],
    },
];

/// After the per-chip block: analog mux. The driver then ramps the
/// clock, writes the fixed hash counting number, enables version rolling
/// and switches the UART, in NerdOS's order (`baud_switch_after_ramp_ms`).
const INIT_POST: &[InitStep] = &[
    // Analog Mux Control
    InitStep::WriteAll {
        reg: reg::ANALOG_MUX_CONTROL,
        value: [0x00, 0x00, 0x00, 0x02],
    },
];

/// The BM1373 profile.
pub const BM1373: ChipProfile = ChipProfile {
    name: "BM1373",
    chip_id: 0x1372,
    chip_id_reply_len: 9,
    chip_id_alias: 0x1373,
    address_stride: 16,
    command_address_shift: 2,
    // ESP-Miner: "the generic BM13xx nonce-space calculation makes
    // BM1372/BM1373 chips search overlapping work; this is the known-good
    // S21 Pro stock register value." NerdOS uses the same 0x1EB5.
    hcn_fixed: Some(0x0000_1EB5),
    cores: 128,
    small_cores: 6860,
    domains: 4,
    default_difficulty: 4096,
    stock: (425_000_000, 1010),
    frequency_options_mhz: &[250, 275, 300, 325, 350, 375, 400, 425, 475, 500, 550],
    voltage_options_mv: &[
        980, 990, 1000, 1010, 1020, 1030, 1040, 1050, 1060, 1070, 1080,
    ],
    pll: PllSpec::bm1370(),
    job_id_step: 24,
    hcn_errata: 0,
    // Never seen a reply yet: the full second the reference drivers allow.
    count_window_ms: 1000,
    // A Gaia (2026-09-29) answered the chip-id read but never hashed with
    // the switch before the ramp; NerdOS ramps at 115200 and switches last.
    baud_switch_after_ramp_ms: Some(500),
    fast_uart: (FAST_UART_1M, FAST_UART_1M_BAUD),
    init_chain: INIT_CHAIN,
    init_per_chip: INIT_PER_CHIP,
    init_post: INIT_POST,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::{render, Rendered};

    #[test]
    fn waits_a_full_second_for_chip_ids_and_switches_baud_last() {
        assert_eq!(BM1373.count_window_ms, 1000);
        assert_eq!(crate::chip::bm1370::BM1370.count_window_ms, 500);
        assert_eq!(BM1373.baud_switch_after_ramp_ms, Some(500));
        assert_eq!(crate::chip::bm1370::BM1370.baud_switch_after_ramp_ms, None);
        // Version rolling is enabled by the driver after the ramp, not in
        // the lists: no enable-bit write before the count or after it.
        let enable = crate::regs::version_mask(crate::regs::BIP320_VERSION_MASK);
        let enables = render(&BM1373, 1)
            .filter(|s| match s {
                Rendered::Cmd { payload, .. } => payload[1] == 0xA4 && payload[2..6] == enable,
                Rendered::Delay(_) | Rendered::CountChips => false,
            })
            .count();
        assert_eq!(enables, 0);
    }
    use crate::frame::{Group, Op};

    /// NerdOS `BM1373::init()` for four chips, as `send6` bytes
    /// (group/op, address, register, value), up to the analog mux.
    #[test]
    fn init_renders_nerdos_send6_sequence_for_four_chips() {
        let all = |reg: u8, v: [u8; 4]| Rendered::write(Group::All, 0x00, reg, v);
        let one = |addr: u8, reg: u8, v: [u8; 4]| Rendered::write(Group::Single, addr, reg, v);
        let pre = [0x80, 0x00, 0xFF, 0xFF];
        let mut want = vec![
            all(0xA4, pre),
            all(0xA4, pre),
            all(0xA4, pre),
            all(0xA4, pre),
            Rendered::two(Group::All, Op::Read, 0x00, 0x00),
            Rendered::CountChips,
            all(0xA4, pre),
            all(0xA8, [0x00, 0x07, 0x00, 0x00]),
            all(0x18, [0xFF, 0x00, 0xC1, 0x00]),
            Rendered::two(Group::All, Op::Inactive, 0x00, 0x00),
        ];
        for addr in [0u8, 16, 32, 48] {
            want.push(Rendered::two(Group::Single, Op::SetAddress, addr, 0x00));
        }
        want.push(all(0x3C, [0x80, 0x00, 0x80, 0x0C]));
        // ticket mask for difficulty 4096: 12 leading one bits
        want.push(all(0x14, crate::regs::ticket_mask(4096.0)));
        want.push(all(0x58, [0x00, 0x01, 0x11, 0x11]));
        want.push(all(0x68, [0x5A, 0xA5, 0x5A, 0xA5]));
        for addr in [0u8, 16, 32, 48] {
            // commands go to address ÷ 4
            let a = addr >> 2;
            want.push(one(a, 0xA8, [0x00, 0x07, 0x01, 0xF0]));
            want.push(one(a, 0x18, [0xFF, 0x00, 0xC1, 0x00]));
            want.push(one(a, 0x3C, [0x80, 0x00, 0x80, 0x0C]));
            want.push(one(a, 0x3C, [0x80, 0x00, 0x82, 0xAA]));
        }
        want.push(all(0x54, [0x00, 0x00, 0x00, 0x02]));
        // NerdOS's tail (frequency ramp, 0x10, version rolling on, then
        // the baud switch) is the driver's, after the lists.
        let got: Vec<Rendered> = render(&BM1373, 4).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn addressing_uses_the_fixed_stride_and_the_command_shift() {
        assert_eq!(BM1373.address_interval(4), 16);
        assert_eq!(BM1373.address_interval(1), 16);
        assert_eq!(BM1373.chip_address(4, 3), 48);
        assert_eq!(BM1373.command_address(48), 12);
        assert_eq!(BM1373.chip_index(4, 48), Some(3));
        assert_eq!(BM1373.chip_index(4, 64), None);
        assert_eq!(BM1373.chip_index(4, 8), None);
        // Nonce address bits: 256/4 = 64 per chip, low bits free.
        assert_eq!(BM1373.chip_index_from_nonce(4, 0x00), Some(0));
        assert_eq!(BM1373.chip_index_from_nonce(4, 0x3F), Some(0));
        assert_eq!(BM1373.chip_index_from_nonce(4, 0xC1), Some(3));
        assert_eq!(BM1373.chip_index_from_nonce(1, 0xFF), Some(0));
        assert!(BM1373.is_chip_id(0x1372));
        assert!(BM1373.is_chip_id(0x1373));
        assert!(!BM1373.is_chip_id(0x1370));
    }

    #[test]
    fn gaia_defaults_are_on_the_pll_grid() {
        assert!(crate::pll::solve(&BM1373.pll, BM1373.stock.0).is_some());
        for mhz in BM1373.frequency_options_mhz {
            assert!(
                crate::pll::solve(&BM1373.pll, u32::from(*mhz) * 1_000_000).is_some(),
                "{mhz}"
            );
        }
    }
}

//! The BM1373 — NerdAxe Gaia (Q1373, one chip on 12 V) and Bitaxe Naja
//! Duo (two chips in two voltage domains on 12 V).
//!
//! Two inits, one chip. [`BM1373`] is NerdOS's `BM1373::init()`
//! (`components/bm1397/bm1373.cpp`, branch `develop`, read 2026-09-24),
//! the firmware that runs on the Gaia: the BM1370 sequence with the
//! version-rolling mask written *without* the enable bit until the end,
//! a fixed address stride of 16, Misc Control `FF 00 C1 00`, and no
//! extra register writes beyond the per-chip block. Chip counts and
//! option tables are NerdOS's Q1373 board class. It runs on the Gaia
//! since 2026-09-29.
//!
//! [`BM1373_ESP`] is ESP-Miner's `BM1373_init()`, the firmware of the
//! Naja Duo: it agrees on the chip id (0x1372, alias 0x1373), the
//! 9-byte chip-id reply, the stride, the command address shift and the
//! fixed hash counting number, and drives a longer init (3 MBaud before
//! the ramp, soft reset, core registers, an IO strength for the last
//! chip of each voltage domain, which on the Naja Duo is both chips:
//! chip 0 reaches chip 1 through a discrete level shifter). An A/B on
//! the Gaia (2026-10-10, docs/05) found the same rate and the same
//! error count under both. The Naja Duo gets ESP-Miner's by default,
//! the Gaia keeps NerdOS's; [`BM1373_ESP_1M`] is ESP-Miner's list with
//! NerdOS's late 1 MBaud switch, the fallback should 3 MBaud not hold
//! across the Naja Duo's level shifters. Which board gets which is the
//! firmware's `board::chip()`.

use super::{ChipProfile, Hcn, InitStep};
use crate::pll::PllSpec;
use crate::regs::{reg, FAST_UART_1M, FAST_UART_1M_BAUD, FAST_UART_3M, FAST_UART_3M_BAUD};

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
    response_len: 11,
    host_midstates: 0,
    chip_id_alias: 0x1373,
    address_stride: 16,
    command_address_shift: 2,
    // ESP-Miner: "the generic BM13xx nonce-space calculation makes
    // BM1372/BM1373 chips search overlapping work; this is the known-good
    // S21 Pro stock register value." NerdOS uses the same 0x1EB5.
    hcn: Hcn::Fixed(0x0000_1EB5),
    cores: 128,
    small_cores: 6860,
    // Eight domain counters, 0x88–0x8B and 0xAC–0xAF, each about an
    // eighth of the total (NerdAxe Gaia, 2026-10-01); read all, no ×2.
    domains: 8,
    rate_register: None,
    domain_hashrate_scale: 1,
    default_difficulty: 4096,
    stock: (425_000_000, 1010),
    frequency_options_mhz: &[250, 275, 300, 325, 350, 375, 400, 425, 475, 500, 550],
    voltage_options_mv: &[
        980, 990, 1000, 1010, 1020, 1030, 1040, 1050, 1060, 1070, 1080,
    ],
    pll: PllSpec::bm1370(),
    job_id_step: 24,
    // Never seen a reply yet: the full second the reference drivers allow.
    count_window_ms: 1000,
    // A Gaia (2026-09-29) answered the chip-id read but never hashed with
    // the switch before the ramp; NerdOS ramps at 115200 and switches last.
    baud_switch_after_ramp_ms: Some(500),
    // NerdOS `m_asicJobIntervalMs` on the Gaia.
    job_interval_ms: 1500,
    fast_uart_reg: reg::FAST_UART_CONFIGURATION,
    fast_uart: (FAST_UART_1M, FAST_UART_1M_BAUD),
    pll_prewrite: None,
    init_source: "NerdOS",
    init_chain: INIT_CHAIN,
    init_per_chip: INIT_PER_CHIP,
    init_post: INIT_POST,
};

/// ESP-Miner's `BM1373_init()` (bitaxeorg/ESP-Miner `master` a31e084,
/// `components/asic/bm1373.c`, the init unchanged since 5c27713 of
/// 2026-08-22; the firmware of the Bitaxe Naja Duo), step for step,
/// comments from the C where it had them. Read 2026-10-10; the diff
/// against NerdOS's is `docs/05-BM1373-init-NerdOS-vs-ESP-Miner.md`.
/// ESP-Miner waits `BM1372_INIT_STEP_DELAY_MS` (10 ms) between its
/// steps and 20 ms after the soft reset. The ticket mask is the
/// profile's, as in the NerdOS list (256 on the Naja Duo under ESP-Miner
/// v2.15.4; 4096 here, as on the Gaia). Ran on the Gaia in the A/B of
/// 2026-10-10 (same rate and error count as NerdOS's); the Naja Duo's
/// default ([`BM1373_ESP`]).
const D: InitStep = InitStep::Delay { ms: 10 };

/// `BM1373_init()` up to the chain-wide IO driver strength.
const INIT_CHAIN_ESP: &[InitStep] = &[
    // "Discover the chain at reset baud before changing any ASIC
    // configuration." No version mask first.
    InitStep::ReadAll { reg: reg::CHIP_ID },
    InitStep::CountChips,
    InitStep::ChainInactive,
    D,
    InitStep::SetAddress,
    D,
    // "Match the stock BM1372 setup order: nonce overflow, ticket mask,
    // IO, ring-oscillator pads, then UART and the staged core
    // initialization." Core register 0x0D, nonce bin overflow ctrl:
    // disable (0x04).
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8D, 0x04],
    },
    InitStep::TicketMask,
    InitStep::WriteAll {
        reg: reg::IO_DRIVER_STRENGTH,
        value: [0x00, 0x01, 0x11, 0x11],
    },
];

/// The IO driver strength for the last chip of each voltage domain
/// (`BM1372_IO_DRIVER_STRENGTH_DOMAIN_END`). ESP-Miner writes it to
/// that chip alone; on the Gaia (one chip, one domain) and the Naja Duo
/// (two chips in two domains) that is every chip, which is what the
/// per-chip list reaches. A multi-chip single-domain board would need a
/// domain-aware step.
const INIT_PER_CHIP_ESP: &[InitStep] = &[InitStep::WriteChip {
    reg: reg::IO_DRIVER_STRENGTH,
    value: [0x00, 0x01, 0xF1, 0x11],
}];

/// From the ROSC pad disable to the analog mux: the UART switch, the
/// "fast" soft reset and the staged core registers.
const INIT_POST_ESP: &[InitStep] = &[
    // ROSC pad disable (NerdOS calls it the PLL3 parameter), same word.
    InitStep::WriteAll {
        reg: reg::PLL3_PARAMETER,
        value: [0x5A, 0xA5, 0x5A, 0xA5],
    },
    D,
    // Auto work configuration 0, then the chips to 3 MBaud and the host
    // after them ("Setting ASIC UART to 3000000 baud").
    InitStep::WriteAll {
        reg: reg::AUTO_WORK_CONFIGURATION,
        value: [0x00, 0x00, 0x00, 0x00],
    },
    InitStep::SwitchBaud,
    D,
    // "fast" soft reset, then 20 ms
    InitStep::WriteAll {
        reg: reg::SOFT_RESET_CONTROL,
        value: [0xF0, 0x07, 0x01, 0xF0],
    },
    InitStep::Delay { ms: 20 },
    // Misc control ("RNO enable"): NerdOS's word, broadcast only.
    InitStep::WriteAll {
        reg: reg::MISC_CONTROL,
        value: [0xFF, 0x00, 0xC1, 0x00],
    },
    D,
    // Misc control add ("CRR disable")
    InitStep::WriteAll {
        reg: reg::MISC_CONTROL_ADD,
        value: [0x00, 0x00, 0x00, 0x00],
    },
    D,
    // Core register 0x0B, clock select ctrl: 0
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x8B, 0x00],
    },
    D,
    // Core register 0x00, clock delay ctrl: 0x5A ("PWTH6_CCDLY2";
    // NerdOS writes 0x0C here)
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, 0x5A],
    },
    D,
    // Core register 0x02, core enable
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x82, 0xAA],
    },
    D,
    // Analog mux control: the temperature diode
    InitStep::WriteAll {
        reg: reg::ANALOG_MUX_CONTROL,
        value: [0x00, 0x00, 0x00, 0x02],
    },
    // …then `do_frequency_transition()` from 50 MHz (the first word on
    // the grid is 56.25 MHz, where our ramp starts), the hash counting
    // number and the enabled version mask: the driver's tail.
];

/// The BM1373 as ESP-Miner brings it up (see the notes on
/// [`INIT_CHAIN_ESP`]). Everything but the init lists and the UART rate
/// is [`BM1373`]. The list switches the UART itself
/// ([`InitStep::SwitchBaud`], 3 MBaud); after the lists the driver only
/// ramps, writes the hash counting number and enables version rolling.
/// The Bitaxe Naja Duo's default; on the Gaia only by the `chipInit`
/// setting.
pub const BM1373_ESP: ChipProfile = ChipProfile {
    baud_switch_after_ramp_ms: None,
    fast_uart: (FAST_UART_3M, FAST_UART_3M_BAUD),
    init_source: "ESP-Miner",
    init_chain: INIT_CHAIN_ESP,
    init_per_chip: INIT_PER_CHIP_ESP,
    init_post: INIT_POST_ESP,
    ..BM1373
};

/// [`INIT_POST_ESP`] without its [`InitStep::SwitchBaud`]: the same
/// writes in the same order, the UART left at 115200 for the driver to
/// switch after the ramp, as NerdOS does. Derived, so the two lists
/// cannot drift apart.
const INIT_POST_ESP_1M: [InitStep; INIT_POST_ESP.len() - 1] = without_switch(INIT_POST_ESP);

/// `list` with its one `SwitchBaud` step left out.
const fn without_switch<const N: usize>(list: &[InitStep]) -> [InitStep; N] {
    let mut out = [InitStep::Delay { ms: 0 }; N];
    let (mut i, mut o) = (0, 0);
    while i < list.len() {
        if !matches!(list[i], InitStep::SwitchBaud) {
            assert!(o < N, "more than one SwitchBaud in the list");
            out[o] = list[i];
            o += 1;
        }
        i += 1;
    }
    assert!(o == N, "no SwitchBaud in the list");
    out
}

/// ESP-Miner's init at NerdOS's UART pace: the same register writes as
/// [`BM1373_ESP`], but the chips stay at 115200 through the lists and
/// the ramp and move to 1 MBaud 500 ms after it, as [`BM1373`] does.
/// The fallback for the Naja Duo should 3 MBaud not hold across its two
/// level shifters (TLV3544 between the domains, SN74AXC4T774 to the
/// ESP): the firmware steps down to it when the 3 MBaud init does not
/// bring the chain up, and the `chipInit` setting `espminer-1m` picks it
/// outright. Never the default anywhere.
pub const BM1373_ESP_1M: ChipProfile = ChipProfile {
    baud_switch_after_ramp_ms: Some(500),
    fast_uart: (FAST_UART_1M, FAST_UART_1M_BAUD),
    init_source: "ESP-Miner 1 MBaud",
    init_post: &INIT_POST_ESP_1M,
    ..BM1373_ESP
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::{render, Rendered};

    /// ESP-Miner `BM1373_init()` for one chip (the Gaia), item for item,
    /// up to the analog mux; the ramp, 0x10 and the enabled mask are the
    /// driver's. Delays as the C has them (`vTaskDelay` 10 ms, 20 ms
    /// after the soft reset, none elsewhere).
    #[test]
    fn esp_init_renders_esp_miner_sequence_for_one_chip() {
        use crate::frame::{Group, Op};
        let all = |reg: u8, v: [u8; 4]| Rendered::write(Group::All, 0x00, reg, v);
        let d = Rendered::Delay(10);
        let want = vec![
            Rendered::two(Group::All, Op::Read, 0x00, 0x00),
            Rendered::CountChips,
            Rendered::two(Group::All, Op::Inactive, 0x00, 0x00),
            d,
            Rendered::two(Group::Single, Op::SetAddress, 0x00, 0x00),
            d,
            all(0x3C, [0x80, 0x00, 0x8D, 0x04]),
            all(0x14, crate::regs::ticket_mask(4096.0)),
            all(0x58, [0x00, 0x01, 0x11, 0x11]),
            // the domain-end strength, to chip 0 at command address 0
            Rendered::write(Group::Single, 0x00, 0x58, [0x00, 0x01, 0xF1, 0x11]),
            all(0x68, [0x5A, 0xA5, 0x5A, 0xA5]),
            d,
            all(0xC4, [0x00, 0x00, 0x00, 0x00]),
            all(0x28, [0x80, 0x00, 0x00, 0x00]),
            Rendered::HostBaud(3_000_000),
            d,
            all(0xA8, [0xF0, 0x07, 0x01, 0xF0]),
            Rendered::Delay(20),
            all(0x18, [0xFF, 0x00, 0xC1, 0x00]),
            d,
            all(0x19, [0x00, 0x00, 0x00, 0x00]),
            d,
            all(0x3C, [0x80, 0x00, 0x8B, 0x00]),
            d,
            all(0x3C, [0x80, 0x00, 0x80, 0x5A]),
            d,
            all(0x3C, [0x80, 0x00, 0x82, 0xAA]),
            d,
            all(0x54, [0x00, 0x00, 0x00, 0x02]),
        ];
        let got: Vec<Rendered> = render(&BM1373_ESP, 1).collect();
        assert_eq!(got, want);
        // No version-mask write in the lists at all: ESP-Miner enables
        // rolling once, after the ramp (the driver's tail).
        assert!(got
            .iter()
            .all(|r| r.payload().get(1) != Some(&reg::VERSION_ROLLING)));
    }

    /// The Naja Duo: two chips in two domains, so both get the domain-end
    /// strength, at command addresses 0 and 4 (16 ÷ 4).
    #[test]
    fn esp_init_writes_the_domain_end_strength_to_each_chip() {
        use crate::frame::Group;
        let ends: Vec<Rendered> = render(&BM1373_ESP, 2)
            .filter(|r| {
                r.payload().get(1) == Some(&reg::IO_DRIVER_STRENGTH)
                    && r.payload()[2..] == [0x00, 0x01, 0xF1, 0x11]
            })
            .collect();
        assert_eq!(
            ends,
            vec![
                Rendered::write(Group::Single, 0x00, 0x58, [0x00, 0x01, 0xF1, 0x11]),
                Rendered::write(Group::Single, 0x04, 0x58, [0x00, 0x01, 0xF1, 0x11]),
            ]
        );
    }

    /// Only the init and the UART rate differ from the NerdOS profile.
    #[test]
    fn esp_profile_is_the_bm1373_with_another_init() {
        assert_eq!(BM1373_ESP.init_source, "ESP-Miner");
        assert_eq!(BM1373_ESP.fast_uart, ([0x80, 0x00, 0x00, 0x00], 3_000_000));
        assert_eq!(BM1373_ESP.baud_switch_after_ramp_ms, None);
        assert_eq!(BM1373_ESP.chip_id, BM1373.chip_id);
        assert_eq!(BM1373_ESP.chip_id_alias, BM1373.chip_id_alias);
        assert_eq!(BM1373_ESP.chip_id_reply_len, BM1373.chip_id_reply_len);
        assert_eq!(BM1373_ESP.address_stride, BM1373.address_stride);
        assert_eq!(
            BM1373_ESP.command_address_shift,
            BM1373.command_address_shift
        );
        assert_eq!(BM1373_ESP.hcn, BM1373.hcn);
        assert_eq!(BM1373_ESP.pll, BM1373.pll);
        assert_eq!(BM1373_ESP.count_window_ms, BM1373.count_window_ms);
        assert_eq!(BM1373_ESP.job_interval_ms, BM1373.job_interval_ms);
        assert_eq!(BM1373_ESP.default_difficulty, BM1373.default_difficulty);
        assert_eq!(BM1373_ESP.fast_uart_reg, BM1373.fast_uart_reg);
    }

    /// The 1 MBaud variant: ESP-Miner's writes minus the UART switch,
    /// nothing else, and NerdOS's late switch instead.
    #[test]
    fn esp_1m_profile_is_the_esp_list_without_the_switch() {
        assert_eq!(BM1373_ESP_1M.init_source, "ESP-Miner 1 MBaud");
        assert_eq!(BM1373_ESP_1M.baud_switch_after_ramp_ms, Some(500));
        assert_eq!(BM1373_ESP_1M.fast_uart, BM1373.fast_uart);
        assert_eq!(BM1373_ESP_1M.init_chain, BM1373_ESP.init_chain);
        assert_eq!(BM1373_ESP_1M.init_per_chip, BM1373_ESP.init_per_chip);
        let kept: Vec<InitStep> = INIT_POST_ESP
            .iter()
            .copied()
            .filter(|s| !matches!(s, InitStep::SwitchBaud))
            .collect();
        assert_eq!(BM1373_ESP_1M.init_post, kept.as_slice());
        assert_eq!(BM1373_ESP_1M.init_post.len(), INIT_POST_ESP.len() - 1);
        // Rendered for two chips: the 0x28 write and the host move are
        // gone, every other item is the 3 MBaud list's.
        let fast: Vec<Rendered> = render(&BM1373_ESP, 2)
            .filter(|r| {
                !matches!(r, Rendered::HostBaud(_))
                    && r.payload().get(1) != Some(&reg::FAST_UART_CONFIGURATION)
            })
            .collect();
        let slow: Vec<Rendered> = render(&BM1373_ESP_1M, 2).collect();
        assert_eq!(slow, fast);
        assert!(slow.iter().all(|r| !matches!(r, Rendered::HostBaud(_))));
        assert!(render(&BM1373_ESP, 2).any(|r| r == Rendered::HostBaud(3_000_000)));
    }

    /// A board's own small-core count rides on the chip profile; 0 keeps
    /// the chip's.
    #[test]
    fn with_small_cores_changes_nothing_else() {
        let naja = BM1373_ESP.with_small_cores(6725);
        assert_eq!(naja.small_cores, 6725);
        assert_eq!(
            ChipProfile {
                small_cores: BM1373_ESP.small_cores,
                ..naja.clone()
            },
            BM1373_ESP
        );
        assert_eq!(BM1373.clone().with_small_cores(0), BM1373);
    }

    #[test]
    fn waits_a_full_second_for_chip_ids_and_switches_baud_last() {
        assert_eq!(BM1373.count_window_ms, 1000);
        assert_eq!(crate::chip::bm1370::BM1370.count_window_ms, 500);
        assert_eq!(BM1373.baud_switch_after_ramp_ms, Some(500));
        assert_eq!(BM1373.job_interval_ms, 1500);
        assert_eq!(crate::chip::bm1370::BM1370.job_interval_ms, 0);
        assert_eq!(crate::chip::bm1370::BM1370.baud_switch_after_ramp_ms, None);
        // Version rolling is enabled by the driver after the ramp, not in
        // the lists: no enable-bit write before the count or after it.
        let enable = crate::regs::version_mask(crate::regs::BIP320_VERSION_MASK);
        let enables = render(&BM1373, 1)
            .filter(|s| match s {
                Rendered::Cmd { payload, .. } => payload[1] == 0xA4 && payload[2..6] == enable,
                _ => false,
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

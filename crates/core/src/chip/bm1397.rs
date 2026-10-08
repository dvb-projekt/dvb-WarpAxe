//! The BM1397 — NerdNOS (one chip on a T-Display-S3 carrier), Bitaxe Max.
//!
//! The init is WantClue's `BM1397_init()` / `_send_init()`
//! (NerdMiner_v2, branch `nerdnos`, `src/drivers/nerd-nos/bm1397.cpp`,
//! read 2026-10-08), the firmware that runs on Oliver's NerdNOS (.46)
//! today; ESP-Miner's `bm1397.c` (v2.15.3, the Bitaxe Max) sends the same
//! registers in the same order and adds one write (`MISC_CONTROL` ←
//! `00 00 7A 31`, the default UART divider, before the ramp). Both derive
//! from cgminer's `driver-gekko.c`. Every constant below names where it
//! came from.
//!
//! What is different from the BM1370 family, and where it lives:
//!
//! * nine-byte responses for everything (`response_len`), no version
//!   bytes in a nonce; the chip does not roll versions, the host sends
//!   four midstates per job ([`crate::job::MidstateJob`],
//!   `host_midstates`) and the nonce says which one hit;
//! * the UART rate is set in `MISC_CONTROL` (0x18), not 0x28; the switch
//!   comes after the clock is set, with a 100 ms pause before the host
//!   follows (WantClue `nerdnos_init`: `BM1397_set_max_baud`, 100 ms,
//!   `SERIAL_set_baud`, 100 ms);
//! * no hash counting number, no version-rolling mask, no per-chip pass;
//! * the PLL word follows cgminer's `calc_gsf_freq` ([`PllSpec::bm1397`])
//!   and `0x70 ← 0F 0F 0F 00` goes out before it (`pll_prewrite`).
//!
//! Nothing here has been near a chip under WarpAxe. The first things to
//! check on the .46: the chip-id reply (`AA 55 13 97 18 00 …`, nine
//! bytes), that the chain hashes after this sequence at 300 MHz, and
//! that the nonces verify against the rolled version.

use super::{ChipProfile, Hcn, InitStep};
use crate::pll::PllSpec;
use crate::regs::{reg, BM1397_FAST_UART, BM1397_FAST_UART_BAUD};

/// WantClue `_send_init()`: count, inactive, addresses, six register
/// writes, then (in the driver) the clock.
const INIT_CHAIN: &[InitStep] = &[
    // _send_read_address(): CMD_READ 00 to all; chip_id replies
    InitStep::ReadAll { reg: reg::CHIP_ID },
    InitStep::CountChips,
    // vTaskDelay(SLEEP_TIME) = 20 ms
    InitStep::Delay { ms: 20 },
    // _send_chain_inactive(), one _set_chip_address per chip (i · 256/n)
    InitStep::ChainInactive,
    InitStep::SetAddress,
    // init1 - clock_order_control0
    InitStep::WriteAll {
        reg: reg::CLOCK_ORDER_CONTROL_0,
        value: [0x00, 0x00, 0x00, 0x00],
    },
    // init2 - clock_order_control1
    InitStep::WriteAll {
        reg: reg::CLOCK_ORDER_CONTROL_1,
        value: [0x00, 0x00, 0x00, 0x00],
    },
    // init3 - ordered_clock_enable
    InitStep::WriteAll {
        reg: reg::ORDERED_CLOCK_ENABLE,
        value: [0x00, 0x00, 0x00, 0x01],
    },
    // init4 - init_4_? (core register control)
    InitStep::WriteAll {
        reg: reg::CORE_REGISTER_CONTROL,
        value: [0x80, 0x00, 0x80, 0x74],
    },
    // BM1397_set_job_difficulty_mask(BM1397_INITIAL_DIFFICULTY); ESP-Miner:
    // the family difficulty, 256 on the Max
    InitStep::TicketMask,
    // init5 - pll3_parameter
    InitStep::WriteAll {
        reg: reg::PLL3_PARAMETER,
        value: [0xC0, 0x70, 0x01, 0x11],
    },
    // init6 - fast_uart_configuration
    InitStep::WriteAll {
        reg: reg::FAST_UART_CONFIGURATION,
        value: [0x06, 0x00, 0x00, 0x0F],
    },
    // …then BM1397_send_hash_frequency(frequency): the driver's ramp.
];

/// The BM1397 profile.
pub const BM1397: ChipProfile = ChipProfile {
    name: "BM1397",
    chip_id: 0x1397,
    // WantClue `chip_id[] = {0xAA, 0x55, 0x13, 0x97, 0x18, 0x00}`,
    // ESP-Miner `BM1397_CHIP_ID_RESPONSE_LENGTH 9`.
    chip_id_reply_len: 9,
    response_len: 9,
    // ESP-Miner `ASIC_BM1397.software_midstates = 4`, WantClue
    // `num_midstates = 4`.
    host_midstates: 4,
    chip_id_alias: 0,
    address_stride: 0,
    command_address_shift: 0,
    hcn: Hcn::None,
    // ESP-Miner `main/device_config.h`: core_count 168, small_core_count
    // 672, hash_domains 1. No domain counters are read; the chip reports
    // a rate in register 0x04 (ESP-Miner `REGISTER_HASHRATE`, WantClue
    // `BM1397_read_hashrate` every 2.5 s), the error counter is 0x4C.
    cores: 168,
    small_cores: 672,
    domains: 0,
    rate_register: Some(reg::BM1397_HASHRATE),
    domain_hashrate_scale: 1,
    default_difficulty: 256,
    // NerdNOS `BM1397_init(300, 1)` on a core rail the TPSM863257 sets by
    // resistors to 1.4 V (R12 40.2k / R13 30k, 0.6 V reference). ESP-Miner
    // runs the Max at 425 MHz / 1400 mV.
    stock: (300_000_000, 1400),
    // Around stock, capped where nothing has been measured: the board is
    // USB-powered without a fan. ESP-Miner's Max list starts at 400.
    frequency_options_mhz: &[200, 225, 250, 275, 300, 325, 350, 375, 400],
    // The NerdNOS has no adjustable regulator; the Max's AxeOS list is
    // 1100–1500.
    voltage_options_mv: &[],
    pll: PllSpec::bm1397(),
    // `job.job_id = (job_id * 4) % 128` (WantClue), `id = (id + 4) % 128`
    // (ESP-Miner): ids in steps of 4.
    job_id_step: 4,
    // Both references wait up to a second per chip-id reply.
    count_window_ms: 1000,
    // WantClue `nerdnos_init`: set_max_baud, 100 ms, host follows.
    baud_switch_after_ramp_ms: Some(100),
    // WantClue `NERDNOS_JOB_INTERVAL_MS 30`; ESP-Miner's Max
    // `default_asic_timeout` is 20 ms. Four versions × 2³² nonces are
    // gone in about 85 ms at 300 MHz.
    job_interval_ms: 30,
    fast_uart_reg: reg::MISC_CONTROL,
    fast_uart: (BM1397_FAST_UART, BM1397_FAST_UART_BAUD),
    // cgminer `prefreq`: `00 70 0F 0F 0F 00`, twice, before the PLL word.
    pll_prewrite: Some((reg::PLL0_DIVIDER, [0x0F, 0x0F, 0x0F, 0x00])),
    init_chain: INIT_CHAIN,
    init_per_chip: &[],
    init_post: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::{render, Rendered};
    use crate::frame::{Group, Op};

    /// WantClue `_send_init()` for one chip, frame by frame, as `send6`
    /// bytes (address, register, value).
    #[test]
    fn init_renders_wantclue_send_init_for_one_chip() {
        let all = |reg: u8, v: [u8; 4]| Rendered::write(Group::All, 0x00, reg, v);
        let want = vec![
            Rendered::two(Group::All, Op::Read, 0x00, 0x00),
            Rendered::CountChips,
            Rendered::Delay(20),
            Rendered::two(Group::All, Op::Inactive, 0x00, 0x00),
            Rendered::two(Group::Single, Op::SetAddress, 0x00, 0x00),
            all(0x80, [0x00, 0x00, 0x00, 0x00]),
            all(0x84, [0x00, 0x00, 0x00, 0x00]),
            all(0x20, [0x00, 0x00, 0x00, 0x01]),
            all(0x3C, [0x80, 0x00, 0x80, 0x74]),
            all(0x14, [0x00, 0x00, 0x00, 0xFF]),
            all(0x68, [0xC0, 0x70, 0x01, 0x11]),
            all(0x28, [0x06, 0x00, 0x00, 0x0F]),
        ];
        let got: Vec<Rendered> = render(&BM1397, 1).collect();
        assert_eq!(got, want);
        // No version-rolling mask anywhere: the chip does not roll.
        assert!(!got
            .iter()
            .any(|r| r.payload().get(1) == Some(&reg::VERSION_ROLLING)));
    }

    /// Two chips: addresses 0 and 128 (`i * (256 / asic_count)`), the
    /// register writes once, to all.
    #[test]
    fn two_chips_split_the_address_space_and_share_the_writes() {
        let got: Vec<Rendered> = render(&BM1397, 2).collect();
        let set: Vec<u8> = got
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Rendered::Cmd {
                        op: Op::SetAddress,
                        ..
                    }
                )
            })
            .map(|r| r.payload()[0])
            .collect();
        assert_eq!(set, [0, 128]);
        let singles = got
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Rendered::Cmd {
                        group: Group::Single,
                        op: Op::Write,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(singles, 0, "no per-chip block");
        assert_eq!(BM1397.chip_address(2, 1), 128);
        assert_eq!(BM1397.chip_index_from_nonce(1, 0x5A), Some(0));
    }

    #[test]
    fn profile_numbers() {
        let p = &BM1397;
        assert_eq!(p.chip_id, 0x1397);
        assert_eq!((p.chip_id_reply_len, p.response_len), (9, 9));
        assert_eq!(p.host_midstates, 4);
        assert_eq!(p.hcn, Hcn::None);
        assert_eq!((p.cores, p.small_cores, p.domains), (168, 672, 0));
        assert_eq!(p.rate_register, Some(0x04));
        assert_eq!(p.stock, (300_000_000, 1400));
        assert_eq!(p.job_id_step, 4);
        assert_eq!(p.fast_uart_reg, reg::MISC_CONTROL);
        assert_eq!(p.fast_uart, ([0x00, 0x00, 0x61, 0x31], 1_562_500));
        assert_eq!(p.baud_switch_after_ramp_ms, Some(100));
        assert_eq!(p.job_interval_ms, 30);
        assert_eq!(p.pll_prewrite, Some((0x70, [0x0F, 0x0F, 0x0F, 0x00])));
        assert!(p.voltage_options_mv.is_empty());
        assert_eq!(p.frequency_options_mhz.last(), Some(&400));
    }

    /// The job ids as both drivers hand them out: 4, 8, …, 124, 0, 4.
    #[test]
    fn job_ids_step_by_four() {
        let mut ids = crate::job::JobIds::with_step(BM1397.job_id_step);
        let seq: Vec<u8> = (0..33).map(|_| ids.advance()).collect();
        assert_eq!(seq[0], 4);
        assert_eq!(seq[30], 124);
        assert_eq!(seq[31], 0);
        assert_eq!(seq[32], 4);
        assert!(seq.iter().all(|i| i % 4 == 0 && *i < 128));
    }

    /// Every option is on the gekko grid exactly (multiples of 25 MHz
    /// over the band's divider).
    #[test]
    fn frequency_options_are_exact_on_the_gekko_grid() {
        for mhz in BM1397.frequency_options_mhz {
            let w = crate::pll::solve_mhz(&BM1397.pll, u32::from(*mhz)).unwrap();
            assert_eq!(
                w.frequency_hz(BM1397.pll.xtal_hz),
                u64::from(*mhz) * 1_000_000,
                "{mhz}"
            );
        }
    }
}

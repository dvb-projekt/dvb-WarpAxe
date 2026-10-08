//! Register addresses and the payloads written to them.
//!
//! A register write on the BM13xx is a command frame whose payload is
//! `chip_addr | register | value[4]`; a read is `chip_addr | register`.
//! The value encodings below are the ones the reference driver uses for
//! the BM1366/68/70/73 — shared across the family, with the chip-specific
//! numbers (which register, which constant) living in the chip profile.
//!
//! Source: ESP-Miner `bm1370.c` (`BM1370_set_version_mask`,
//! `BM1370_set_max_baud`, `BM1370_set_nonce_space`,
//! `BM1370_send_hash_frequency`) and `asic_common.c`
//! (`get_difficulty_mask`, `calculate_bm_timeout_ms`). Vectors from
//! `test_version_rolling.c` and the `bm1370.c` init sequence.

use crate::pll::{PllSpec, PllWord};

/// Register addresses on the BM1370. Names follow the reference driver
/// where it has them; the rest are what the init sequence writes to.
pub mod reg {
    /// Chip id; read with `GROUP_ALL` to enumerate the chain.
    pub const CHIP_ID: u8 = 0x00;
    /// BM1397 hash-rate register (ESP-Miner `REGISTER_HASHRATE`,
    /// WantClue `BM1397_read_hashrate`): bit 31 long/short window, bits
    /// 30..0 the rate in units of 2²⁰ hashes per second.
    pub const BM1397_HASHRATE: u8 = 0x04;
    /// BM1397 ordered-clock enable (`ORDERED_CLOCK_ENABLE`, init writes
    /// `00 00 00 01`).
    pub const ORDERED_CLOCK_ENABLE: u8 = 0x20;
    /// BM1397 PLL0 divider (`prefreq`, written `0F 0F 0F 00` before every
    /// PLL0 parameter write; cgminer `calc_gsf_freq`).
    pub const PLL0_DIVIDER: u8 = 0x70;
    /// BM1397 clock order control 0 (init writes zero).
    pub const CLOCK_ORDER_CONTROL_0: u8 = 0x80;
    /// BM1397 clock order control 1 (init writes zero).
    pub const CLOCK_ORDER_CONTROL_1: u8 = 0x84;
    /// Hash-clock PLL parameters: `[vco_scale, fb, refdiv, postdivs]`.
    pub const PLL0_PARAMETER: u8 = 0x08;
    /// Hash counting number: how far each core rolls the nonce.
    pub const HASH_COUNTING_NUMBER: u8 = 0x10;
    /// Ticket mask: the difficulty a nonce must meet to be reported.
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc control.
    pub const MISC_CONTROL: u8 = 0x18;
    /// UART baud-rate configuration.
    pub const FAST_UART_CONFIGURATION: u8 = 0x28;
    /// Core register access (indirect: core id and value in the payload).
    pub const CORE_REGISTER_CONTROL: u8 = 0x3C;
    /// Error counter (read-only).
    pub const ERROR_COUNT: u8 = 0x4C;
    /// Analog mux control; `…02` selects the temperature diode.
    pub const ANALOG_MUX_CONTROL: u8 = 0x54;
    /// I/O driver strength.
    pub const IO_DRIVER_STRENGTH: u8 = 0x58;
    /// PLL3 parameters; the init writes a `5A A5 5A A5` pattern here.
    pub const PLL3_PARAMETER: u8 = 0x68;
    /// Per-domain hash counters, read-only, 0x88..=0x8B; one step per
    /// 2^32 hashes.
    pub const DOMAIN_0_COUNT: u8 = 0x88;
    /// Domain 1.
    pub const DOMAIN_1_COUNT: u8 = 0x89;
    /// Domain 2.
    pub const DOMAIN_2_COUNT: u8 = 0x8A;
    /// Domain 3.
    pub const DOMAIN_3_COUNT: u8 = 0x8B;
    /// Total nonce counter, read-only.
    pub const TOTAL_COUNT: u8 = 0x8C;
    /// Domains 4–7 of the BM1373, which has eight: found by a register
    /// scan on a NerdAxe Gaia, 2026-10-01 — each of 0x88–0x8B and
    /// 0xAC–0xAF counts about an eighth of `TOTAL_COUNT`. No reference
    /// firmware reads them; ESP-Miner doubles the four it reads instead.
    pub const DOMAIN_4_COUNT: u8 = 0xAC;
    /// Domain 5.
    pub const DOMAIN_5_COUNT: u8 = 0xAD;
    /// Domain 6.
    pub const DOMAIN_6_COUNT: u8 = 0xAE;
    /// Domain 7.
    pub const DOMAIN_7_COUNT: u8 = 0xAF;

    /// The counter register of domain `d` (0–7).
    pub const fn domain_count(d: u8) -> u8 {
        if d < 4 {
            DOMAIN_0_COUNT + d
        } else {
            DOMAIN_4_COUNT + (d - 4)
        }
    }

    /// The domain a counter register belongs to, if it is one.
    pub const fn domain_of(register: u8) -> Option<u8> {
        if register >= DOMAIN_0_COUNT && register <= DOMAIN_3_COUNT {
            Some(register - DOMAIN_0_COUNT)
        } else if register >= DOMAIN_4_COUNT && register <= DOMAIN_7_COUNT {
            Some(4 + register - DOMAIN_4_COUNT)
        } else {
            None
        }
    }
    /// Version rolling: `[0x90, 0x00, mask_hi, mask_lo]`.
    pub const VERSION_ROLLING: u8 = 0xA4;
    /// Written early in init (`00 07 00 00`, then `00 07 01 F0` per chip);
    /// `bm1373.c` calls it SOFT_RESET_CONTROL.
    pub const SOFT_RESET_CONTROL: u8 = 0xA8;
    /// Written twice in init with `00 00 44 80`; purpose unknown.
    pub const REG_B9: u8 = 0xB9;
}

/// Payload of a register write: `chip_addr | register | value`.
pub const fn write_payload(chip_addr: u8, register: u8, value: [u8; 4]) -> [u8; 6] {
    [chip_addr, register, value[0], value[1], value[2], value[3]]
}

/// Payload of a register read: `chip_addr | register`.
pub const fn read_payload(chip_addr: u8, register: u8) -> [u8; 2] {
    [chip_addr, register]
}

/// Mirror the bits of a byte (`0x01` ↔ `0x80`). The ticket mask goes on
/// the wire bit-reversed per byte.
pub const fn reverse_bits(b: u8) -> u8 {
    b.reverse_bits()
}

/// Value for [`reg::VERSION_ROLLING`]: enable rolling of the mask's
/// bits. The chip rolls sixteen bits, 13..28 of the header version, so
/// the mask is shifted down by 13 and anything outside those sixteen
/// bits is discarded. `0x90 00` in the first two bytes is the enable.
pub const fn version_mask(mask: u32) -> [u8; 4] {
    let rolled = ((mask >> 13) & 0xFFFF) as u16;
    let b = rolled.to_be_bytes();
    [0x90, 0x00, b[0], b[1]]
}

/// The same mask without the enable bit: `0x80 00` — the BM1373 gets
/// it before enumeration and the enabled form after init (NerdOS).
pub const fn version_mask_no_enable(mask: u32) -> [u8; 4] {
    let mut b = version_mask(mask);
    b[0] = 0x80;
    b
}

/// The mask BIP 320 reserves for version rolling: bits 13..28.
pub const BIP320_VERSION_MASK: u32 = 0x1fff_e000;

/// Value for [`reg::TICKET_MASK`] for a share difficulty.
///
/// The mask must be a contiguous run of ones, so the difficulty is
/// rounded *down* to a power of two (after rounding a fractional
/// difficulty up to an integer), minus one; each byte is bit-reversed
/// on the wire. Difficulty 256 → `00 00 00 FF`, 512 → `00 00 80 FF`.
pub fn ticket_mask(difficulty: f64) -> [u8; 4] {
    // ceil(), by hand: no libm in no_std. Difficulties are positive.
    let whole = difficulty as u32;
    let diff_int = if (whole as f64) < difficulty {
        whole + 1
    } else {
        whole
    };
    let mask = ticket_mask_int(diff_int);
    let b = mask.to_be_bytes();
    [
        reverse_bits(b[0]),
        reverse_bits(b[1]),
        reverse_bits(b[2]),
        reverse_bits(b[3]),
    ]
}

/// The un-reversed mask for an integer difficulty: largest power of two
/// not above it, minus one.
pub const fn ticket_mask_int(difficulty: u32) -> u32 {
    if difficulty <= 1 {
        return 0;
    }
    let power = 31 - difficulty.leading_zeros();
    (1u32 << power) - 1
}

/// The difficulty a ticket-mask register value stands for: un-reverse
/// the bytes, and the mask plus one is the (power-of-two) difficulty.
/// What the simulator uses to know when a nonce is "found".
pub const fn ticket_mask_to_difficulty(value: [u8; 4]) -> u32 {
    let mask = u32::from_be_bytes([
        reverse_bits(value[0]),
        reverse_bits(value[1]),
        reverse_bits(value[2]),
        reverse_bits(value[3]),
    ]);
    mask.wrapping_add(1)
}

/// Value for [`reg::FAST_UART_CONFIGURATION`] that the reference driver
/// writes to move the BM1370 to 1 MBaud. (Its comment says "divider of
/// 0 for 3,125,000" and its log line says 1000000; the driver then sets
/// the host UART to 1000000, so that is the number that is known to
/// work.)
pub const FAST_UART_1M: [u8; 4] = [0x11, 0x30, 0x02, 0x00];
/// The host baud rate to switch to after writing [`FAST_UART_1M`].
pub const FAST_UART_1M_BAUD: u32 = 1_000_000;

/// Value for the BM1397's [`reg::MISC_CONTROL`] that sets its UART
/// divider: `baud = 25 MHz / ((divider + 1) · 8)`, the divider in the
/// low five bits of the third byte (bits 9–13 of the register), the
/// rest as the reference writes it (`00 00 6x 31`; the default of 26
/// gives 115 749). WantClue NerdNOS `BM1397_set_max_baud` uses divider
/// 1 (1 562 500), ESP-Miner divider 0 (3 125 000).
pub const fn bm1397_misc_control_baud(divider: u8) -> [u8; 4] {
    [0x00, 0x00, 0x60 | (divider & 0x1F), 0x31]
}

/// The baud rate a BM1397 divider selects.
pub const fn bm1397_baud(divider: u8) -> u32 {
    25_000_000 / ((divider as u32 + 1) * 8)
}

/// Hashes per count of the BM1397's hash-rate register (ESP-Miner
/// `HASHRATE_UNIT 0x100000`, WantClue `<< 20llu`).
pub const BM1397_HASHRATE_UNIT: u64 = 1 << 20;

/// What the BM1397 reports in register 0x04 (`BM1397_HASHRATE`): the
/// rate in hashes per second and whether it is the long window (bit 31
/// set) or the short one. `None` for the value 0x007FFFFF, which
/// ESP-Miner treats as "no reading" (`update_hashrate`); WantClue logs
/// both windows, ESP-Miner uses the short one.
pub const fn bm1397_hashrate(value: u32) -> Option<(u64, bool)> {
    let count = value & 0x7FFF_FFFF;
    if count == 0x007F_FFFF {
        return None;
    }
    Some((
        count as u64 * BM1397_HASHRATE_UNIT,
        value & 0x8000_0000 != 0,
    ))
}

/// The register value a BM1397 would report for a rate, for the
/// simulator: the count in units of 2²⁰, the window in bit 31.
pub const fn bm1397_hashrate_value(hashes_per_s: u64, long_window: bool) -> u32 {
    let count = (hashes_per_s / BM1397_HASHRATE_UNIT) as u32 & 0x7FFF_FFFF;
    if long_window {
        count | 0x8000_0000
    } else {
        count
    }
}

/// The BM1397 fast rate as NerdNOS runs it: divider 1.
pub const BM1397_FAST_UART: [u8; 4] = bm1397_misc_control_baud(1);
/// Its host baud rate, 1 562 500.
pub const BM1397_FAST_UART_BAUD: u32 = bm1397_baud(1);

/// Hash counting number for the whole nonce space at 100 %.
///
/// The chip rolls `2³² / cores_up / chips_up` nonces per job on each
/// core; at `freq` that takes `space / freq` seconds, and the register
/// counts in units of half the reference clock (`25 MHz · ½`). An
/// erratum on the BM1370 needs a margin of `2 · 134` counts or nonces
/// come back twice. `cores_up` and `chips_up` are the next powers of two.
///
/// Exact integer version of `BM1370_set_nonce_space(1.0, …)`; the C
/// code's `double` truncates to the same value.
pub fn hash_counting_number(freq_hz: u32, cores: u16, chips: u16, errata: u32) -> u32 {
    let cores_up = u64::from(cores.next_power_of_two());
    let chips_up = u64::from(chips.next_power_of_two());
    let space = (1u64 << 32) / cores_up / chips_up;
    // hcn_max = space · 25 MHz / freq · ½
    let hcn_max = space * 25_000_000 / (2 * u64::from(freq_hz));
    hcn_max.saturating_sub(u64::from(errata)) as u32
}

/// Value for [`reg::PLL0_PARAMETER`] for a solved frequency.
pub fn pll_value(word: &PllWord, spec: &PllSpec) -> [u8; 4] {
    word.register_bytes(spec)
}

/// How long to wait for a result before the job is considered scanned
/// through, in milliseconds. Port of `calculate_bm_timeout_ms()`:
/// serial versions × serial nonces / frequency, scaled by
/// `timeout_percent` (in ‰ here, integer).
///
/// `version_size` is how many versions the chip rolls (65536 with the
/// BIP 320 mask). Returns `fallback_ms` when the inputs make no sense.
pub fn job_timeout_ms(
    freq_hz: u32,
    chips: u16,
    small_cores: u16,
    cores: u16,
    version_size: u32,
    timeout_permille: u32,
    fallback_ms: u32,
) -> u32 {
    if chips == 0 || freq_hz == 0 {
        return fallback_ms;
    }
    let cores_up = u64::from(cores.next_power_of_two());
    let small_up = u64::from(small_cores.next_power_of_two());
    let chips_up = u64::from(chips.next_power_of_two());
    if small_up < cores_up {
        return fallback_ms;
    }
    let midstates = small_up / cores_up;
    let serial_versions = u64::from(version_size) / midstates;
    let serial_nonces = (1u64 << 32) / cores_up / chips_up;
    // full-space time in ms = serial_versions · serial_nonces / freq_hz · 1000
    let full_ms = serial_versions * serial_nonces * 1000 / u64::from(freq_hz);
    if full_ms == 0 {
        return fallback_ms;
    }
    (full_ms * u64::from(timeout_permille) / 1000).min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{encode_cmd, Group, Op};
    use crate::pll::solve_mhz;

    /// `test_version_rolling.c`, "version mask register command remains
    /// byte exact": mask → full 11-byte frame.
    #[test]
    fn version_mask_frames_are_byte_exact() {
        #[rustfmt::skip]
        let cases: &[(u32, [u8; 11])] = &[
            (0,          [0x55, 0xaa, 0x51, 9, 0, 0xa4, 0x90, 0, 0, 0, 0x17]),
            (0x00002000, [0x55, 0xaa, 0x51, 9, 0, 0xa4, 0x90, 0, 0, 1, 0x12]),
            (0x00200000, [0x55, 0xaa, 0x51, 9, 0, 0xa4, 0x90, 0, 1, 0, 0x0b]),
            (0x02468000, [0x55, 0xaa, 0x51, 9, 0, 0xa4, 0x90, 0, 0x12, 0x34, 6]),
            (0x1fffe000, [0x55, 0xaa, 0x51, 9, 0, 0xa4, 0x90, 0, 0xff, 0xff, 0x1c]),
            (0xe0001fff, [0x55, 0xaa, 0x51, 9, 0, 0xa4, 0x90, 0, 0, 0, 0x17]),
            (0xffffffff, [0x55, 0xaa, 0x51, 9, 0, 0xa4, 0x90, 0, 0xff, 0xff, 0x1c]),
        ];
        for &(mask, want) in cases {
            let payload = write_payload(0x00, reg::VERSION_ROLLING, version_mask(mask));
            let mut buf = [0u8; 16];
            let n = encode_cmd(&mut buf, Group::All, Op::Write, &payload).unwrap();
            assert_eq!(&buf[..n], &want[..], "mask {mask:#010x}");
        }
        assert_eq!(version_mask(BIP320_VERSION_MASK), [0x90, 0x00, 0xff, 0xff]);
    }

    /// Difficulty 256 must produce the init sequence's ticket-mask write
    /// `00 14 00 00 00 FF` (the "present in the S21 XP init" line).
    #[test]
    fn ticket_mask_for_256_matches_the_init_sequence() {
        assert_eq!(
            write_payload(0x00, reg::TICKET_MASK, ticket_mask(256.0)),
            [0x00, 0x14, 0x00, 0x00, 0x00, 0xFF]
        );
    }

    /// The examples in `get_difficulty_mask()`'s comment, after the
    /// per-byte bit reversal it applies.
    #[test]
    fn ticket_mask_rounds_down_to_a_power_of_two_and_reverses_bytes() {
        assert_eq!(ticket_mask(256.0), [0, 0, 0, 0xFF]);
        assert_eq!(ticket_mask(512.0), [0, 0, 0x80, 0xFF]);
        assert_eq!(ticket_mask(1024.0), [0, 0, 0xC0, 0xFF]);
        assert_eq!(
            ticket_mask(300.0),
            [0, 0, 0, 0xFF],
            "300 rounds down to 256"
        );
        assert_eq!(
            ticket_mask(256.125),
            [0, 0, 0, 0xFF],
            "ceil to 257, then down to 256"
        );
        assert_eq!(ticket_mask(255.5), [0, 0, 0, 0xFF], "ceil to 256");
        assert_eq!(ticket_mask(1.0), [0, 0, 0, 0]);
        assert_eq!(ticket_mask(0.5), [0, 0, 0, 0]);
        assert_eq!(ticket_mask(65536.0), [0, 0, 0xFF, 0xFF]);
    }

    #[test]
    fn ticket_mask_to_difficulty_inverts_ticket_mask() {
        for d in [1u32, 2, 4, 256, 512, 1024, 65536, 1 << 20] {
            assert_eq!(ticket_mask_to_difficulty(ticket_mask(d as f64)), d, "{d}");
        }
        assert_eq!(ticket_mask_to_difficulty([0, 0, 0, 0xFF]), 256);
        assert_eq!(ticket_mask_to_difficulty([0, 0, 0x80, 0xFF]), 512);
    }

    #[test]
    fn ticket_mask_int_is_largest_power_of_two_minus_one() {
        for (d, m) in [
            (1, 0),
            (2, 1),
            (3, 1),
            (4, 3),
            (255, 127),
            (256, 255),
            (257, 255),
        ] {
            assert_eq!(ticket_mask_int(d), m, "difficulty {d}");
        }
        assert_eq!(ticket_mask_int(u32::MAX), (1 << 31) - 1);
    }

    #[test]
    fn reverse_bits_examples() {
        assert_eq!(reverse_bits(0x01), 0x80);
        assert_eq!(reverse_bits(0x80), 0x01);
        assert_eq!(reverse_bits(0x0F), 0xF0);
        assert_eq!(reverse_bits(0xFF), 0xFF);
        assert_eq!(reverse_bits(0x00), 0x00);
        assert_eq!(reverse_bits(0x12), 0x48);
    }

    /// WantClue `BM1397_set_max_baud` (divider 1, `0b01100001`) and
    /// `BM1397_set_default_baud` (divider 26, `0b01111010`, 115 749);
    /// ESP-Miner's divider 0 gives 3 125 000.
    #[test]
    fn bm1397_baud_words_match_both_references() {
        assert_eq!(BM1397_FAST_UART, [0x00, 0x00, 0x61, 0x31]);
        assert_eq!(BM1397_FAST_UART_BAUD, 1_562_500);
        assert_eq!(bm1397_misc_control_baud(26), [0x00, 0x00, 0x7A, 0x31]);
        assert_eq!(bm1397_baud(26), 115_740);
        assert_eq!(bm1397_misc_control_baud(0), [0x00, 0x00, 0x60, 0x31]);
        assert_eq!(bm1397_baud(0), 3_125_000);
    }

    /// 300 MHz × 672 small cores = 201.6 GH/s is a count of 192 260
    /// (0x2EF04); the .46's 153 GH/s would be 145 912; 0x007FFFFF is no
    /// reading (ESP-Miner); bit 31 marks the long window.
    #[test]
    fn bm1397_hashrate_register_reads_in_2_to_the_20() {
        assert_eq!(bm1397_hashrate(0x0002_EF04), Some((192_260 << 20, false)));
        assert_eq!(bm1397_hashrate(0x8002_EF04), Some((192_260 << 20, true)));
        assert_eq!(bm1397_hashrate(0x007F_FFFF), None);
        assert_eq!(bm1397_hashrate(0x807F_FFFF), None);
        assert_eq!(bm1397_hashrate(0), Some((0, false)));
        let v = bm1397_hashrate_value(201_600_000_000, false);
        assert_eq!(v, 192_260);
        assert_eq!(bm1397_hashrate(v).unwrap().0, 192_260 << 20);
        assert_eq!(
            bm1397_hashrate_value(153_000_000_000, true),
            0x8000_0000 | 145_912
        );
    }

    /// `BM1370_set_max_baud()`.
    #[test]
    fn fast_uart_payload_matches_the_driver() {
        assert_eq!(
            write_payload(0x00, reg::FAST_UART_CONFIGURATION, FAST_UART_1M),
            [0x00, 0x28, 0x11, 0x30, 0x02, 0x00]
        );
    }

    /// `BM1370_set_nonce_space(1.0, 525, 1, 128)`, by hand:
    /// space = 2³²/128/1 = 33 554 432; hcn_max = space·25/525·0.5 =
    /// 798 915.04; minus 2·134 → 798 647 (truncated).
    #[test]
    fn hash_counting_number_matches_the_c_formula_at_gamma_stock() {
        assert_eq!(hash_counting_number(525_000_000, 128, 1, 2 * 134), 798_647);
        // Two chips halve the space; 128 cores stays a power of two.
        assert_eq!(hash_counting_number(525_000_000, 128, 2, 2 * 134), 399_189);
        // Faster clock → smaller count.
        assert!(hash_counting_number(640_625_000, 128, 1, 2 * 134) < 798_647);
    }

    /// PLL write for the Gamma stock point: register 0x08 ← `40 A8 02 30`.
    #[test]
    fn pll_write_payload_for_525_mhz() {
        let spec = PllSpec::bm1370();
        let w = solve_mhz(&spec, 525).unwrap();
        assert_eq!(
            write_payload(0x00, reg::PLL0_PARAMETER, pll_value(&w, &spec)),
            [0x00, 0x08, 0x40, 0xA8, 0x02, 0x30]
        );
    }

    /// Port check of `calculate_bm_timeout_ms()` for the harness fixture
    /// (2 chips, 112 cores, 200 MHz): with BM1370 numbers instead —
    /// 1 chip, 2040 small / 128 cores, 65536 versions, 525 MHz.
    /// midstates = 2048/128 = 16; serial_versions = 4096;
    /// serial_nonces = 2³²/128 = 33 554 432; full = 4096·33 554 432/525e6
    /// = 261 788 ms; at 100 % that is the timeout.
    #[test]
    fn job_timeout_matches_the_c_formula() {
        let ms = job_timeout_ms(525_000_000, 1, 2040, 128, 65536, 1000, 500);
        assert_eq!(ms, 261_788);
        assert_eq!(
            job_timeout_ms(0, 1, 2040, 128, 65536, 1000, 500),
            500,
            "fallback"
        );
        assert_eq!(
            job_timeout_ms(525_000_000, 0, 2040, 128, 65536, 1000, 500),
            500
        );
    }

    proptest::proptest! {
        /// The ticket mask is always a run of ones (after un-reversing),
        /// never above the difficulty, and monotone in it.
        #[test]
        fn ticket_mask_is_a_contiguous_run_not_above_difficulty(d in 1u32..=1 << 30) {
            let m = ticket_mask_int(d);
            proptest::prop_assert!(m < d);
            proptest::prop_assert!((m + 1).is_power_of_two());
            proptest::prop_assert!(ticket_mask_int(d + 1) >= m);
        }

        #[test]
        fn version_mask_keeps_only_bits_13_to_28(mask in proptest::num::u32::ANY) {
            let v = version_mask(mask);
            let back = u32::from(u16::from_be_bytes([v[2], v[3]])) << 13;
            proptest::prop_assert_eq!(back, mask & BIP320_VERSION_MASK);
            proptest::prop_assert_eq!(&v[..2], &[0x90, 0x00]);
        }

        #[test]
        fn reverse_bits_is_an_involution(b in 0u8..=255) {
            proptest::prop_assert_eq!(reverse_bits(reverse_bits(b)), b);
        }
    }
}

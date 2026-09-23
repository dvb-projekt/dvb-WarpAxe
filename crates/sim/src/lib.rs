//! BM13xx chip simulator: answers on a UART, never hashes.
//!
//! ```text
//!   firmware bytes ──► SimChain::feed ──► CmdDecoder ──► SimChip::handle
//!                                                             │
//!   SimChain::advance(µs) ──────────────────────────────────► nonces due
//!                                                             │
//!   firmware bytes ◄── SimChain::drain ◄───────────────────── responses
//! ```
//!
//! The simulator's job is to make field failures reproducible. A chip
//! that stops producing nonces above a clock, that answers registers
//! but not work, that flips bits on the wire, that sends a nonce twice —
//! each is a [`Fault`] a test injects, and each becomes a regression
//! test for whatever drives the chip.
//!
//! Determinism: everything a chain does is a function of its profile,
//! seed, the bytes fed and the time advanced. Same inputs, same output
//! bytes, every run.

#![forbid(unsafe_code)]

pub mod board;
pub mod chain;
pub mod chip;
pub mod fault;
pub mod rng;

pub use board::{Board, BoardFault, BoardSpec};
pub use chain::SimChain;
pub use chip::{ChipState, ChipStats, SimChip};
pub use fault::Fault;

#[cfg(test)]
mod tests {
    use super::*;
    use warpaxe_core::chip::{bm1370::BM1370, render, Rendered};
    use warpaxe_core::decoder::RxDecoder;
    use warpaxe_core::frame::{encode_cmd, encode_job, Group, Op, MAX_FRAME_LEN};
    use warpaxe_core::job::{Job, JobIds};
    use warpaxe_core::pll::{solve_mhz, PllSpec};
    use warpaxe_core::regs::{self, reg, write_payload};
    use warpaxe_core::result::{JobResult, RegisterResult};

    /// Send the profile's whole init to the chain, as the firmware would.
    fn run_init(chain: &mut SimChain, n: u8) {
        let mut buf = [0u8; MAX_FRAME_LEN];
        for r in render(&BM1370, n) {
            match r {
                Rendered::Cmd {
                    group,
                    op,
                    payload,
                    len,
                    ..
                } => {
                    let k = encode_cmd(&mut buf, group, op, &payload[..len as usize]).unwrap();
                    chain.feed(&buf[..k]);
                }
                Rendered::Delay(ms) => chain.advance(u64::from(ms) * 1000),
                Rendered::CountChips => {}
            }
        }
    }

    fn write_all(chain: &mut SimChain, r: u8, v: [u8; 4]) {
        let mut buf = [0u8; 16];
        let k = encode_cmd(&mut buf, Group::All, Op::Write, &write_payload(0, r, v)).unwrap();
        chain.feed(&buf[..k]);
    }

    fn set_pll(chain: &mut SimChain, mhz: u32) {
        let spec = PllSpec::bm1370();
        let w = solve_mhz(&spec, mhz).unwrap();
        write_all(chain, reg::PLL0_PARAMETER, w.register_bytes(&spec));
    }

    fn send_job(chain: &mut SimChain, job_id: u8) {
        let job = Job {
            job_id,
            starting_nonce: 0,
            nbits: 0x1705dd01,
            ntime: 0x64658bd8,
            merkle_root: [0x11; 32],
            prev_block_hash: [0x22; 32],
            version: 0x20000004,
        };
        let mut buf = [0u8; MAX_FRAME_LEN];
        let k = encode_job(&mut buf, &job.to_bytes()).unwrap();
        chain.feed(&buf[..k]);
    }

    fn decode_all(bytes: &[u8]) -> Vec<warpaxe_core::decoder::Frame> {
        let mut d = RxDecoder::new();
        let mut out = Vec::new();
        d.push_slice(bytes, |f| out.push(f));
        assert_eq!(
            d.stats().dropped,
            0,
            "firmware side must see a clean stream"
        );
        out
    }

    /// The prediction from `crc.rs`, end to end: a register-0 read on a
    /// fresh BM1370 answers `AA 55 13 70 00 00 00 00 00 00 10`.
    #[test]
    fn chip_id_reply_is_the_predicted_bytes() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        let mut buf = [0u8; 16];
        let k = encode_cmd(&mut buf, Group::All, Op::Read, &[0x00, reg::CHIP_ID]).unwrap();
        chain.feed(&buf[..k]);
        assert_eq!(
            chain.drain(),
            vec![0xAA, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10]
        );
    }

    /// The full init from `render()` brings one chip to Addressed with
    /// no protocol violation, and leaves the registers the init wrote.
    #[test]
    fn full_init_addresses_the_chip_without_violations() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        run_init(&mut chain, 1);
        let c = &chain.chips()[0];
        assert_eq!(c.state(), ChipState::Addressed);
        assert_eq!(c.address(), 0);
        assert_eq!(c.stats().violations, 0);
        assert_eq!(c.ticket_difficulty(), 256, "TicketMask step wrote 256");
        assert_eq!(c.register(reg::VERSION_ROLLING), 0x9000_FFFF);
        assert_eq!(c.register(reg::PLL3_PARAMETER), 0x5AA5_5AA5);
        assert_eq!(chain.wire_stats().dropped, 0);
        // One chip-id reply came back.
        let replies = decode_all(&chain.drain());
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].response.payload[..2], [0x13, 0x70]);
    }

    /// Two chips: two chip-id replies, addresses 0 and 128, and a
    /// GROUP_SINGLE read is answered by exactly one of them.
    #[test]
    fn two_chip_chain_addresses_and_routes() {
        let mut chain = SimChain::new(&BM1370, 2, 1);
        run_init(&mut chain, 2);
        assert_eq!(chain.chips()[0].address(), 0);
        assert_eq!(chain.chips()[1].address(), 128);
        assert!(chain
            .chips()
            .iter()
            .all(|c| c.state() == ChipState::Addressed));
        assert_eq!(chain.totals().violations, 0);
        let replies = decode_all(&chain.drain());
        assert_eq!(replies.len(), 2, "one chip-id reply per chip");

        let mut buf = [0u8; 16];
        let k = encode_cmd(&mut buf, Group::Single, Op::Read, &[128, reg::TICKET_MASK]).unwrap();
        chain.feed(&buf[..k]);
        let replies = decode_all(&chain.drain());
        assert_eq!(replies.len(), 1);
        let r = RegisterResult::parse(&replies[0].response.payload);
        assert_eq!((r.asic_address, r.register), (128, reg::TICKET_MASK));
        assert_eq!(regs::ticket_mask_to_difficulty(r.value.to_be_bytes()), 256);
    }

    #[test]
    fn set_address_without_inactive_is_a_violation() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        let mut buf = [0u8; 16];
        let k = encode_cmd(&mut buf, Group::Single, Op::SetAddress, &[0x00, 0x00]).unwrap();
        chain.feed(&buf[..k]);
        assert_eq!(chain.chips()[0].stats().violations, 1);
        assert_eq!(
            chain.chips()[0].state(),
            ChipState::Addressed,
            "…but it still takes it"
        );
    }

    #[test]
    fn pll_write_sets_the_clock_and_a_job_without_a_clock_yields_nothing() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        run_init(&mut chain, 1);
        chain.drain();
        assert_eq!(chain.chips()[0].frequency_hz(), 0);
        send_job(&mut chain, 24);
        chain.advance(10_000_000);
        assert!(chain.drain().is_empty(), "no PLL write, no nonces");
        set_pll(&mut chain, 525);
        assert_eq!(chain.chips()[0].frequency_hz(), 525_000_000);
        assert_eq!(chain.chips()[0].register(reg::PLL0_PARAMETER), 0x40A8_0230);
    }

    /// At 525 MHz, 2040 small cores, difficulty 256: 0.974 nonces/s.
    /// The chip exhausts its nonce space (65536 versions × 2³²/128) after
    /// ~262 s and goes quiet, so over 300 s with one job that is ~255
    /// nonces, ±16 (1σ). Every nonce carries the job id, the chip's
    /// address and a core id in range, and decodes cleanly on the
    /// firmware side.
    #[test]
    fn nonces_arrive_at_the_expected_rate_with_the_chips_stamp() {
        let mut chain = SimChain::new(&BM1370, 1, 7);
        run_init(&mut chain, 1);
        set_pll(&mut chain, 525);
        chain.drain();
        let rate = chain.chips()[0].nonce_rate_per_s();
        assert!((rate - 0.974).abs() < 0.01, "rate {rate}");
        send_job(&mut chain, 24);
        chain.advance(300_000_000);
        let frames = decode_all(&chain.drain());
        let n = frames.len();
        assert!(
            (205..=305).contains(&n),
            "{n} nonces in 300 s (expect ~255, ±3σ)"
        );
        for f in &frames {
            assert!(f.response.is_job());
            let r = JobResult::parse(&f.response.payload);
            assert_eq!(r.job_id, 24);
            assert_eq!(r.asic_address, 0);
            assert!(r.core_id < 128);
            assert!(r.small_core_id < 16);
            assert_eq!(r.version_bits & !regs::BIP320_VERSION_MASK, 0);
        }
        assert_eq!(chain.chips()[0].stats().nonces as usize, n);
    }

    #[test]
    fn a_new_job_replaces_the_old_one_in_the_nonces() {
        let mut chain = SimChain::new(&BM1370, 1, 3);
        run_init(&mut chain, 1);
        set_pll(&mut chain, 525);
        let mut ids = JobIds::bm1370();
        send_job(&mut chain, ids.advance());
        chain.advance(60_000_000);
        let second = ids.advance();
        send_job(&mut chain, second);
        chain.drain();
        chain.advance(60_000_000);
        let frames = decode_all(&chain.drain());
        assert!(!frames.is_empty());
        assert!(frames
            .iter()
            .all(|f| JobResult::parse(&f.response.payload).job_id == second));
    }

    #[test]
    fn same_seed_same_bytes() {
        let run = |seed| {
            let mut chain = SimChain::new(&BM1370, 1, seed);
            run_init(&mut chain, 1);
            set_pll(&mut chain, 600);
            send_job(&mut chain, 24);
            chain.advance(120_000_000);
            chain.drain()
        };
        assert_eq!(run(5), run(5));
        assert_ne!(run(5), run(6));
    }

    #[test]
    fn fault_hang_above_stops_nonces_but_not_registers() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        run_init(&mut chain, 1);
        chain
            .chip_mut(0)
            .inject(Fault::HangAbove { hz: 575_000_000 });
        set_pll(&mut chain, 550);
        send_job(&mut chain, 24);
        chain.advance(120_000_000);
        chain.drain();
        assert!(chain.chips()[0].stats().nonces > 50, "runs fine at 550");
        let before = chain.chips()[0].stats().nonces;
        set_pll(&mut chain, 600);
        send_job(&mut chain, 48);
        chain.advance(120_000_000);
        chain.drain();
        assert_eq!(
            chain.chips()[0].stats().nonces,
            before,
            "hung at 600: no nonces"
        );
        // …but the chip still talks.
        let mut buf = [0u8; 16];
        let k = encode_cmd(&mut buf, Group::All, Op::Read, &[0x00, reg::PLL0_PARAMETER]).unwrap();
        chain.feed(&buf[..k]);
        let r = decode_all(&chain.drain());
        assert_eq!(r.len(), 1);
        assert_eq!(
            RegisterResult::parse(&r[0].response.payload).value,
            0x50C0_0230
        );
    }

    #[test]
    fn fault_no_response_silences_then_recovers() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        run_init(&mut chain, 1);
        chain.drain();
        chain
            .chip_mut(0)
            .inject(Fault::NoResponse { for_us: 5_000_000 });
        let mut buf = [0u8; 16];
        let k = encode_cmd(&mut buf, Group::All, Op::Read, &[0x00, reg::CHIP_ID]).unwrap();
        chain.feed(&buf[..k]);
        assert!(chain.drain().is_empty());
        assert_eq!(chain.chips()[0].stats().suppressed, 1);
        chain.advance(6_000_000);
        chain.feed(&buf[..k]);
        assert_eq!(decode_all(&chain.drain()).len(), 1);
    }

    #[test]
    fn fault_bit_flips_show_up_as_desync_on_the_firmware_side() {
        let mut chain = SimChain::new(&BM1370, 1, 9);
        run_init(&mut chain, 1);
        set_pll(&mut chain, 525);
        chain.drain();
        chain.chip_mut(0).inject(Fault::BitFlipTx { per_mille: 20 });
        send_job(&mut chain, 24);
        chain.advance(300_000_000);
        let bytes = chain.drain();
        let mut d = RxDecoder::new();
        let mut ok = 0;
        d.push_slice(&bytes, |_| ok += 1);
        let s = d.stats();
        assert!(
            s.dropped > 0 || s.crc_failures > 0,
            "flips must be visible: {s:?}"
        );
        assert!(ok > 0, "and some frames still get through");
        assert!(ok < chain.chips()[0].stats().nonces as usize);
    }

    #[test]
    fn fault_duplicate_nonce_sends_some_twice() {
        let mut chain = SimChain::new(&BM1370, 1, 11);
        run_init(&mut chain, 1);
        set_pll(&mut chain, 525);
        chain.drain();
        chain
            .chip_mut(0)
            .inject(Fault::DuplicateNonce { per_mille: 500 });
        send_job(&mut chain, 24);
        chain.advance(300_000_000);
        let frames = decode_all(&chain.drain());
        let dups = frames
            .windows(2)
            .filter(|w| w[0].response == w[1].response)
            .count();
        assert!(dups > 20, "{dups} duplicates in {} frames", frames.len());
    }

    #[test]
    fn fault_reg_read_corrupt_changes_the_read_back_value() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        run_init(&mut chain, 1);
        chain.drain();
        chain.chip_mut(0).inject(Fault::RegReadCorrupt {
            reg: reg::MISC_CONTROL,
        });
        let mut buf = [0u8; 16];
        let k = encode_cmd(&mut buf, Group::All, Op::Read, &[0x00, reg::MISC_CONTROL]).unwrap();
        chain.feed(&buf[..k]);
        let r = decode_all(&chain.drain());
        let v = RegisterResult::parse(&r[0].response.payload).value;
        assert_ne!(v, chain.chips()[0].register(reg::MISC_CONTROL));
        assert_eq!(
            v ^ 0x0001_0100,
            chain.chips()[0].register(reg::MISC_CONTROL)
        );
    }

    #[test]
    fn fast_uart_write_moves_the_chip_to_1_mbaud_unless_ignored() {
        let mut chain = SimChain::new(&BM1370, 1, 1);
        assert_eq!(chain.chips()[0].baud(), 115_200);
        write_all(&mut chain, reg::FAST_UART_CONFIGURATION, regs::FAST_UART_1M);
        assert_eq!(chain.chips()[0].baud(), 1_000_000);

        let mut chain = SimChain::new(&BM1370, 1, 1);
        chain.chip_mut(0).inject(Fault::BaudSwitchIgnored);
        write_all(&mut chain, reg::FAST_UART_CONFIGURATION, regs::FAST_UART_1M);
        assert_eq!(chain.chips()[0].baud(), 115_200);
    }

    #[test]
    fn garbage_nonces_are_still_valid_frames() {
        let mut chain = SimChain::new(&BM1370, 1, 13);
        run_init(&mut chain, 1);
        set_pll(&mut chain, 525);
        chain.drain();
        chain
            .chip_mut(0)
            .inject(Fault::GarbageNonces { per_mille: 1000 });
        send_job(&mut chain, 24);
        chain.advance(100_000_000);
        let frames = decode_all(&chain.drain());
        assert!(!frames.is_empty());
        // With every nonce garbage, the job id in the id byte is random:
        // most will not be 24.
        let wrong = frames
            .iter()
            .filter(|f| JobResult::parse(&f.response.payload).job_id != 24)
            .count();
        assert!(
            wrong * 2 > frames.len(),
            "{wrong} of {} carry a wrong job id",
            frames.len()
        );
    }
}

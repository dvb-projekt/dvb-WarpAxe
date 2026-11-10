//! The driver against the simulator, in-process. Same code path the
//! firmware will run, minus the UART.

use std::collections::VecDeque;
use std::convert::Infallible;

use warpaxe_chain::{Chain, Config, Error, Event, InitError, Phase, Uart, Work};
use warpaxe_core::chip::bm1370::BM1370;
use warpaxe_core::chip::bm1373::BM1373;
use warpaxe_core::chip::bm1397::BM1397;
use warpaxe_core::chip::ChipProfile;
use warpaxe_core::regs::reg;
use warpaxe_sim::chain::SimChain;
use warpaxe_sim::chip::ChipState;
use warpaxe_sim::fault::Fault;

/// A `SimChain` behind the `Uart` trait, with the one thing the chain
/// itself does not model: a baud-rate mismatch. Bytes sent or received
/// while the host and the chips disagree on the rate are lost.
struct SimUart {
    sim: SimChain,
    rx: VecDeque<u8>,
    host_baud: u32,
    lost_to_baud: u64,
}

impl SimUart {
    fn with(profile: &'static ChipProfile, chips: u8, seed: u64) -> Self {
        SimUart {
            sim: SimChain::new(profile, chips, seed),
            rx: VecDeque::new(),
            host_baud: 115_200,
            lost_to_baud: 0,
        }
    }

    fn rates_match(&self) -> bool {
        self.sim
            .chips()
            .first()
            .is_none_or(|c| c.baud() == self.host_baud)
    }

    fn advance(&mut self, dt_us: u64) {
        self.sim.advance(dt_us);
        let out = self.sim.drain();
        if self.rates_match() {
            self.rx.extend(out);
        } else {
            self.lost_to_baud += out.len() as u64;
        }
    }
}

impl Uart for SimUart {
    type Error = Infallible;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
        if self.rates_match() {
            self.sim.feed(bytes);
        } else {
            self.lost_to_baud += bytes.len() as u64;
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Infallible> {
        let mut n = 0;
        while n < buf.len() {
            match self.rx.pop_front() {
                Some(b) => {
                    buf[n] = b;
                    n += 1;
                }
                None => break,
            }
        }
        Ok(n)
    }

    fn set_baud(&mut self, baud: u32) -> Result<(), Infallible> {
        self.host_baud = baud;
        Ok(())
    }
}

struct Bench {
    drv: Chain,
    uart: SimUart,
    now: u64,
    events: Vec<Event>,
    works: u32,
}

impl Bench {
    fn new(chips: u8, config: Config) -> Self {
        Self::with(&BM1370, chips, config)
    }
    fn with(profile: &'static ChipProfile, chips: u8, config: Config) -> Self {
        let mut drv = Chain::new(profile, config);
        drv.start(0);
        Bench {
            drv,
            uart: SimUart::with(profile, chips, 7),
            now: 0,
            events: Vec::new(),
            works: 0,
        }
    }

    /// Low difficulty so nonces come in seconds, not minutes.
    fn config() -> Config {
        Config {
            difficulty: 8,
            ..Config::for_profile(&BM1370)
        }
    }

    fn run_ms(&mut self, ms: u64) {
        let Bench {
            drv,
            uart,
            now,
            events,
            ..
        } = self;
        for _ in 0..ms {
            drv.poll(uart, *now, &mut |e| events.push(e)).unwrap();
            uart.advance(1000);
            *now += 1000;
        }
    }

    fn run_until_ready(&mut self, max_ms: u64) {
        for _ in 0..max_ms {
            self.run_ms(1);
            if self.drv.accepting_work() {
                return;
            }
            if self.drv.phase() == Phase::Failed {
                return;
            }
        }
        panic!("not ready after {max_ms} ms: {:?}", self.drv.phase());
    }

    fn work(&mut self) -> Work {
        self.works += 1;
        let k = self.works;
        Work {
            starting_nonce: 0,
            nbits: 0x1703_0ecd,
            ntime: 0x66aa_0000 + k,
            merkle_root: [(k & 0xFF) as u8; 32],
            prev_block_hash: [0x11; 32],
            version: 0x2000_0000,
        }
    }

    /// Mine for `ms`, submitting fresh work every 100 ms.
    fn mine_ms(&mut self, ms: u64) {
        let mut left = ms;
        while left > 0 {
            let w = self.work();
            self.drv.submit(&mut self.uart, &w, self.now).unwrap();
            let step = left.min(100);
            self.run_ms(step);
            left -= step;
        }
    }

    fn count<F: Fn(&Event) -> bool>(&self, f: F) -> usize {
        self.events.iter().filter(|e| f(e)).count()
    }

    fn has<F: Fn(&Event) -> bool>(&self, f: F) -> bool {
        self.events.iter().any(f)
    }
}

#[test]
fn bring_up_enumerates_addresses_ramps_and_breaks_no_rule() {
    let mut b = Bench::new(1, Bench::config());
    b.run_until_ready(20_000);
    assert!(b.has(|e| matches!(
        e,
        Event::ChipFound {
            index: 0,
            chip_id: 0x1370
        }
    )));
    assert!(b.has(|e| *e == Event::Enumerated { count: 1 }));
    assert!(b.has(|e| *e == Event::Ready));
    assert!(b.has(|e| *e == Event::FrequencyApplied { hz: 525_000_000 }));
    assert_eq!(b.drv.frequency_hz(), 525_000_000);
    let chip = &b.uart.sim.chips()[0];
    assert_eq!(chip.state(), ChipState::Addressed);
    assert_eq!(chip.frequency_hz(), 525_000_000);
    assert_eq!(chip.baud(), 1_000_000, "chip switched to the fast rate");
    assert_eq!(b.uart.host_baud, 1_000_000, "host followed");
    assert_eq!(
        b.uart.lost_to_baud, 0,
        "nothing sent across a rate mismatch"
    );
    assert_eq!(
        chip.ticket_difficulty(),
        8,
        "configured difficulty, not the default"
    );
    assert_ne!(chip.register(reg::HASH_COUNTING_NUMBER), 0, "HCN written");
    let t = b.uart.sim.totals();
    assert_eq!(t.violations, 0, "no sequence violation seen by the chip");
    assert_eq!(b.uart.sim.wire_stats().crc_failures, 0);
    assert_eq!(b.drv.stats().decoder.crc_failures, 0);
    // The ramp: 56.25 → 525 in 6.25 steps is 75 PLL writes, plus the first.
    assert!(chip.stats().writes > 75, "writes = {}", chip.stats().writes);
}

#[test]
fn four_chips_get_distinct_addresses_and_every_nonce_is_attributed() {
    let mut b = Bench::new(4, Bench::config());
    b.run_until_ready(20_000);
    assert_eq!(b.drv.chip_count(), 4);
    let addrs: Vec<u8> = b.drv.chips().iter().map(|c| c.address).collect();
    assert_eq!(addrs, vec![0, 64, 128, 192]);
    let sim_addrs: Vec<u8> = b.uart.sim.chips().iter().map(|c| c.address()).collect();
    assert_eq!(
        sim_addrs, addrs,
        "the chips took the addresses the driver thinks they have"
    );

    b.mine_ms(5_000);
    let sim_nonces = u64::from(b.uart.sim.totals().nonces);
    let yielded = b.count(|e| matches!(e, Event::Nonce(_))) as u64;
    assert_eq!(
        yielded, sim_nonces,
        "every nonce the chips sent was yielded"
    );
    assert_eq!(b.drv.stats().unknown_chip, 0);
    assert_eq!(b.drv.stats().stale_local, 0);
    for i in 0..4u8 {
        let n = b.count(|e| matches!(e, Event::Nonce(f) if f.chip == i));
        assert!(n > 50, "chip {i} yielded only {n} nonces in 5 s");
        assert_eq!(b.drv.chips()[usize::from(i)].nonces, n as u64);
    }
    // Versions carry the rolled bits on top of the work's version.
    assert!(b.has(|e| matches!(e, Event::Nonce(f) if f.version & 0x2000_0000 != 0)));
    assert!(b.has(|e| matches!(e, Event::Nonce(f) if f.version != 0x2000_0000)));
}

#[test]
fn nonces_for_retired_work_are_counted_not_yielded() {
    let mut b = Bench::new(1, Bench::config());
    b.run_until_ready(20_000);
    let w = b.work();
    let r = b.drv.submit(&mut b.uart, &w, b.now).unwrap();
    b.run_ms(1_000);
    let live = b.count(|e| matches!(e, Event::Nonce(f) if f.work == r));
    assert!(live > 10, "{live}");
    b.drv.retire_all();
    let before = b.events.len();
    b.run_ms(1_000);
    let after: Vec<&Event> = b.events[before..].iter().collect();
    assert!(after.iter().all(|e| !matches!(e, Event::Nonce(_))));
    assert!(after.iter().any(|e| matches!(e, Event::Stale { .. })));
    assert!(b.drv.stats().stale_local > 10);
}

#[test]
fn a_hung_chip_is_reported_silent_and_comes_back_when_the_clock_drops() {
    let mut b = Bench::new(2, Bench::config());
    b.uart
        .sim
        .chip_mut(1)
        .inject(Fault::HangAbove { hz: 400_000_000 });
    b.run_until_ready(20_000);
    b.mine_ms(3_000);
    assert!(b.has(|e| *e == Event::ChipSilent { index: 1 }));
    assert!(!b.has(|e| *e == Event::ChipSilent { index: 0 }));
    assert!(b.drv.chips()[1].silent);
    assert_eq!(b.drv.chips()[1].nonces, 0);
    assert!(b.drv.chips()[0].nonces > 50);

    b.drv.set_frequency(400_000_000);
    b.mine_ms(6_000); // 20 steps down at 100 ms, then nonces again
    assert!(b.has(|e| *e == Event::FrequencyApplied { hz: 400_000_000 }));
    assert!(b.has(|e| *e == Event::ChipBack { index: 1 }));
    assert!(!b.drv.chips()[1].silent);
    assert!(b.drv.chips()[1].nonces > 0);
}

#[test]
fn a_frequency_is_reported_as_solved_not_as_requested() {
    let mut b = Bench::new(1, Bench::config());
    b.run_until_ready(20_000);
    assert_eq!(b.drv.set_frequency(640_000_000), Some(640_625_000));
    b.mine_ms(3_000);
    assert!(b.has(|e| *e == Event::FrequencyApplied { hz: 640_625_000 }));
    assert_eq!(b.uart.sim.chips()[0].frequency_hz(), 640_625_000);
    // Off the grid entirely: refused up front, nothing changes.
    assert_eq!(b.drv.set_frequency(5_000_000), None);
    b.mine_ms(500);
    assert_eq!(b.drv.frequency_hz(), 640_625_000);
    assert!(!b.has(|e| matches!(e, Event::FrequencyUnreachable { .. })));
}

#[test]
fn a_configured_clock_off_the_grid_falls_back_to_stock() {
    let cfg = Config {
        frequency_hz: 5_000_000,
        ..Bench::config()
    };
    let mut b = Bench::new(1, cfg);
    b.run_until_ready(20_000);
    assert!(b.has(|e| *e == Event::FrequencyUnreachable { hz: 5_000_000 }));
    assert!(b.has(|e| *e == Event::FrequencyApplied { hz: 525_000_000 }));
}

#[test]
fn bit_flips_show_as_crc_failures_and_desyncs_not_as_nonces() {
    let mut b = Bench::new(1, Bench::config());
    b.run_until_ready(20_000);
    b.uart.sim.inject_all(Fault::BitFlipTx { per_mille: 20 });
    b.mine_ms(5_000);
    let s = b.drv.stats();
    assert!(s.decoder.crc_failures > 0, "{s:?}");
    assert!(s.desyncs > 0, "{s:?}");
    let yielded = b.count(|e| matches!(e, Event::Nonce(_))) as u64;
    let sent = u64::from(b.uart.sim.totals().nonces);
    assert!(
        yielded < sent,
        "some frames must have been lost: {yielded} of {sent}"
    );
    assert!(
        yielded > sent / 2,
        "but most got through: {yielded} of {sent}"
    );
}

#[test]
fn garbage_nonces_land_in_the_counters_not_at_the_pool() {
    let mut b = Bench::new(1, Bench::config());
    b.run_until_ready(20_000);
    b.uart
        .sim
        .inject_all(Fault::GarbageNonces { per_mille: 300 });
    b.mine_ms(5_000);
    let s = *b.drv.stats();
    let sent = u64::from(b.uart.sim.totals().nonces);
    // A garbage payload has a random address (with one chip every value
    // maps to it, as on the real BM1370) and a random job id (16 slots,
    // of which one or two are live): it lands as stale or duplicate.
    assert!(s.stale_local + s.duplicates > 0, "{s:?}");
    assert!(
        s.nonces + s.unknown_chip + s.stale_local + s.duplicates == sent,
        "{s:?} vs {sent}"
    );
}

#[test]
fn duplicate_nonces_are_dropped_once() {
    let mut b = Bench::new(1, Bench::config());
    b.run_until_ready(20_000);
    b.uart
        .sim
        .inject_all(Fault::DuplicateNonce { per_mille: 500 });
    b.mine_ms(3_000);
    assert!(b.drv.stats().duplicates > 10, "{:?}", b.drv.stats());
    let mut keys: Vec<(u32, u32, u8)> = b
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Nonce(f) => Some((f.nonce, f.version, f.work.job_id)),
            _ => None,
        })
        .collect();
    let n = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), n, "yielded nonces are unique");
}

#[test]
fn an_ignored_baud_switch_is_total_silence_not_a_crash() {
    let mut b = Bench::new(2, Bench::config());
    b.uart.sim.inject_all(Fault::BaudSwitchIgnored);
    b.run_until_ready(20_000);
    assert!(b.uart.lost_to_baud > 0);
    b.mine_ms(3_000);
    assert_eq!(b.drv.stats().nonces, 0);
    assert!(b.has(|e| *e == Event::ChipSilent { index: 0 }));
    assert!(b.has(|e| *e == Event::ChipSilent { index: 1 }));
}

#[test]
fn a_chip_count_mismatch_is_reported_and_the_chain_runs_with_what_it_found() {
    let cfg = Config {
        expected_chips: 4,
        ..Bench::config()
    };
    let mut b = Bench::new(3, cfg);
    b.run_until_ready(20_000);
    assert!(b.has(|e| *e
        == Event::ChipCountMismatch {
            expected: 4,
            found: 3
        }));
    assert_eq!(b.drv.chip_count(), 3);
    b.mine_ms(2_000);
    assert!(b.drv.chips().iter().all(|c| c.nonces > 0));
}

#[test]
fn no_chips_is_a_failed_bring_up_and_work_is_refused() {
    let mut b = Bench::new(0, Bench::config());
    b.run_until_ready(5_000);
    assert_eq!(b.drv.phase(), Phase::Failed);
    assert!(b.has(|e| *e == Event::InitFailed(InitError::NoChips)));
    let w = b.work();
    assert!(matches!(
        b.drv.submit(&mut b.uart, &w, b.now),
        Err(Error::NotReady)
    ));
}

#[test]
fn needs_work_after_the_nonce_space_is_scanned() {
    let mut b = Bench::new(1, Bench::config());
    b.run_until_ready(20_000);
    assert!(b.drv.needs_work(b.now), "nothing submitted yet");
    let w = b.work();
    b.drv.submit(&mut b.uart, &w, b.now).unwrap();
    assert!(!b.drv.needs_work(b.now));
    let timeout = b.drv.work_timeout_us();
    assert!(timeout > 1_000_000, "{timeout}");
    assert!(b.drv.needs_work(b.now + timeout));
}

/// The BM1373 as NerdOS drives it: nine-byte chip-id replies, addresses
/// in steps of 16, single-chip commands at address ÷ 4, the fixed hash
/// counting number. Driver and simulator read the same reference, so
/// this proves consistency, not the chip.
#[test]
fn bm1373_enumerates_with_short_replies_stride_16_and_attributes_nonces() {
    // The Gaia's stock ticket mask is 4096: a nonce every few seconds
    // per chip. A low difficulty makes the attribution check quick.
    let config = Config {
        expected_chips: 4,
        difficulty: 32,
        ..Config::for_profile(&BM1373)
    };
    let mut b = Bench::with(&BM1373, 4, config);
    b.run_until_ready(20_000);
    assert_eq!(b.drv.chip_count(), 4);
    let addrs: Vec<u8> = b.drv.chips().iter().map(|c| c.address).collect();
    assert_eq!(addrs, vec![0, 16, 32, 48]);
    let sim_addrs: Vec<u8> = b.uart.sim.chips().iter().map(|c| c.address()).collect();
    assert_eq!(sim_addrs, addrs);
    assert_eq!(
        b.uart.sim.totals().violations,
        0,
        "no chip saw a rule broken"
    );
    assert_eq!(b.drv.stats().decoder.crc_failures, 0);

    b.mine_ms(10_000);
    let sim_nonces = u64::from(b.uart.sim.totals().nonces);
    assert!(
        sim_nonces > 200,
        "the simulated chain hashed: {sim_nonces} nonces"
    );
    let yielded = b.count(|e| matches!(e, Event::Nonce(_))) as u64;
    assert_eq!(yielded, sim_nonces);
    assert_eq!(b.drv.stats().unknown_chip, 0);
    for i in 0..4u8 {
        let n = b.count(|e| matches!(e, Event::Nonce(f) if f.chip == i));
        assert!(n > 50, "chip {i} yielded only {n} nonces in 10 s");
    }
}

/// The BM1397 path end to end against the simulator: the chip answers
/// the id read with nine bytes, the init is WantClue's register list,
/// the clock is written with the PLL0 divider first and the gekko word,
/// nothing goes to the hash counting number or the version-rolling
/// mask, the baud switch is the MISC_CONTROL write followed by the host
/// 100 ms later, jobs are 146-byte midstate packets, and every nonce
/// comes back with the version the midstate number says.
#[test]
fn bm1397_brings_up_with_nine_byte_replies_midstate_jobs_and_no_rolling() {
    // Difficulty 1: a job lasts 56 ms of the bench's 100 ms interval, so
    // at the stock 256 a nonce would take minutes.
    let config = Config {
        expected_chips: 1,
        difficulty: 1,
        ..Config::for_profile(&BM1397)
    };
    let mut b = Bench::with(&BM1397, 1, config);
    b.run_until_ready(20_000);
    assert_eq!(b.drv.chip_count(), 1);
    assert!(b.has(|e| matches!(
        e,
        Event::ChipFound {
            chip_id: 0x1397,
            ..
        }
    )));
    let chip = &b.uart.sim.chips()[0];
    assert_eq!(chip.state(), ChipState::Addressed);
    assert_eq!(chip.stats().violations, 0);
    assert_eq!(b.drv.stats().decoder.crc_failures, 0);
    assert_eq!(b.drv.stats().decoder.dropped, 0);
    // The register file after init: WantClue's six writes, the ticket
    // mask for difficulty 8, the PLL0 divider and the gekko word for
    // 300 MHz, the UART divider; nothing in 0x10 or 0xA4.
    assert_eq!(chip.register(0x80), 0);
    assert_eq!(chip.register(0x84), 0);
    assert_eq!(chip.register(reg::ORDERED_CLOCK_ENABLE), 1);
    assert_eq!(chip.register(reg::CORE_REGISTER_CONTROL), 0x8000_8074);
    assert_eq!(chip.register(reg::PLL3_PARAMETER), 0xC070_0111);
    assert_eq!(chip.register(reg::FAST_UART_CONFIGURATION), 0x0600_000F);
    assert_eq!(chip.ticket_difficulty(), 1);
    assert_eq!(chip.register(reg::PLL0_DIVIDER), 0x0F0F_0F00);
    assert_eq!(chip.register(reg::PLL0_PARAMETER), 0x4078_0215);
    assert_eq!(chip.frequency_hz(), 300_000_000);
    assert_eq!(b.drv.frequency_hz(), 300_000_000);
    assert_eq!(chip.register(reg::MISC_CONTROL), 0x0000_6131);
    assert_eq!(chip.baud(), 1_562_500);
    assert_eq!(b.uart.host_baud, 1_562_500);
    assert_eq!(chip.register(reg::HASH_COUNTING_NUMBER), 0, "never written");
    assert_eq!(chip.register(reg::VERSION_ROLLING), 0, "never written");
    assert_eq!(b.uart.lost_to_baud, 0, "host and chip switched together");
    // Four versions of 2³² nonces at 300 MHz × 672 small cores: the
    // chip is through a job in well under 100 ms.
    assert!(
        b.drv.work_timeout_us() < 100_000,
        "{}",
        b.drv.work_timeout_us()
    );

    b.mine_ms(5_000);
    let chip = &b.uart.sim.chips()[0];
    assert!(chip.stats().jobs >= 50);
    assert_eq!(chip.job_midstates(), 4, "four midstates per job");
    assert_eq!(chip.stats().violations, 0);
    let sim_nonces = u64::from(chip.stats().nonces);
    assert!(sim_nonces > 50, "{sim_nonces}");
    let yielded = b.count(|e| matches!(e, Event::Nonce(_))) as u64;
    assert_eq!(yielded, sim_nonces);
    assert_eq!(b.drv.stats().unknown_chip, 0);
    // The version of every nonce is the job's base rolled 0–3 times by
    // the BIP 320 mask, and all four midstates get hit.
    let mut seen = [false; 4];
    for e in &b.events {
        if let Event::Nonce(f) = e {
            let k = (f.version - 0x2000_0000) / 0x2000;
            assert!(k < 4, "version {:#x}", f.version);
            assert_eq!(f.version, 0x2000_0000 + k * 0x2000);
            seen[k as usize] = true;
        }
    }
    assert_eq!(seen, [true; 4]);
    // The counter round on this chip is one read, the rate register
    // 0x04 (0x4C is not read: see `REGISTERS_RATE`), the reply nine
    // bytes. The
    // rate comes back in units of 2²⁰ H/s: 300 MHz × 672 = 201.6 GH/s is
    // 192 260; the short window is the rate, the long one the minute
    // figure, both as `hashrateMonitor` reports them.
    let reads_before = chip.stats().reads;
    let w = b.work();
    b.drv.submit(&mut b.uart, &w, b.now).unwrap();
    b.drv.request_counters(&mut b.uart).unwrap();
    b.run_ms(5);
    b.drv.request_counters(&mut b.uart).unwrap();
    b.run_ms(5);
    assert_eq!(b.uart.sim.chips()[0].stats().reads, reads_before + 2);
    let rates: Vec<u32> = b
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Register {
                register: 0x04,
                value,
                ..
            } => Some(*value),
            _ => None,
        })
        .collect();
    assert_eq!(rates, vec![192_260, 0x8000_0000 | 192_260]);
    let m = b.drv.monitor();
    assert_eq!(m.registers(), &[0x04]);
    let c = m.chips()[0];
    assert!((c.total.ghs - 201.6).abs() < 0.01, "{}", c.total.ghs);
    assert!((c.total.ghs_1m - 201.6).abs() < 0.01, "{}", c.total.ghs_1m);
    assert_eq!(c.error.value, 0);
    assert_eq!(m.domains(), 0);
    assert!((m.total_ghs() - 201.6).abs() < 0.01);
}

/// The chips' hash counters read back the way ESP-Miner's hashrate
/// monitor reads them: a second reading a second or more after the
/// first gives each chip's rate, the domains add up to the total, the
/// error counter follows the injected error rate, and a chip that does
/// not answer keeps its old reading while the others carry on.
#[test]
fn a_bm1373_chain_reads_eight_domains_that_add_up_to_the_total() {
    let config = Config {
        expected_chips: 2,
        difficulty: 32,
        ..Config::for_profile(&BM1373)
    };
    let mut b = Bench::with(&BM1373, 2, config);
    b.run_until_ready(40_000);
    b.mine_ms(300);
    b.drv.request_counters(&mut b.uart).unwrap();
    b.mine_ms(1500);
    b.drv.request_counters(&mut b.uart).unwrap();
    b.mine_ms(100);
    let m = b.drv.monitor();
    assert_eq!(m.domains(), 8);
    for (i, c) in m.chips().iter().enumerate() {
        let ghs = f64::from(c.total.ghs);
        assert!(ghs > 100.0, "chip {i}: {ghs}");
        let domains: f64 = (0..8).map(|d| f64::from(m.domain_ghs(i, d))).sum();
        assert!(
            (domains - ghs).abs() / ghs < 0.05,
            "chip {i}: eight domains {domains} vs total {ghs}, no scale"
        );
        for d in 0..8 {
            let share = f64::from(m.domain_ghs(i, d)) / ghs;
            assert!((share - 0.125).abs() < 0.03, "chip {i} domain {d}: {share}");
        }
    }
}

#[test]
fn counter_reads_go_out_one_per_poll_not_as_a_burst() {
    let mut b = Bench::new(4, Bench::config());
    b.run_until_ready(20_000);
    b.mine_ms(300);
    let reads_before: u32 = (0..4).map(|i| b.uart.sim.chip_mut(i).stats().reads).sum();
    b.drv.request_counters(&mut b.uart).unwrap();
    assert!(b.drv.counters_pending());
    // Nothing went out with the request itself; one read per poll.
    let reads_now: u32 = (0..4).map(|i| b.uart.sim.chip_mut(i).stats().reads).sum();
    assert_eq!(reads_now, reads_before);
    b.mine_ms(1);
    let after_one: u32 = (0..4).map(|i| b.uart.sim.chip_mut(i).stats().reads).sum();
    assert_eq!(after_one, reads_before + 1, "one read per poll");
    b.mine_ms(30);
    assert!(!b.drv.counters_pending());
    let after_all: u32 = (0..4).map(|i| b.uart.sim.chip_mut(i).stats().reads).sum();
    assert_eq!(after_all, reads_before + 4 * 6, "4 chips × 6 registers");
    // The round is complete and the monitor has every chip's registers.
    b.mine_ms(1200);
    b.drv.request_counters(&mut b.uart).unwrap();
    b.mine_ms(100);
    for i in 0..4 {
        assert!(b.drv.monitor().chips()[i].total.ghs > 0.0, "chip {i}");
    }
}

#[test]
fn a_single_chip_register_read_is_answered_by_that_chip_alone() {
    let mut b = Bench::new(4, Bench::config());
    b.run_until_ready(20_000);
    b.mine_ms(300);
    let before = b.count(|e| matches!(e, Event::Register { .. }));
    b.drv
        .read_chip_register(&mut b.uart, 2, reg::TOTAL_COUNT)
        .unwrap();
    b.mine_ms(200);
    let replies: Vec<(u8, u8)> = b
        .events
        .iter()
        .skip(before)
        .filter_map(|e| match e {
            Event::Register {
                address, register, ..
            } => Some((*address, *register)),
            _ => None,
        })
        .collect();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, reg::TOTAL_COUNT);
    assert_eq!(b.drv.chip_index(replies[0].0), Some(2));
    // An index the chain does not have sends nothing.
    b.drv
        .read_chip_register(&mut b.uart, 7, reg::TOTAL_COUNT)
        .unwrap();
    b.mine_ms(200);
    assert_eq!(b.count(|e| matches!(e, Event::Register { .. })), before + 1);
}

#[test]
fn hash_counters_read_back_like_the_reference_monitor() {
    let mut b = Bench::new(4, Bench::config());
    b.run_until_ready(20_000);
    b.uart
        .sim
        .chip_mut(2)
        .inject(Fault::HashErrors { per_mille: 20 });
    b.mine_ms(300);
    b.drv.request_counters(&mut b.uart).unwrap();
    b.mine_ms(1500);
    b.drv.request_counters(&mut b.uart).unwrap();
    b.mine_ms(100);
    let m = b.drv.monitor();
    assert_eq!(m.chips().len(), 4);
    let expected = b.drv.frequency_hz() as f64 * f64::from(BM1370.small_cores) / 1e9;
    assert!(
        expected > 100.0,
        "{expected} GH/s at {} Hz",
        b.drv.frequency_hz()
    );
    for (i, c) in m.chips().iter().enumerate() {
        let ghs = f64::from(c.total.ghs);
        assert!(
            (ghs - expected).abs() / expected < 0.1,
            "chip {i}: {ghs} GH/s, expected ~{expected}"
        );
        let domains: f64 = (0..4).map(|d| f64::from(m.domain_ghs(i, d))).sum();
        assert!(
            (domains - ghs).abs() / ghs < 0.05,
            "chip {i}: domains {domains} vs total {ghs}"
        );
        if i == 2 {
            assert!(c.error.value > 0, "errors counted");
            let pct = f64::from(c.error.ghs) / ghs * 100.0;
            assert!((pct - 2.0).abs() < 0.5, "chip 2 error rate {pct} %");
        } else {
            assert_eq!(c.error.value, 0, "chip {i} has no errors");
        }
    }
    assert!((f64::from(m.total_ghs()) - 4.0 * expected).abs() / (4.0 * expected) < 0.1);
    assert!(
        m.error_percent() > 0.3 && m.error_percent() < 0.7,
        "{}",
        m.error_percent()
    );
    assert!(b.count(|e| matches!(e, Event::Register { .. })) >= 48);

    // Chip 1 stops answering: its reading stays, the others move on,
    // and nonces keep flowing.
    let before = m.chips()[1].total.time_us;
    let nonces_before = b.count(|e| matches!(e, Event::Nonce(_)));
    b.uart
        .sim
        .chip_mut(1)
        .inject(Fault::NoResponse { for_us: 10_000_000 });
    b.drv.request_counters(&mut b.uart).unwrap();
    b.mine_ms(1500);
    b.drv.request_counters(&mut b.uart).unwrap();
    b.mine_ms(100);
    let m = b.drv.monitor();
    assert_eq!(
        m.chips()[1].total.time_us,
        before,
        "no new reading from chip 1"
    );
    assert!(m.chips()[0].total.time_us > before, "chip 0 was read again");
    assert!(b.count(|e| matches!(e, Event::Nonce(_))) > nonces_before);
}

// ------------------------------------------------------------ self-test

/// The self-test's hash window against the simulated chain: what the
/// firmware does once a second (request the counters, sample the
/// monitor), done here in simulated time, then judged by
/// `warpaxe_board::selftest`. A chip whose counters are older than five
/// seconds has stopped answering (`fresh = false`).
mod selftest_window {
    use super::*;
    use warpaxe_board::selftest::{judge, BoardReadings, Check, Limits, Note, Status, Window};
    use warpaxe_board::Asic;

    const STALE_US: u64 = 5_000_000;

    fn readings(chips: u8) -> BoardReadings {
        BoardReadings {
            chips_found: chips,
            chips_expected: chips,
            hashing: true,
            has_regulator: true,
            core_set_mv: 1150,
            core_actual_mv: Some(1148),
            regulator_fault: false,
            has_thermal: true,
            chip_temp_c: Some(58.0),
            chip_limit_c: 75.0,
            vr_temp_c: Some(60.0),
            vr_limit_c: 105.0,
            vin_v: Some(5.1),
            nominal_v: 5.0,
        }
    }

    /// Mine for `seconds`, sampling the window once a second as the
    /// firmware's self-test does.
    fn run_window(b: &mut Bench, seconds: u32) -> Window {
        let m = b.drv.monitor();
        let expected = b.drv.frequency_hz() as f32 * f32::from(BM1370.small_cores) / 1e9;
        let mut w = Window::new(
            b.drv.chip_count(),
            m.domains() as u8,
            expected,
            &Limits::DEFAULT,
        );
        for _ in 0..seconds {
            b.drv.request_counters(&mut b.uart).unwrap();
            b.mine_ms(1000);
            let m = b.drv.monitor();
            for (i, c) in m.chips().iter().enumerate() {
                let domains: Vec<f32> = (0..m.domains()).map(|d| m.domain_ghs(i, d)).collect();
                let fresh = b.now.saturating_sub(c.total.time_us) < STALE_US;
                w.sample(i, c.total.ghs, c.error.ghs, &domains, fresh);
            }
        }
        w
    }

    #[test]
    fn a_healthy_chain_passes_the_hash_checks() {
        let mut b = Bench::new(4, Bench::config());
        b.run_until_ready(20_000);
        b.mine_ms(2_000);
        let w = run_window(&mut b, 20);
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &readings(4), Some(&w), None);
        for c in [Check::Chips, Check::Hashrate, Check::Domains, Check::Errors] {
            let o = r.get(c);
            assert_eq!(o.status, Status::Pass, "{o:?}");
        }
        assert!(r.get(Check::Hashrate).value.unwrap() > 90.0, "{r:?}");
        assert!(r.get(Check::Domains).value.unwrap() > 90.0, "{r:?}");
    }

    #[test]
    fn a_hung_chip_fails_the_hashrate_check_by_name() {
        let mut b = Bench::new(4, Bench::config());
        b.run_until_ready(20_000);
        // Chip 2 answers registers but hashes nothing (the silent VR
        // shutdown's chip side, or an over-clock hang).
        b.uart.sim.chip_mut(2).inject(Fault::HangAbove { hz: 1 });
        b.mine_ms(2_000);
        let w = run_window(&mut b, 20);
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &readings(4), Some(&w), None);
        let h = r.get(Check::Hashrate);
        assert_eq!((h.status, h.chip), (Status::Fail, Some(2)), "{h:?}");
        assert!(h.value.unwrap() < 10.0, "{h:?}");
        assert_eq!(h.note, Note::None, "it answered, it just did not hash");
        let d = r.get(Check::Domains);
        assert_eq!((d.status, d.chip), (Status::Fail, Some(2)), "{d:?}");
        assert!(!r.passed());
    }

    #[test]
    fn a_chip_that_stops_answering_is_reported_as_no_reply() {
        let mut b = Bench::new(2, Bench::config());
        b.run_until_ready(20_000);
        b.mine_ms(2_000);
        b.uart.sim.chip_mut(1).inject(Fault::NoResponse {
            for_us: 120_000_000,
        });
        let w = run_window(&mut b, 20);
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &readings(2), Some(&w), None);
        let h = r.get(Check::Hashrate);
        assert_eq!(
            (h.status, h.chip, h.note),
            (Status::Fail, Some(1), Note::NoReply),
            "{h:?}"
        );
        assert_eq!(r.get(Check::Hashrate).value, Some(0.0));
    }

    #[test]
    fn a_weak_domain_fails_the_domain_check_and_names_chip_and_domain() {
        let mut b = Bench::new(4, Bench::config());
        b.run_until_ready(20_000);
        // Chip 1, domain 3 at a third of its share: the chip's total
        // drops to ~83 %, still above the chip limit, so only the domain
        // check must catch it.
        b.uart.sim.chip_mut(1).inject(Fault::WeakDomain {
            domain: 3,
            per_mille: 333,
        });
        b.mine_ms(2_000);
        let w = run_window(&mut b, 30);
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &readings(4), Some(&w), None);
        let d = r.get(Check::Domains);
        assert_eq!(
            (d.status, d.chip, d.domain),
            (Status::Fail, Some(1), Some(3)),
            "{d:?}"
        );
        assert!(d.value.unwrap() < 50.0, "{d:?}");
        assert_eq!(
            r.get(Check::Hashrate).status,
            Status::Pass,
            "{:?}",
            r.get(Check::Hashrate)
        );
        assert_eq!(r.failed(), 1);
    }

    #[test]
    fn hash_errors_above_the_chip_limit_fail_the_error_check() {
        let mut b = Bench::new(2, Bench::config());
        b.run_until_ready(20_000);
        b.uart
            .sim
            .chip_mut(0)
            .inject(Fault::HashErrors { per_mille: 70 });
        b.mine_ms(2_000);
        let w = run_window(&mut b, 20);
        let r = judge(Asic::Bm1370, &Limits::DEFAULT, &readings(2), Some(&w), None);
        let e = r.get(Check::Errors);
        assert_eq!((e.status, e.chip), (Status::Fail, Some(0)), "{e:?}");
        assert!((e.value.unwrap() - 7.0).abs() < 1.0, "{e:?}");
        // The same chip judged as a BM1373 would pass (8 % limit).
        let r = judge(Asic::Bm1373, &Limits::DEFAULT, &readings(2), Some(&w), None);
        assert_eq!(r.get(Check::Errors).status, Status::Pass);
    }
}

/// The first chain start after a board choice (firmware `board::gate`,
/// `miner.rs`): the enumeration decides whether the choice stands.
mod board_choice {
    use super::*;
    use warpaxe_board::choice::{first_start_verdict, FirstStart};

    fn enumerated(b: &Bench) -> Option<u8> {
        b.events.iter().find_map(|e| match e {
            Event::Enumerated { count } => Some(*count),
            _ => None,
        })
    }

    fn unexpected(b: &Bench) -> Option<u16> {
        b.events.iter().find_map(|e| match e {
            Event::UnexpectedChip { chip_id } => Some(*chip_id),
            _ => None,
        })
    }

    /// Two BM1370 answer where the chosen profile has four (a NerdQAxe++
    /// picked for a two-chip board): the count refuses the choice; the
    /// same chain under a two-chip profile is verified.
    #[test]
    fn a_wrong_chip_count_refuses_the_choice() {
        let mut b = Bench::new(2, Bench::config());
        b.run_until_ready(20_000);
        let count = enumerated(&b).expect("enumerated");
        assert_eq!(count, 2);
        assert_eq!(unexpected(&b), None);
        // Two is a sibling's count (a two-chip board on the same bus):
        // another board. Nobody's count: chips missing, the choice stands.
        assert_eq!(
            first_start_verdict(count, 4, unexpected(&b), &[2]),
            FirstStart::WrongCount {
                found: 2,
                expected: 4
            }
        );
        assert_eq!(
            first_start_verdict(count, 4, unexpected(&b), &[]),
            FirstStart::ShortCount {
                found: 2,
                expected: 4
            }
        );
        assert_eq!(
            first_start_verdict(count, 2, unexpected(&b), &[]),
            FirstStart::Verified { chips: 2 }
        );
    }

    /// Chips of another type under the chosen profile's driver (a Naja
    /// Duo picked as a Gamma Hex, a Q1373 as a Q1370, or the other way
    /// round): nothing is counted. The BM1373 answers the BM1370 driver
    /// with its 9-byte id frames, which the 11-byte decoder cannot read
    /// (NoChips); the BM1370's 11-byte frames under the BM1373 driver
    /// carry an id the profile does not know (UnexpectedChip, then
    /// NoChips). Either way the verdict refuses the choice.
    #[test]
    fn a_chip_of_another_type_refuses_the_choice() {
        for (driver, chips) in [(&BM1370, &BM1373), (&BM1373, &BM1370)] {
            let mut drv = Chain::new(driver, Config::for_profile(driver));
            drv.start(0);
            let mut b = Bench {
                drv,
                uart: SimUart::with(chips, 2, 7),
                now: 0,
                events: Vec::new(),
                works: 0,
            };
            b.run_until_ready(20_000);
            assert_eq!(enumerated(&b), None, "{} under {}", chips.name, driver.name);
            assert!(
                b.has(|e| matches!(e, Event::InitFailed(InitError::NoChips))),
                "{} under {}: {:?}",
                chips.name,
                driver.name,
                b.events
            );
            let verdict = first_start_verdict(0, 6, unexpected(&b), &[2]);
            assert!(
                matches!(verdict, FirstStart::NoChips | FirstStart::WrongChip { .. }),
                "{verdict:?}"
            );
            eprintln!("{} under {}: {verdict:?}", chips.name, driver.name);
        }
    }

    /// A dead chain (nothing answers) is not a wrong board by itself:
    /// the firmware gives it three inits, then refuses with NoChips.
    #[test]
    fn a_silent_chain_counts_as_no_chips() {
        let mut b = Bench::new(1, Bench::config());
        b.uart
            .sim
            .chip_mut(0)
            .inject(Fault::NoResponse { for_us: 60_000_000 });
        b.run_until_ready(20_000);
        assert_eq!(b.drv.phase(), Phase::Failed);
        assert_eq!(enumerated(&b), None);
        assert_eq!(first_start_verdict(0, 1, None, &[]), FirstStart::NoChips);
    }
}

// ---------------------------------------------------------- domain latch

/// The domain-latch policy against the simulated chain: what the
/// firmware does once a second (request the counters, hand the monitor's
/// domain rates to [`DomainLatch`]), in simulated time. A chip whose
/// counters are older than five seconds is not fresh.
mod domain_latch {
    use super::*;
    use warpaxe_chain::{DomainLatch, LatchSample, LatchVerdict};

    const STALE_US: u64 = 5_000_000;

    /// Mine for `seconds`, observing once a second; the verdicts with
    /// the simulated second they came at.
    fn observe(b: &mut Bench, p: &mut DomainLatch, seconds: u64) -> Vec<(u64, LatchVerdict)> {
        let mut out = Vec::new();
        for _ in 0..seconds {
            b.drv.request_counters(&mut b.uart).unwrap();
            b.mine_ms(1000);
            let m = b.drv.monitor();
            let rates: Vec<Vec<f32>> = (0..m.chips().len())
                .map(|i| (0..m.domains()).map(|d| m.domain_ghs(i, d)).collect())
                .collect();
            let samples: Vec<LatchSample> = m
                .chips()
                .iter()
                .zip(b.drv.chips())
                .zip(&rates)
                .map(|((c, st), r)| LatchSample {
                    silent: st.silent,
                    fresh: b.now.saturating_sub(c.total.time_us) < STALE_US,
                    domains: r,
                })
                .collect();
            let now_s = b.now / 1_000_000;
            if let Some(v) = p.observe(now_s, b.drv.accepting_work(), &samples) {
                out.push((now_s, v));
            }
        }
        out
    }

    #[test]
    fn a_latched_domain_is_caught_after_ten_minutes_and_named() {
        let mut b = Bench::new(4, Bench::config());
        b.run_until_ready(20_000);
        // Chip 1, domain 2 at 3 % of its share: the chip still answers
        // and finds nonces, only that domain's counter crawls.
        b.uart.sim.chip_mut(1).inject(Fault::WeakDomain {
            domain: 2,
            per_mille: 30,
        });
        b.mine_ms(2_000);
        let mut p = DomainLatch::new();
        let start = b.now / 1_000_000;
        let v = observe(&mut b, &mut p, 700);
        assert_eq!(v.len(), 1, "{v:?}");
        let (at, verdict) = v[0];
        let LatchVerdict::Reset {
            chip,
            domain,
            percent,
        } = verdict
        else {
            panic!("{verdict:?}");
        };
        assert_eq!((chip, domain), (1, 2));
        assert!((2..=4).contains(&percent), "percent {percent}");
        // The 600th fresh sample; the first read or two carry no rate
        // yet but are fresh all the same.
        assert!(
            (600..=610).contains(&(at - start)),
            "at {at}, start {start}"
        );
        assert!(!b.drv.chips()[1].silent, "the chip itself is not silent");
    }

    #[test]
    fn a_weak_domain_at_half_its_share_does_not_trigger() {
        let mut b = Bench::new(4, Bench::config());
        b.run_until_ready(20_000);
        b.uart.sim.chip_mut(1).inject(Fault::WeakDomain {
            domain: 2,
            per_mille: 500,
        });
        b.mine_ms(2_000);
        let mut p = DomainLatch::new();
        // Two full windows and change.
        let v = observe(&mut b, &mut p, 1300);
        assert!(v.is_empty(), "{v:?}");
        assert_eq!(p.given_up().count(), 0);
    }

    #[test]
    fn a_healthy_chain_is_left_alone() {
        let mut b = Bench::new(2, Bench::config());
        b.run_until_ready(20_000);
        b.mine_ms(2_000);
        let mut p = DomainLatch::new();
        assert!(observe(&mut b, &mut p, 650).is_empty());
    }
}

// ------------------------------------------------------------ core hold

/// A lower core voltage that comes with a lower clock, against the
/// simulated chain: the mining loop's side (hand the request to the
/// driver, report the clock) and the board task's side (decide every
/// 100 ms) as the firmware runs them, in simulated time. What counts is
/// the clock the simulated chips really run at when the voltage goes.
mod core_hold {
    use super::*;
    use warpaxe_board::core_hold::{Clock, CoreHold, Step, MAX_HOLD_MS};

    struct Rig {
        b: Bench,
        wanted_mhz: u32,
        handed_mhz: u32,
        settled_mhz: u32,
        hold: CoreHold,
        applied_mv: u16,
        wanted_mv: u16,
    }

    /// The voltage went: when (ms after the request), which step, and
    /// the fastest clock among the simulated chips at that moment.
    #[derive(Debug, Clone, Copy)]
    struct Went {
        after_ms: u64,
        step: Step,
        chips_hz: u32,
    }

    impl Rig {
        /// A chain of `chips`, up and settled at the profile's clock
        /// (525 MHz), 1200 mV applied.
        fn up(chips: u8) -> Rig {
            let mut b = Bench::new(chips, Bench::config());
            b.run_until_ready(20_000);
            let mhz = b.drv.frequency_hz() / 1_000_000;
            Rig {
                b,
                wanted_mhz: mhz,
                handed_mhz: mhz,
                settled_mhz: mhz,
                hold: CoreHold::new(),
                applied_mv: 1200,
                wanted_mv: 1200,
            }
        }

        fn chips_hz(&self) -> u32 {
            self.b
                .uart
                .sim
                .chips()
                .iter()
                .map(|c| c.frequency_hz().min(u64::from(u32::MAX)) as u32)
                .max()
                .unwrap_or(0)
        }

        /// The mining loop's pass (every simulated millisecond).
        fn miner_pass(&mut self) {
            if self.wanted_mhz != self.handed_mhz {
                // A new request: what had settled no longer has.
                self.settled_mhz = 0;
                self.b.drv.set_frequency(self.wanted_mhz * 1_000_000);
                self.handed_mhz = self.wanted_mhz;
            }
            if self.handed_mhz == self.wanted_mhz && self.b.drv.clock_settled() {
                self.settled_mhz = self.wanted_mhz;
            }
        }

        /// The board task's pass (every 100 ms): `Some` when the
        /// voltage was set in it.
        fn board_pass(&mut self, now_ms: u64) -> Option<Step> {
            let clock = Clock {
                wanted_mhz: self.wanted_mhz,
                settled_mhz: self.settled_mhz,
                now_hz: self.b.drv.frequency_hz(),
                unreachable: false,
            };
            let step = self
                .hold
                .decide(now_ms, self.applied_mv, self.wanted_mv, clock);
            if step != Step::Hold && self.wanted_mv != self.applied_mv {
                self.applied_mv = self.wanted_mv;
                return Some(step);
            }
            None
        }

        /// Run for `ms`; the first time the voltage goes.
        fn run(&mut self, ms: u64) -> Option<Went> {
            let mut went = None;
            for t in 1..=ms {
                self.miner_pass();
                self.b.run_ms(1);
                if t % 100 == 0 {
                    if let Some(step) = self.board_pass(t) {
                        went.get_or_insert(Went {
                            after_ms: t,
                            step,
                            chips_hz: self.chips_hz(),
                        });
                    }
                }
            }
            went
        }
    }

    #[test]
    fn a_lower_voltage_waits_until_the_chips_run_at_the_lower_clock() {
        let mut r = Rig::up(4);
        assert_eq!(r.chips_hz(), 525_000_000);
        // PATCH: 525 → 425 MHz, 1200 → 1100 mV, both at once.
        r.wanted_mhz = 425;
        r.wanted_mv = 1100;
        let went = r.run(5_000).expect("the voltage goes");
        assert_eq!(went.step, Step::Apply);
        // 100 MHz in 6.25 MHz steps every 100 ms: 1.6 s, then the ramp
        // closes; the board's next pass after that.
        assert!(
            (1_600..=2_000).contains(&went.after_ms),
            "after {} ms",
            went.after_ms
        );
        assert_eq!(went.chips_hz, 425_000_000, "every chip at the new clock");
        assert_eq!(r.applied_mv, 1100);
        assert!(r.b.drv.clock_settled());
    }

    #[test]
    fn until_then_the_old_voltage_stands_at_every_clock_above_the_new_one() {
        let mut r = Rig::up(2);
        r.wanted_mhz = 425;
        r.wanted_mv = 1100;
        // Pass by pass: as long as any chip clocks above 425 MHz the
        // voltage is the old one.
        for t in 1..=4_000u64 {
            r.miner_pass();
            r.b.run_ms(1);
            if t % 100 == 0 {
                r.board_pass(t);
            }
            if r.chips_hz() > 425_000_000 {
                assert_eq!(r.applied_mv, 1200, "at {t} ms, {} Hz", r.chips_hz());
            }
        }
        assert_eq!(r.applied_mv, 1100);
    }

    #[test]
    fn a_raise_goes_at_once_whatever_the_clock_does() {
        // Up with the clock: the voltage first, as before.
        let mut r = Rig::up(2);
        r.wanted_mhz = 600;
        r.wanted_mv = 1250;
        let went = r.run(3_000).expect("the voltage goes");
        assert_eq!((went.after_ms, went.step), (100, Step::Apply));
        assert!(went.chips_hz < 600_000_000, "{}", went.chips_hz);
        // Down with the clock, up with the voltage: at once too.
        let mut r = Rig::up(2);
        r.wanted_mhz = 425;
        r.wanted_mv = 1250;
        let went = r.run(3_000).expect("the voltage goes");
        assert_eq!(went.after_ms, 100);
    }

    #[test]
    fn a_voltage_change_alone_goes_at_once() {
        let mut r = Rig::up(2);
        r.wanted_mv = 1100;
        let went = r.run(1_000).expect("the voltage goes");
        assert_eq!((went.after_ms, went.step), (100, Step::Apply));
        assert_eq!(went.chips_hz, 525_000_000);
    }

    #[test]
    fn the_bring_up_voltage_drops_at_once_while_the_chain_ramps_up() {
        // A chain that has just started: the bring-up voltage (1250 mV)
        // goes down to the configured one right after the init
        // (`board::chain_ready`), during the ramp up to the target.
        let b = Bench::new(2, Bench::config());
        let mut r = Rig {
            b,
            wanted_mhz: 525,
            handed_mhz: 525,
            settled_mhz: 0,
            hold: CoreHold::new(),
            applied_mv: 1250,
            wanted_mv: 1250,
        };
        // Into the ramp: some PLL words written, the target not reached.
        while r.chips_hz() < 100_000_000 {
            r.miner_pass();
            r.b.run_ms(1);
        }
        assert!(!r.b.drv.clock_settled());
        r.wanted_mv = 1150;
        let went = r.run(500).expect("the voltage goes");
        assert_eq!((went.after_ms, went.step), (100, Step::Apply));
        assert!(went.chips_hz < 525_000_000);
    }

    #[test]
    fn a_second_lower_clock_during_the_ramp_keeps_the_voltage_waiting() {
        let mut r = Rig::up(2);
        r.wanted_mhz = 475;
        r.wanted_mv = 1150;
        assert!(r.run(400).is_none(), "still ramping to 475");
        // Further down before the first ramp is over.
        r.wanted_mhz = 425;
        r.wanted_mv = 1100;
        let went = r.run(5_000).expect("the voltage goes");
        assert_eq!(went.chips_hz, 425_000_000);
        assert_eq!(r.applied_mv, 1100);
    }

    #[test]
    fn back_to_an_earlier_clock_in_the_middle_of_a_ramp_up_still_waits() {
        // Settled at 425 MHz.
        let mut r = Rig::up(2);
        r.wanted_mhz = 425;
        r.wanted_mv = 1100;
        r.run(4_000).expect("the voltage goes");
        assert_eq!((r.settled_mhz, r.applied_mv), (425, 1100));
        // Up to 525 MHz with more voltage: at once.
        r.wanted_mhz = 525;
        r.wanted_mv = 1200;
        let went = r.run(800).expect("the voltage goes");
        assert_eq!(went.after_ms, 100);
        assert!(
            r.chips_hz() > 440_000_000,
            "on the way up: {}",
            r.chips_hz()
        );
        assert!(!r.b.drv.clock_settled());
        // Back to 425 MHz and 1100 mV before the ramp up is over: the
        // clock that had settled at 425 is not where the chips are.
        r.wanted_mhz = 425;
        r.wanted_mv = 1100;
        let went = r.run(4_000).expect("the voltage goes");
        assert_eq!(went.chips_hz, 425_000_000, "after {} ms", went.after_ms);
        assert!(went.after_ms > 100);
    }

    #[test]
    fn a_lower_clock_during_the_bring_up_becomes_the_ramp_s_target() {
        // The chain is still coming up towards 525 MHz when 425 MHz and
        // a lower voltage are asked for: the request goes to the chain
        // at once, the bring-up ramp ends at 425, and the chips never
        // run above it, so the voltage has nothing to wait for.
        let b = Bench::new(2, Bench::config());
        let mut r = Rig {
            b,
            wanted_mhz: 525,
            handed_mhz: 525,
            settled_mhz: 0,
            hold: CoreHold::new(),
            applied_mv: 1200,
            wanted_mv: 1200,
        };
        while r.chips_hz() < 100_000_000 {
            r.miner_pass();
            r.b.run_ms(1);
        }
        r.wanted_mhz = 425;
        r.wanted_mv = 1100;
        let went = r.run(300).expect("the voltage goes");
        assert_eq!(went.after_ms, 100);
        let mut top = r.chips_hz();
        for _ in 0..12_000 {
            r.miner_pass();
            r.b.run_ms(1);
            top = top.max(r.chips_hz());
        }
        assert!(r.b.drv.clock_settled());
        assert_eq!(top, 425_000_000, "never above the clock asked for");
        assert_eq!(r.b.drv.frequency_hz(), 425_000_000);
    }

    /// A chip whose bring-up ends with the baud switch (BM1373, BM1397):
    /// a clock asked for while the driver waits out the settle time.
    fn request_in_the_baud_settle(profile: &'static ChipProfile, from_hz: u32, to_hz: u32) {
        let config = Config {
            frequency_hz: from_hz,
            ..Config::for_profile(profile)
        };
        let mut b = Bench::with(profile, 2, config);
        // The bring-up ramp is over, the chain not ready yet.
        while !b.has(|e| matches!(e, Event::FrequencyApplied { .. })) {
            b.run_ms(1);
            assert!(b.now < 60_000_000, "{}: no ramp end", profile.name);
        }
        assert!(
            !b.drv.accepting_work(),
            "{}: in the settle time",
            profile.name
        );
        assert_eq!(b.drv.frequency_hz(), from_hz);
        b.drv.set_frequency(to_hz).expect("a PLL setting");
        b.run_until_ready(5_000);
        // Ready, and not settled: the request gets its ramp.
        assert!(!b.drv.clock_settled(), "{}", profile.name);
        for _ in 0..20_000 {
            b.run_ms(1);
            if b.drv.clock_settled() {
                break;
            }
        }
        assert!(b.drv.clock_settled(), "{}", profile.name);
        assert_eq!(b.drv.frequency_hz(), to_hz, "{}", profile.name);
        for c in b.uart.sim.chips() {
            assert_eq!(c.frequency_hz(), u64::from(to_hz), "{}", profile.name);
        }
    }

    #[test]
    fn a_clock_asked_for_during_the_baud_settle_is_ramped_to_afterwards() {
        request_in_the_baud_settle(&BM1373, 425_000_000, 325_000_000);
        request_in_the_baud_settle(&BM1397, 425_000_000, 300_000_000);
    }

    #[test]
    fn a_chain_that_stops_answering_its_ramp_does_not_hold_the_voltage_for_ever() {
        let mut r = Rig::up(2);
        r.wanted_mhz = 425;
        r.wanted_mv = 1100;
        // The mining loop never hands the request on (stuck elsewhere):
        // the chips stay at 525 MHz. The board lets go after the limit.
        let mut went = None;
        for t in (100..=MAX_HOLD_MS + 1_000).step_by(100) {
            if let Some(step) = r.board_pass(t) {
                went = Some((t, step));
                break;
            }
        }
        assert_eq!(went, Some((100 + MAX_HOLD_MS, Step::ApplyLate)));
    }
}

// ---------------------------------------------------------- pool outage

/// No pool answers: the chips go off, and come back when one does. The
/// rule (`warpaxe_stratum::outage`) is driven by a model of the
/// firmware's connect loop (`stratum.rs`: failures 3 s, 6 s, 12 s …
/// apart, the fallback after three on the main pool, both pools in turn
/// every half minute once they are out) against pools that come and go
/// on a script, with the simulated chain behind it: off means the chain
/// is gone, back on means a fresh chain started at the configured clock,
/// as the firmware's chain restart does.
mod pool_outage {
    use super::*;
    use warpaxe_stratum::outage::{next_probe, Change, PoolWatch};

    /// The configured point: not the profile's default, so "the old
    /// values" is a statement.
    const MHZ: u32 = 600;
    const MV: u16 = 1180;

    struct Rig {
        /// The chain while the chips are on.
        chain: Option<Bench>,
        chain_starts: u32,
        /// The board task's applied core voltage; kept while the rail
        /// is off and used again when it comes back.
        applied_mv: u16,
        has_fallback: bool,
        watch: PoolWatch,
        // The connect loop.
        on_fallback: bool,
        fails: u32,
        backoff_s: u64,
        next_attempt_s: u64,
        /// A session is up since (s), on the fallback or not.
        session: Option<(u64, bool)>,
        changes: Vec<(u64, Change)>,
    }

    fn config() -> Config {
        Config {
            frequency_hz: MHZ * 1_000_000,
            ..Bench::config()
        }
    }

    impl Rig {
        fn new(has_fallback: bool) -> Rig {
            let mut r = Rig {
                chain: None,
                chain_starts: 0,
                applied_mv: MV,
                has_fallback,
                watch: PoolWatch::new(),
                on_fallback: false,
                fails: 0,
                backoff_s: 3,
                next_attempt_s: 0,
                session: None,
                changes: Vec::new(),
            };
            r.start_chain();
            r
        }

        fn start_chain(&mut self) {
            let mut b = Bench::new(2, config());
            b.run_until_ready(30_000);
            self.chain = Some(b);
            self.chain_starts += 1;
        }

        fn note(&mut self, t: u64, c: Option<Change>) {
            match c {
                Some(Change::Unavailable) => self.chain = None,
                Some(Change::Available { .. }) => self.start_chain(),
                None => return,
            }
            self.changes.extend(c.map(|c| (t, c)));
        }

        /// One second of the connect loop at `t`, with the pools up or
        /// not as given.
        fn second(&mut self, t: u64, main_up: bool, fallback_up: bool) {
            let up = |fallback: bool| if fallback { fallback_up } else { main_up };
            if let Some((since, fallback)) = self.session {
                if up(fallback) {
                    // The pool sends work.
                    let c = self.watch.answered(t);
                    self.note(t, c);
                } else {
                    // The session breaks: a short one is a failure.
                    self.session = None;
                    if t - since < 60 {
                        self.failure(t, fallback);
                    } else {
                        self.fails = 0;
                        self.backoff_s = 3;
                        self.next_attempt_s = t;
                    }
                }
            } else if t >= self.next_attempt_s {
                if self.has_fallback
                    && !self.on_fallback
                    && self.fails >= 3
                    && !self.watch.unavailable()
                {
                    self.on_fallback = true;
                    self.fails = 0;
                }
                if up(self.on_fallback) {
                    self.session = Some((t, self.on_fallback));
                } else {
                    self.failure(t, self.on_fallback);
                }
            }
            let c = self.watch.tick(t, self.has_fallback);
            self.note(t, c);
            if let Some(b) = self.chain.as_mut() {
                b.mine_ms(1000);
            }
        }

        fn failure(&mut self, t: u64, fallback: bool) {
            if !fallback {
                self.fails += 1;
            }
            let c = self.watch.failed(t, fallback, self.has_fallback);
            self.note(t, c);
            if self.watch.unavailable() {
                let (next, pause) = next_probe(fallback, self.has_fallback);
                self.on_fallback = next;
                self.fails = 0;
                self.next_attempt_s = t + pause.max(1);
                self.backoff_s = 3;
            } else {
                if self.has_fallback && self.watch.alternate() {
                    self.on_fallback = !self.on_fallback;
                    self.fails = 0;
                    self.backoff_s = self.backoff_s.min(12);
                }
                self.next_attempt_s = t + self.backoff_s;
                self.backoff_s = (self.backoff_s * 2).min(60);
            }
        }

        fn run(&mut self, from: u64, to: u64, main_up: bool, fallback_up: bool) {
            for t in from..to {
                self.second(t, main_up, fallback_up);
            }
        }

        fn chips_on(&self) -> bool {
            self.chain.is_some()
        }
    }

    #[test]
    fn both_pools_gone_switches_the_chips_off_after_a_minute_and_one_back_brings_them_on() {
        let mut r = Rig::new(true);
        r.run(0, 300, true, true);
        assert!(r.chips_on());
        assert!(r.changes.is_empty());
        // Both pools gone at 300 s.
        r.run(300, 800, false, false);
        assert!(!r.chips_on(), "chips off");
        assert_eq!(r.changes.len(), 1, "{:?}", r.changes);
        let (off_at, c) = r.changes[0];
        assert_eq!(c, Change::Unavailable);
        // Not before a minute of failures on both, not long after it.
        assert!((360..=420).contains(&off_at), "off at {off_at}");
        // Only the fallback comes back at 800 s: found within one round
        // of probes, and the chain starts over at the configured clock;
        // the voltage is the one the rail had.
        r.run(800, 900, false, true);
        assert!(r.chips_on(), "chips back on");
        let (on_at, c) = r.changes[1];
        assert!(matches!(c, Change::Available { off_s } if off_s == on_at - off_at));
        assert!((800..=835).contains(&on_at), "on at {on_at}");
        assert_eq!(r.chain_starts, 2);
        let b = r.chain.as_ref().unwrap();
        assert!(b.drv.accepting_work());
        assert_eq!(b.drv.frequency_hz(), MHZ * 1_000_000);
        assert_eq!(b.drv.chip_count(), 2);
        assert_eq!(r.applied_mv, MV);
        assert!(
            b.count(|e| matches!(e, Event::Nonce(_))) > 0,
            "hashing again"
        );
        assert_eq!(r.changes.len(), 2);
    }

    #[test]
    fn the_main_pool_back_is_found_too_while_the_fallback_stays_down() {
        let mut r = Rig::new(true);
        r.run(0, 100, true, true);
        r.run(100, 400, false, false);
        assert!(!r.chips_on());
        // The connect loop used to stay on the fallback's backoff here.
        r.run(400, 480, true, false);
        assert!(r.chips_on());
        assert_eq!(r.session.map(|s| s.1), Some(false), "on the main pool");
    }

    #[test]
    fn an_outage_that_begins_on_the_fallback_is_one_too() {
        let mut r = Rig::new(true);
        r.run(0, 100, true, true);
        // The main pool dies; the miner moves to the fallback and runs
        // there for a quarter of an hour.
        r.run(100, 1000, false, true);
        assert!(r.chips_on());
        assert_eq!(r.session.map(|s| s.1), Some(true));
        // Then the internet goes: the fallback is gone as well.
        r.run(1000, 1300, false, false);
        assert!(!r.chips_on(), "chips off");
        let off_at = r.changes[0].0;
        assert!((1060..=1120).contains(&off_at), "off at {off_at}");
        // Everything back: the main pool is found within a round.
        r.run(1300, 1400, true, true);
        assert!(r.chips_on());
        assert_eq!(r.session.map(|s| s.1), Some(false), "on the main pool");
        assert_eq!(r.chain_starts, 2);
    }

    #[test]
    fn a_hiccup_of_the_fallback_while_the_main_pool_is_down_switches_nothing() {
        for blip in [5, 10, 20, 40] {
            let mut r = Rig::new(true);
            r.run(0, 100, true, true);
            r.run(100, 1000, false, true);
            assert_eq!(r.session.map(|s| s.1), Some(true));
            // The fallback gone for `blip` seconds, the main pool still
            // down: both are tried in turn, the fallback is found back.
            r.run(1000, 1000 + blip, false, false);
            r.run(1000 + blip, 1400, false, true);
            assert!(r.changes.is_empty(), "{blip} s: {:?}", r.changes);
            assert_eq!(r.chain_starts, 1, "{blip} s");
            assert_eq!(
                r.session.map(|s| s.1),
                Some(true),
                "{blip} s: on the fallback"
            );
        }
    }

    #[test]
    fn a_short_outage_leaves_the_chips_on() {
        let mut r = Rig::new(true);
        r.run(0, 100, true, true);
        // Wi-Fi gone for 40 s.
        r.run(100, 140, false, false);
        r.run(140, 600, true, true);
        assert!(r.changes.is_empty(), "{:?}", r.changes);
        assert_eq!(r.chain_starts, 1, "the chain was never touched");
        assert!(r.chips_on());
    }

    #[test]
    fn the_main_pool_gone_with_the_fallback_up_is_the_fallback_s_case() {
        let mut r = Rig::new(true);
        r.run(0, 100, true, true);
        r.run(100, 1000, false, true);
        assert!(r.changes.is_empty(), "{:?}", r.changes);
        assert!(r.chips_on());
        assert_eq!(r.session.map(|s| s.1), Some(true), "on the fallback");
    }

    #[test]
    fn without_a_fallback_the_main_pool_alone_decides() {
        let mut r = Rig::new(false);
        r.run(0, 100, true, false);
        r.run(100, 400, false, false);
        assert!(!r.chips_on());
        let off_at = r.changes[0].0;
        assert!((160..=220).contains(&off_at), "off at {off_at}");
        r.run(400, 460, true, false);
        assert!(r.chips_on());
        assert_eq!(r.chain_starts, 2);
    }

    #[test]
    fn no_pool_from_the_start_switches_off_and_the_first_job_switches_on() {
        // A miner booted without Wi-Fi: the chain comes up, no pool ever
        // answers.
        let mut r = Rig::new(false);
        r.run(0, 200, false, false);
        assert!(!r.chips_on());
        assert_eq!(r.changes[0].1, Change::Unavailable);
        r.run(200, 260, true, false);
        assert!(r.chips_on());
    }
}

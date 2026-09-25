//! The driver against the simulator, in-process. Same code path the
//! firmware will run, minus the UART.

use std::collections::VecDeque;
use std::convert::Infallible;

use warpaxe_chain::{Chain, Config, Error, Event, InitError, Phase, Uart, Work};
use warpaxe_core::chip::bm1370::BM1370;
use warpaxe_core::chip::bm1373::BM1373;
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

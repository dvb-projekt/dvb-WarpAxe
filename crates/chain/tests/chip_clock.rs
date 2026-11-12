//! A clock of its own per chip (`Chain::set_chip_offsets`), against the
//! simulator: only the chip that is meant changes its PLL, no chip is
//! ever taken above the clock asked for it, and a chain without offsets
//! puts the same bytes on the wire at the same moments as before there
//! were any.

use std::collections::VecDeque;
use std::convert::Infallible;

use warpaxe_chain::{Chain, Config, Event, Phase, Uart, Work};
use warpaxe_core::chip::bm1370::BM1370;
use warpaxe_core::chip::bm1373::{BM1373, BM1373_ESP, BM1373_ESP_1M};
use warpaxe_core::chip::bm1397::BM1397;
use warpaxe_core::chip::ChipProfile;
use warpaxe_sim::chain::SimChain;

const MHZ: u32 = 1_000_000;

/// The simulated chain behind the `Uart` trait, as in `tests/sim.rs`,
/// with a running digest of everything the driver sends and when.
struct SimUart {
    sim: SimChain,
    rx: VecDeque<u8>,
    host_baud: u32,
    t_us: u64,
    /// FNV-1a over (time of the write, its bytes).
    digest: u64,
    frames: u64,
}

impl SimUart {
    fn new(profile: &'static ChipProfile, chips: u8) -> Self {
        SimUart {
            sim: SimChain::new(profile, chips, 7),
            rx: VecDeque::new(),
            host_baud: 115_200,
            t_us: 0,
            digest: 0xcbf2_9ce4_8422_2325,
            frames: 0,
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
        self.t_us += dt_us;
        let out = self.sim.drain();
        if self.rates_match() {
            self.rx.extend(out);
        }
    }

    fn mix(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.digest ^= u64::from(b);
            self.digest = self.digest.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl Uart for SimUart {
    type Error = Infallible;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
        let t = self.t_us.to_le_bytes();
        self.mix(&t);
        self.mix(bytes);
        self.frames += 1;
        if self.rates_match() {
            self.sim.feed(bytes);
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
        let t = self.t_us.to_le_bytes();
        self.mix(&t);
        self.mix(&baud.to_le_bytes());
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
    /// The highest clock every chip was seen at, Hz.
    top: Vec<u64>,
}

impl Bench {
    fn new(profile: &'static ChipProfile, chips: u8, mhz: u32) -> Self {
        Self::with_offsets(profile, chips, mhz, &[])
    }

    /// A chain with `offsets_mhz` set before its start, as the firmware
    /// hands them over.
    fn with_offsets(
        profile: &'static ChipProfile,
        chips: u8,
        mhz: u32,
        offsets_mhz: &[i16],
    ) -> Self {
        let config = Config {
            frequency_hz: mhz * MHZ,
            ..Config::for_profile(profile)
        };
        Self::build(profile, chips, config, offsets_mhz)
    }

    fn build(
        profile: &'static ChipProfile,
        chips: u8,
        config: Config,
        offsets_mhz: &[i16],
    ) -> Self {
        let mut drv = Chain::new(profile, config);
        if !offsets_mhz.is_empty() {
            drv.set_chip_offsets(offsets_mhz);
        }
        drv.start(0);
        Bench {
            drv,
            uart: SimUart::new(profile, chips),
            now: 0,
            events: Vec::new(),
            works: 0,
            top: vec![0; usize::from(chips)],
        }
    }

    /// One millisecond per pass; work whenever the driver asks for it,
    /// the counters once a second: what the firmware's loop does.
    fn run_ms(&mut self, ms: u64) {
        for _ in 0..ms {
            let Bench {
                drv,
                uart,
                now,
                events,
                ..
            } = self;
            drv.poll(uart, *now, &mut |e| events.push(e)).unwrap();
            if self.drv.needs_work(self.now) {
                self.works += 1;
                let k = self.works;
                let w = Work {
                    starting_nonce: 0,
                    nbits: 0x1703_0ecd,
                    ntime: 0x66aa_0000 + k,
                    merkle_root: [(k & 0xFF) as u8; 32],
                    prev_block_hash: [0x11; 32],
                    version: 0x2000_0000,
                };
                self.drv.submit(&mut self.uart, &w, self.now).unwrap();
            }
            if self.now.is_multiple_of(1_000_000) {
                self.drv.request_counters(&mut self.uart).unwrap();
            }
            self.uart.advance(1000);
            self.now += 1000;
            for (top, c) in self.top.iter_mut().zip(self.uart.sim.chips()) {
                *top = (*top).max(c.frequency_hz());
            }
        }
    }

    fn run_until_settled(&mut self, max_ms: u64) {
        for _ in 0..max_ms {
            self.run_ms(1);
            if self.drv.clock_settled() {
                return;
            }
            assert_ne!(self.drv.phase(), Phase::Failed, "bring-up failed");
        }
        panic!("not settled after {max_ms} ms: {:?}", self.drv.phase());
    }

    fn chip_mhz(&self) -> Vec<f64> {
        self.uart
            .sim
            .chips()
            .iter()
            .map(|c| c.frequency_hz() as f64 / 1e6)
            .collect()
    }
}

/// Bring-up, mining, a ramp up, a ramp down, a request in the middle of
/// a ramp and one the PLL cannot make: everything the driver sends.
fn wire_digest(profile: &'static ChipProfile, chips: u8, mhz: [u32; 4]) -> (u64, u64) {
    let mut b = Bench::new(profile, chips, mhz[0]);
    b.run_until_settled(30_000);
    b.run_ms(3_000);
    b.drv.set_frequency(mhz[1] * MHZ);
    b.run_ms(6_000);
    b.drv.set_frequency(mhz[2] * MHZ);
    b.run_ms(700);
    b.drv.set_frequency(mhz[3] * MHZ);
    b.run_ms(8_000);
    // No PLL setting on the BM1370/73 (refused, nothing sent); the
    // BM1397's formula clamps it and ramps there.
    b.drv.set_frequency(5 * MHZ);
    b.run_ms(7_000);
    assert!(b.drv.clock_settled(), "{}", profile.name);
    (b.uart.digest, b.uart.frames)
}

/// The chains as the boards have them, each with the bytes it put on
/// the wire before a chip could have a clock of its own (main c24b901,
/// 2026-11-12). The table is the proof that a chain without offsets
/// runs exactly as it did: do not update it for a change that is not
/// meant to alter what an existing board sends.
const AS_BEFORE: [(&ChipProfile, u8, [u32; 4], u64, u64); 8] = [
    (&BM1370, 1, [525, 600, 400, 625], 0xe962_28bc_a1e1_0c79, 280),
    (&BM1370, 2, [600, 650, 500, 640], 0x0781_9552_9173_9c38, 445),
    (&BM1370, 4, [590, 625, 550, 600], 0xec92_9445_f120_8316, 751),
    (
        &BM1370,
        8,
        [625, 650, 575, 700],
        0x749e_ef46_dddf_2cc0,
        1391,
    ),
    (&BM1373, 1, [425, 475, 350, 450], 0x17d2_a463_5c1f_130f, 353),
    (
        &BM1373_ESP,
        2,
        [600, 640, 525, 610],
        0xbf83_22f7_ec48_87bf,
        379,
    ),
    (
        &BM1373_ESP_1M,
        2,
        [600, 640, 525, 610],
        0x231f_62fc_e92d_6024,
        379,
    ),
    (&BM1397, 1, [300, 350, 250, 325], 0x2a8e_8946_79cb_ec15, 925),
];

#[test]
fn a_chain_without_offsets_puts_the_same_bytes_on_the_wire_as_before() {
    let mut differ = Vec::new();
    for (profile, chips, mhz, digest, frames) in AS_BEFORE {
        let got = wire_digest(profile, chips, mhz);
        if got != (digest, frames) {
            differ.push(format!(
                "({} {}, {chips}, {mhz:?}, {:#018x}, {}),",
                profile.name, profile.init_source, got.0, got.1
            ));
        }
    }
    assert!(
        differ.is_empty(),
        "the wire changed:\n{}",
        differ.join("\n")
    );
}

/// Per chip: the clock now, and the largest move seen between two
/// passes, MHz.
struct Watch {
    last: Vec<f64>,
    largest_move: f64,
}

impl Watch {
    fn new(b: &Bench) -> Self {
        Watch {
            last: b.chip_mhz(),
            largest_move: 0.0,
        }
    }

    /// Run until the clock has settled, looking at every chip each
    /// millisecond; `ceiling[i]` is what chip `i` must never exceed.
    fn settle(&mut self, b: &mut Bench, ceiling: &[f64], what: &str) {
        for ms in 0..60_000 {
            b.run_ms(1);
            let now = b.chip_mhz();
            for (i, (&f, &was)) in now.iter().zip(&self.last).enumerate() {
                self.largest_move = self.largest_move.max((f - was).abs());
                assert!(
                    f <= ceiling[i] + 0.7,
                    "{what}: chip {i} at {f} MHz, above its {} MHz",
                    ceiling[i]
                );
            }
            self.last = now;
            if b.drv.clock_settled() && ms > 2 {
                return;
            }
        }
        panic!("{what}: not settled");
    }
}

fn pll_writes(b: &Bench) -> Vec<u32> {
    b.uart
        .sim
        .chips()
        .iter()
        .map(|c| c.stats().writes)
        .collect()
}

/// The chains the offsets are for: four and eight BM1370 (NerdQAxe++,
/// NerdOctaxe), two BM1373 under both inits (Naja Duo).
const CHAINS: [(&ChipProfile, u8, u32); 4] = [
    (&BM1370, 4, 600),
    (&BM1370, 8, 625),
    (&BM1373_ESP, 2, 600),
    (&BM1373, 2, 600),
];

#[test]
fn an_offset_moves_the_one_chip_in_ramp_steps_and_no_other_is_written_to() {
    for (profile, chips, base) in CHAINS {
        let name = format!("{} x{chips} ({})", profile.name, profile.init_source);
        let n = usize::from(chips);
        let mut b = Bench::new(profile, chips, base);
        b.run_until_settled(30_000);
        b.run_ms(1_500);
        let at_base = vec![f64::from(base); n];
        assert_eq!(b.chip_mhz(), at_base, "{name}");
        // The last chip but one: never address 0, the one a write to
        // all would look like.
        let k = n - 1;
        let mut offsets = vec![0i16; n];
        offsets[k] = -25;
        let before = pll_writes(&b);
        let frames = b.uart.frames;
        let mut w = Watch::new(&b);
        b.drv.set_chip_offsets(&offsets);
        w.settle(&mut b, &at_base, &name);

        let mut want = at_base.clone();
        want[k] = f64::from(base) - 25.0;
        assert_eq!(b.chip_mhz(), want, "{name}");
        assert!(w.largest_move <= 6.25, "{name}: {} MHz", w.largest_move);
        let hz: Vec<u32> = want.iter().map(|m| (m * 1e6) as u32).collect();
        assert_eq!(b.drv.chip_frequencies_hz(), &hz[..], "{name}");
        assert_eq!(
            b.drv.frequency_hz(),
            base * MHZ,
            "{name}: the chain's clock"
        );
        // Four steps of 6.25 MHz to the one chip; what the others got
        // since is what follows every ramp for all (the hash counting
        // number), and the chip got that too.
        let after = pll_writes(&b);
        for i in 0..n {
            let extra = if i == k { 4 } else { 0 };
            assert_eq!(
                after[i] - before[i],
                after[0] - before[0] + extra,
                "{name}: writes to chip {i}"
            );
        }
        assert!(
            after[0] - before[0] <= 1,
            "{name}: at most the one write to all"
        );
        assert!(
            b.events
                .iter()
                .rev()
                .any(|e| *e == Event::FrequencyApplied { hz: base * MHZ }),
            "{name}"
        );
        // Work and counter reads aside, the wire carried those writes
        // only: 4 to the chip, at most one to all.
        let sent = b.uart.frames - frames;
        assert!(sent >= 4, "{name}");

        // Back to none: the same chip alone, up again.
        let before = pll_writes(&b);
        let mut w = Watch::new(&b);
        b.drv.set_chip_offsets(&[]);
        w.settle(&mut b, &at_base, &name);
        assert_eq!(b.chip_mhz(), at_base, "{name}");
        let after = pll_writes(&b);
        assert_eq!(after[k] - before[k], after[0] - before[0] + 4, "{name}");
    }
}

#[test]
fn offsets_set_before_the_start_are_part_of_the_bring_up_and_no_chip_passes_its_clock() {
    let cases: [(&ChipProfile, u8, u32, &[i16]); 4] = [
        (&BM1370, 4, 590, &[0, 0, 0, -30]),
        (&BM1370, 8, 625, &[0, 0, -25, 0, -50, 0, 0, 25]),
        (&BM1373_ESP, 2, 600, &[0, -40]),
        (&BM1373, 2, 600, &[-15, 0]),
    ];
    for (profile, chips, base, offsets) in cases {
        let name = format!("{} x{chips} ({})", profile.name, profile.init_source);
        let mut b = Bench::with_offsets(profile, chips, base, offsets);
        let want: Vec<f64> = offsets
            .iter()
            .map(|&o| f64::from(base) + f64::from(o))
            .collect();
        let mut w = Watch::new(&b);
        w.settle(&mut b, &want, &name);
        assert_eq!(b.chip_mhz(), want, "{name}");
        assert_eq!(b.drv.frequency_hz(), base * MHZ, "{name}");
        // From the first write on: the highest clock each chip ever had
        // is the one it was to have.
        let top: Vec<f64> = b.top.iter().map(|&hz| hz as f64 / 1e6).collect();
        assert_eq!(top, want, "{name}");
        assert_eq!(b.uart.sim.totals().violations, 0, "{name}");
        // And the chain hashes: every chip returns nonces at its clock.
        b.run_ms(20_000);
        for (i, c) in b.drv.chips().iter().enumerate() {
            assert!(c.nonces > 0, "{name}: chip {i} without a nonce");
            assert!(!c.silent, "{name}: chip {i} silent");
        }
    }
}

#[test]
fn a_new_chain_clock_carries_the_offsets_and_no_chip_passes_its_old_or_new_clock() {
    for (profile, chips, base) in CHAINS {
        let name = format!("{} x{chips} ({})", profile.name, profile.init_source);
        let n = usize::from(chips);
        let mut offsets = vec![0i16; n];
        offsets[n - 1] = -25;
        offsets[0] = 25;
        let own = |base: u32| -> Vec<f64> {
            offsets
                .iter()
                .map(|&o| f64::from(base) + f64::from(o))
                .collect()
        };
        let mut b = Bench::with_offsets(profile, chips, base, &offsets);
        b.run_until_settled(30_000);
        assert_eq!(b.chip_mhz(), own(base), "{name}");

        // Up: no chip above its new clock on the way.
        let up = base + 25;
        let mut w = Watch::new(&b);
        assert!(b.drv.set_frequency(up * MHZ).is_some());
        w.settle(&mut b, &own(up), &name);
        assert_eq!(b.chip_mhz(), own(up), "{name}");
        assert!(w.largest_move <= 6.25, "{name}: {} MHz", w.largest_move);
        assert_eq!(b.drv.frequency_hz(), up * MHZ, "{name}");

        // Down: no chip above the clock it came from.
        let down = base - 50;
        let mut w = Watch::new(&b);
        assert!(b.drv.set_frequency(down * MHZ).is_some());
        w.settle(&mut b, &own(up), &name);
        assert_eq!(b.chip_mhz(), own(down), "{name}");
        assert!(w.largest_move <= 6.25, "{name}: {} MHz", w.largest_move);
        assert_eq!(b.drv.frequency_hz(), down * MHZ, "{name}");
    }
}

#[test]
fn an_offset_asked_for_in_the_middle_of_a_ramp_or_of_the_bring_up_arrives() {
    // In the middle of a ramp down.
    let mut b = Bench::new(&BM1370, 4, 600);
    b.run_until_settled(30_000);
    b.drv.set_frequency(500 * MHZ);
    b.run_ms(450);
    b.drv.set_chip_offsets(&[0, -20, 0, 0]);
    let mut w = Watch::new(&b);
    w.settle(&mut b, &[600.0; 4], "mid-ramp");
    assert_eq!(b.chip_mhz(), [500.0, 480.0, 500.0, 500.0]);

    // During the bring-up, before the chips are counted, and in the
    // settle time after the ramp (the BM1373 under NerdOS's init).
    for at_ms in [5, 400] {
        let mut b = Bench::new(&BM1373, 2, 600);
        b.run_ms(at_ms);
        assert!(!b.drv.accepting_work());
        b.drv.set_chip_offsets(&[0, -30]);
        b.run_until_settled(30_000);
        assert_eq!(b.chip_mhz(), [600.0, 570.0], "at {at_ms} ms");
        assert_eq!(b.top, [600 * u64::from(MHZ), 570 * u64::from(MHZ)]);
    }
    let mut b = Bench::new(&BM1373, 2, 600);
    while !b
        .events
        .iter()
        .any(|e| matches!(e, Event::FrequencyApplied { .. }))
    {
        b.run_ms(1);
    }
    assert!(!b.drv.accepting_work(), "in the settle time");
    b.drv.set_chip_offsets(&[-30, 0]);
    b.run_until_settled(30_000);
    assert_eq!(b.chip_mhz(), [570.0, 600.0]);
}

#[test]
fn an_offset_without_a_pll_setting_or_past_the_chain_leaves_the_chips_at_the_chains_clock() {
    let mut b = Bench::new(&BM1370, 4, 600);
    b.run_until_settled(30_000);
    let before = pll_writes(&b);
    // 600 − 600 MHz is no clock; entries past the fourth chip are no
    // chip's.
    b.drv.set_chip_offsets(&[0, -600, 0, 0, -25, -25]);
    b.run_ms(2_000);
    assert!(b.drv.clock_settled());
    assert_eq!(b.chip_mhz(), [600.0; 4]);
    let after = pll_writes(&b);
    for i in 0..4 {
        assert!(after[i] - before[i] <= 1, "chip {i}");
    }
    assert!(!b
        .events
        .iter()
        .any(|e| matches!(e, Event::FrequencyUnreachable { .. })));
}

#[test]
fn the_counters_and_the_nonces_of_a_chip_follow_its_own_clock() {
    let offsets = [0i16, 0, -100, 0];
    // A low ticket mask, so the nonces are many enough to compare.
    let config = Config {
        frequency_hz: 600 * MHZ,
        difficulty: 8,
        ..Config::for_profile(&BM1370)
    };
    let mut b = Bench::build(&BM1370, 4, config, &offsets);
    b.run_until_settled(30_000);
    b.run_ms(150_000);
    let mon = b.drv.monitor();
    for (i, c) in mon.chips().iter().enumerate() {
        let hz = b.drv.chip_frequencies_hz()[i];
        let expected = f64::from(hz) * f64::from(BM1370.small_cores) / 1e9;
        let ratio = f64::from(c.total.ghs_1m) / expected;
        assert!(
            (0.97..=1.03).contains(&ratio),
            "chip {i}: {} GH/s against {expected} at {hz} Hz",
            c.total.ghs_1m
        );
    }
    // Nonces: the slow chip has about 500/600 of the others' mean.
    let n: Vec<f64> = b.drv.chips().iter().map(|c| c.nonces as f64).collect();
    let others = (n[0] + n[1] + n[3]) / 3.0;
    let ratio = n[2] / others;
    assert!((0.78..=0.89).contains(&ratio), "{n:?}");
    assert!(b.drv.chips().iter().all(|c| !c.silent));
    assert_eq!(b.drv.stats().duplicates, 0);
}

#[test]
fn the_nonce_space_is_sized_for_the_fastest_chip() {
    use warpaxe_core::chip::Hcn;
    use warpaxe_core::regs::{self, reg};
    let Hcn::Computed { errata } = BM1370.hcn else {
        panic!("the BM1370 computes its hash counting number");
    };
    let hcn = |hz: u32| regs::hash_counting_number(hz, BM1370.cores, 4, errata);
    let mut b = Bench::new(&BM1370, 4, 600);
    b.run_until_settled(30_000);
    let plain = b.drv.work_timeout_us();
    for c in b.uart.sim.chips() {
        assert_eq!(c.register(reg::HASH_COUNTING_NUMBER), hcn(600 * MHZ));
    }
    // A slower chip changes nothing for the others.
    b.drv.set_chip_offsets(&[0, -50, 0, 0]);
    b.run_until_settled(30_000);
    assert_eq!(b.drv.work_timeout_us(), plain);
    for c in b.uart.sim.chips() {
        assert_eq!(c.register(reg::HASH_COUNTING_NUMBER), hcn(600 * MHZ));
    }
    // A faster one: work sooner, and the number for its clock on all.
    b.drv.set_chip_offsets(&[0, -50, 0, 50]);
    b.run_until_settled(30_000);
    assert!(b.drv.work_timeout_us() < plain);
    for c in b.uart.sim.chips() {
        assert_eq!(c.register(reg::HASH_COUNTING_NUMBER), hcn(650 * MHZ));
    }
}

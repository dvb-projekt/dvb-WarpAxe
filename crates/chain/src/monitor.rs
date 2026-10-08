//! The chips' own hash counters, read back as AxeOS's `hashrateMonitor`
//! does it (ESP-Miner `main/tasks/hashrate_monitor_task.c`, GPLv3): every
//! second the driver asks each chip for its total counter, its four
//! domain counters and its error counter; the difference between two
//! readings over the time between them is the chip's hashrate. The
//! counters advance by one per 2^32 hashes (`HASH_CNT_LSB` in ESP-Miner
//! `components/stratum/utils.c`) and wrap at 32 bits; readings less than
//! a second apart are ignored, as the reference does, so a burst of
//! replies cannot fake a spike.
//!
//! The BM1397 has no counters: it reports a rate in register 0x04, in
//! units of 2²⁰ H/s with bit 31 telling a long window from a short one
//! (ESP-Miner `update_hashrate`, WantClue `process_hashrate_response`).
//! That value goes straight into the chip's total: the short window as
//! the second-by-second rate, the long one as the minute rate.
//!
//! The reference reports the raw error counter value, not a rate, as
//! `errorCount`; both are kept here. BM1373 domain counters run at half
//! speed in the reference's reading (`domain_hashrate_scale` 2.0 in
//! ESP-Miner `main/device_config.h`), applied by [`HashrateMonitor::domain_ghs`].

use warpaxe_core::regs::{self, reg};

/// Hashes per counter step.
pub const HASH_COUNT_UNIT: f64 = 4_294_967_296.0;
/// Domains a chip has at most.
pub const MAX_DOMAINS: usize = 8;
/// Readings closer than this are ignored (reference: 1 s).
const MIN_INTERVAL_US: u64 = 1_000_000;
/// The slow window: a second rate over the last minute, for controllers
/// that must not act on one second's reading (the reference has a
/// `hashrate_1m` for the same reason).
const WINDOW_US: u64 = 60_000_000;
/// More counter steps per second than any chip can hash (4096 × 2^32 =
/// 17.6 TH/s per chip): the counter restarted (chip reset, brown-out
/// during a voltage step) or the reading was garbage. Such a delta is
/// not a rate; the reading only re-syncs the baseline.
const MAX_STEPS_PER_SECOND: u64 = 4096;

/// One counter and the rate derived from its last two readings.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Measurement {
    /// The last raw counter value.
    pub value: u32,
    /// When it was read, µs; 0 = never.
    pub time_us: u64,
    /// GH/s from the last two readings; 0 until there are two.
    pub ghs: f32,
    /// GH/s over the last full minute; 0 until a minute has passed.
    pub ghs_1m: f32,
    /// Counter value at the start of the running minute.
    win_value: u32,
    /// When the running minute started, µs; 0 = not started.
    win_time_us: u64,
}

impl Measurement {
    /// A new reading (ESP-Miner `update_hash_counter`).
    fn update(&mut self, value: u32, now_us: u64) {
        if self.time_us != 0 {
            let dt = now_us.saturating_sub(self.time_us);
            if dt < MIN_INTERVAL_US {
                return;
            }
            let steps = value.wrapping_sub(self.value);
            if !plausible(steps, dt) {
                // Counter restarted or garbage: start over from here,
                // keep the last rates.
                self.value = value;
                self.time_us = now_us;
                self.win_value = value;
                self.win_time_us = now_us;
                return;
            }
            self.ghs = rate_ghs(steps, dt);
            let wdt = now_us.saturating_sub(self.win_time_us);
            if self.win_time_us != 0 && wdt >= WINDOW_US {
                let wsteps = value.wrapping_sub(self.win_value);
                if plausible(wsteps, wdt) {
                    self.ghs_1m = rate_ghs(wsteps, wdt);
                }
                self.win_value = value;
                self.win_time_us = now_us;
            }
        } else {
            self.win_value = value;
            self.win_time_us = now_us;
        }
        self.value = value;
        self.time_us = now_us;
    }
}

impl Measurement {
    /// A rate the chip reported itself (BM1397 register 0x04): the short
    /// window is the rate, the long one the minute rate. The raw value
    /// is kept as `value`; a reading the reference calls invalid
    /// (0x007FFFFF) changes nothing.
    fn report_rate(&mut self, value: u32, now_us: u64) {
        let Some((hs, long)) = regs::bm1397_hashrate(value) else {
            return;
        };
        let ghs = (hs as f64 / 1e9) as f32;
        if long {
            self.ghs_1m = ghs;
        } else {
            self.ghs = ghs;
        }
        self.value = value;
        self.time_us = now_us;
    }
}

/// Counter steps over `dt_us` as GH/s.
fn rate_ghs(steps: u32, dt_us: u64) -> f32 {
    let hashes = f64::from(steps) * HASH_COUNT_UNIT;
    (hashes / (dt_us as f64 / 1e6) / 1e9) as f32
}

/// A delta a chip could actually have hashed in `dt_us`.
fn plausible(steps: u32, dt_us: u64) -> bool {
    u64::from(steps) <= MAX_STEPS_PER_SECOND * dt_us.div_ceil(1_000_000)
}

impl Measurement {}

/// One chip's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ChipMonitor {
    /// All domains together.
    pub total: Measurement,
    /// Per domain.
    pub domains: [Measurement; MAX_DOMAINS],
    /// Hash errors.
    pub error: Measurement,
}

/// The chain's counters, one entry per chip.
#[derive(Debug, Clone)]
pub struct HashrateMonitor<const N: usize> {
    chips: [ChipMonitor; N],
    count: usize,
    domains: usize,
    domain_scale: f32,
    /// The chip's own rate register, if it has one instead of counters.
    rate_register: Option<u8>,
}

impl<const N: usize> HashrateMonitor<N> {
    /// For a chain of chips with `domains` domains each; `domain_scale`
    /// is what a domain reading is multiplied by (1.0, or 2.0 on the
    /// BM1373).
    pub fn new(domains: u8, domain_scale: u8, rate_register: Option<u8>) -> Self {
        HashrateMonitor {
            chips: [ChipMonitor::default(); N],
            count: 0,
            domains: usize::from(domains).min(MAX_DOMAINS),
            domain_scale: f32::from(domain_scale.max(1)),
            rate_register,
        }
    }

    /// The chain was enumerated: `count` chips answer.
    pub fn set_chip_count(&mut self, count: u8) {
        self.count = usize::from(count).min(N);
    }

    /// Forget every reading (the chain stopped: counters restart at 0
    /// and a wrapped difference would read as a spike).
    pub fn reset(&mut self) {
        self.chips = [ChipMonitor::default(); N];
    }

    /// The six registers the driver polls per chip with four domains,
    /// in polling order.
    pub const REGISTERS: [u8; 6] = [
        reg::ERROR_COUNT,
        reg::DOMAIN_0_COUNT,
        reg::DOMAIN_1_COUNT,
        reg::DOMAIN_2_COUNT,
        reg::DOMAIN_3_COUNT,
        reg::TOTAL_COUNT,
    ];
    /// The ten registers for a chip with eight domains (BM1373).
    pub const REGISTERS_8: [u8; 10] = [
        reg::ERROR_COUNT,
        reg::DOMAIN_0_COUNT,
        reg::DOMAIN_1_COUNT,
        reg::DOMAIN_2_COUNT,
        reg::DOMAIN_3_COUNT,
        reg::DOMAIN_4_COUNT,
        reg::DOMAIN_5_COUNT,
        reg::DOMAIN_6_COUNT,
        reg::DOMAIN_7_COUNT,
        reg::TOTAL_COUNT,
    ];

    /// The BM1397's two: its rate register and the error counter
    /// (ESP-Miner `REGISTER_MAP`).
    pub const REGISTERS_RATE: [u8; 2] = [reg::BM1397_HASHRATE, reg::ERROR_COUNT];

    /// The registers the driver polls per chip of this chain, in order.
    pub fn registers(&self) -> &'static [u8] {
        if self.domains == 0 {
            return match self.rate_register {
                Some(reg::BM1397_HASHRATE) => &Self::REGISTERS_RATE,
                _ => &[],
            };
        }
        if self.domains > 4 {
            &Self::REGISTERS_8
        } else {
            &Self::REGISTERS
        }
    }

    /// A register reply from chip `chip`; `true` when it was one of the
    /// counters.
    pub fn on_register(&mut self, chip: usize, register: u8, value: u32, now_us: u64) -> bool {
        if chip >= self.count {
            return false;
        }
        let m = &mut self.chips[chip];
        if self.rate_register == Some(register) {
            m.total.report_rate(value, now_us);
            return true;
        }
        match register {
            reg::TOTAL_COUNT => m.total.update(value, now_us),
            reg::ERROR_COUNT => m.error.update(value, now_us),
            reg::DOMAIN_0_COUNT..=reg::DOMAIN_3_COUNT
            | reg::DOMAIN_4_COUNT..=reg::DOMAIN_7_COUNT => {
                let d = usize::from(reg::domain_of(register).unwrap_or(u8::MAX));
                if d < self.domains {
                    m.domains[d].update(value, now_us);
                }
            }
            _ => return false,
        }
        true
    }

    /// Chips with entries.
    pub fn chips(&self) -> &[ChipMonitor] {
        &self.chips[..self.count]
    }

    /// Domains per chip.
    pub fn domains(&self) -> usize {
        self.domains
    }

    /// A domain's hashrate, scaled as the reference reports it.
    pub fn domain_ghs(&self, chip: usize, domain: usize) -> f32 {
        self.chips
            .get(chip)
            .and_then(|c| c.domains.get(domain))
            .map_or(0.0, |m| m.ghs * self.domain_scale)
    }

    /// Sum of the chips' total counters, GH/s.
    pub fn total_ghs(&self) -> f32 {
        self.chips().iter().map(|c| c.total.ghs).sum()
    }

    /// Sum of the chips' error rates, GH/s.
    pub fn error_ghs(&self) -> f32 {
        self.chips().iter().map(|c| c.error.ghs).sum()
    }

    /// Sum of the chips' total rates over the last minute, GH/s.
    pub fn total_1m_ghs(&self) -> f32 {
        self.chips().iter().map(|c| c.total.ghs_1m).sum()
    }

    /// Sum of the chips' error rates over the last minute, GH/s.
    pub fn error_1m_ghs(&self) -> f32 {
        self.chips().iter().map(|c| c.error.ghs_1m).sum()
    }

    /// Errors as a percentage of the total over the last minute: what a
    /// controller should act on. 0 until the first minute is complete.
    pub fn error_percent_1m(&self) -> f32 {
        let t = self.total_1m_ghs();
        if t > 0.0 {
            self.error_1m_ghs() / t * 100.0
        } else {
            0.0
        }
    }

    /// Errors as a percentage of the total (reference `error_percentage`).
    pub fn error_percent(&self) -> f32 {
        let t = self.total_ghs();
        if t > 0.0 {
            self.error_ghs() / t * 100.0
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_reading_gives_the_rate_and_a_burst_is_ignored() {
        let mut m: HashrateMonitor<4> = HashrateMonitor::new(4, 1, None);
        m.set_chip_count(2);
        assert!(m.on_register(0, reg::TOTAL_COUNT, 1000, 1_000_000));
        assert_eq!(m.chips()[0].total.ghs, 0.0, "one reading is no rate");
        // 300 steps in one second: 300 × 2^32 H/s = 1288.5 GH/s.
        assert!(m.on_register(0, reg::TOTAL_COUNT, 1300, 2_000_000));
        let ghs = m.chips()[0].total.ghs;
        assert!((ghs - 1288.49).abs() < 0.1, "{ghs}");
        // 200 ms later: too close, ignored, value and time untouched.
        assert!(m.on_register(0, reg::TOTAL_COUNT, 1400, 2_200_000));
        assert_eq!(m.chips()[0].total.value, 1300);
        assert!((m.chips()[0].total.ghs - 1288.49).abs() < 0.1);
        assert_eq!(m.total_ghs(), ghs, "chip 1 has nothing yet");
        // Unknown register, chip beyond the count: not counters.
        assert!(!m.on_register(0, reg::TICKET_MASK, 1, 3_000_000));
        assert!(!m.on_register(2, reg::TOTAL_COUNT, 1, 3_000_000));
    }

    #[test]
    fn a_restarted_counter_or_garbage_reading_is_not_a_rate() {
        let mut m: HashrateMonitor<8> = HashrateMonitor::new(4, 1, None);
        m.set_chip_count(8);
        // Chip 3 reads normally: 300 steps a second.
        m.on_register(3, reg::TOTAL_COUNT, 100_000, 1_000_000);
        m.on_register(3, reg::ERROR_COUNT, 5_000, 1_000_000);
        m.on_register(3, reg::TOTAL_COUNT, 100_300, 2_000_000);
        m.on_register(3, reg::ERROR_COUNT, 5_003, 2_000_000);
        let good = m.chips()[3].total.ghs;
        // The chip restarts during a voltage step: both counters at 0.
        m.on_register(3, reg::TOTAL_COUNT, 5, 3_000_000);
        m.on_register(3, reg::ERROR_COUNT, 1, 3_000_000);
        assert_eq!(m.chips()[3].total.ghs, good, "the wrap is not a rate");
        assert!(m.error_percent() < 2.0, "{}", m.error_percent());
        // From the restart on, deltas count again.
        m.on_register(3, reg::TOTAL_COUNT, 305, 4_000_000);
        m.on_register(3, reg::ERROR_COUNT, 4, 4_000_000);
        assert!((m.chips()[3].total.ghs - good).abs() < 0.1);
        assert!((m.chips()[3].error.ghs - 12.88).abs() < 0.1);
        // A garbage total reading (bit flipped high) is skipped too.
        m.on_register(3, reg::TOTAL_COUNT, 305 + 300 + 0x0100_0000, 5_000_000);
        assert!((m.chips()[3].total.ghs - good).abs() < 0.1);
    }

    /// A BM1397 reports its rate instead of counting: the short window
    /// is the rate at once, the long one the minute figure, the invalid
    /// marker changes nothing, and the two registers read are 0x04 and
    /// the error counter.
    #[test]
    fn a_reported_rate_goes_straight_into_the_total() {
        let mut m: HashrateMonitor<1> = HashrateMonitor::new(0, 1, Some(reg::BM1397_HASHRATE));
        m.set_chip_count(1);
        assert_eq!(m.registers(), &[0x04, 0x4C]);
        assert!(m.on_register(0, 0x04, 192_260, 1_000));
        let c = m.chips()[0];
        assert!((c.total.ghs - 201.6).abs() < 0.01, "{}", c.total.ghs);
        assert_eq!(c.total.ghs_1m, 0.0);
        assert!(m.on_register(0, 0x04, 0x8000_0000 | 145_912, 2_000));
        let c = m.chips()[0];
        assert!((c.total.ghs - 201.6).abs() < 0.01);
        assert!((c.total.ghs_1m - 153.0).abs() < 0.01, "{}", c.total.ghs_1m);
        assert!(m.on_register(0, 0x04, 0x007F_FFFF, 3_000));
        assert_eq!(
            m.chips()[0].total.value,
            0x8000_0000 | 145_912,
            "invalid: unchanged"
        );
        assert!((m.total_ghs() - 201.6).abs() < 0.01);
        assert!((m.total_1m_ghs() - 153.0).abs() < 0.01);
        // Without a rate register a counterless chip reads nothing.
        let none: HashrateMonitor<1> = HashrateMonitor::new(0, 1, None);
        assert!(none.registers().is_empty());
    }

    #[test]
    fn the_minute_window_smooths_one_bad_second() {
        let mut m: HashrateMonitor<1> = HashrateMonitor::new(4, 1, None);
        m.set_chip_count(1);
        let mut total = 0u32;
        let mut err = 0u32;
        for s in 0..=61u64 {
            let now = 1_000_000 + s * 1_000_000;
            m.on_register(0, reg::TOTAL_COUNT, total, now);
            m.on_register(0, reg::ERROR_COUNT, err, now);
            total += 300;
            // One second with a burst of errors, otherwise 1 %.
            err += if s == 30 { 150 } else { 3 };
        }
        // The instant value saw the burst at second 31 and forgot it.
        assert!(m.error_percent() < 1.5, "{}", m.error_percent());
        assert!(m.chips()[0].total.ghs_1m > 0.0, "a minute has passed");
        let pct = m.error_percent_1m();
        assert!(
            pct > 1.5 && pct < 2.5,
            "burst averaged into the minute: {pct}"
        );
        assert!(
            (m.total_1m_ghs() - 1288.5).abs() < 1.0,
            "{}",
            m.total_1m_ghs()
        );
    }

    #[test]
    fn a_wrapped_counter_still_gives_the_difference() {
        let mut m: HashrateMonitor<1> = HashrateMonitor::new(4, 1, None);
        m.set_chip_count(1);
        m.on_register(0, reg::TOTAL_COUNT, u32::MAX - 9, 1);
        m.on_register(0, reg::TOTAL_COUNT, 90, 1_000_001);
        let ghs = m.chips()[0].total.ghs;
        assert!((ghs - 429.5).abs() < 0.1, "{ghs}");
    }

    #[test]
    fn domains_errors_and_the_bm1373_scale() {
        let mut m: HashrateMonitor<2> = HashrateMonitor::new(4, 2, None);
        m.set_chip_count(1);
        for (r, v) in [
            (reg::DOMAIN_0_COUNT, 0),
            (reg::DOMAIN_1_COUNT, 0),
            (reg::DOMAIN_2_COUNT, 0),
            (reg::DOMAIN_3_COUNT, 0),
            (reg::TOTAL_COUNT, 0),
            (reg::ERROR_COUNT, 0),
        ] {
            m.on_register(0, r, v, 5);
        }
        for (r, v) in [
            (reg::DOMAIN_0_COUNT, 10),
            (reg::DOMAIN_1_COUNT, 20),
            (reg::DOMAIN_2_COUNT, 30),
            (reg::DOMAIN_3_COUNT, 40),
            (reg::TOTAL_COUNT, 200),
            (reg::ERROR_COUNT, 2),
        ] {
            m.on_register(0, r, v, 2_000_005);
        }
        let unit = |steps: f32| steps * 4.294_967 / 2.0; // GH/s per step over 2 s
        assert!(
            (m.domain_ghs(0, 0) - unit(10.0) * 2.0).abs() < 0.01,
            "scaled ×2"
        );
        assert!((m.domain_ghs(0, 3) - unit(40.0) * 2.0).abs() < 0.01);
        assert!(
            (m.chips()[0].domains[1].ghs - unit(20.0)).abs() < 0.01,
            "raw"
        );
        assert!((m.total_ghs() - unit(200.0)).abs() < 0.01);
        assert!((m.error_ghs() - unit(2.0)).abs() < 0.001);
        assert!((m.error_percent() - 1.0).abs() < 0.01);
        assert_eq!(m.chips()[0].error.value, 2, "errorCount is the raw value");
        assert_eq!(m.domain_ghs(0, 4), 0.0);
    }

    #[test]
    fn eight_domains_read_all_eight_registers_and_add_up_without_a_scale() {
        let mut m: HashrateMonitor<1> = HashrateMonitor::new(8, 1, None);
        m.set_chip_count(1);
        assert_eq!(m.registers().len(), 10);
        assert_eq!(m.registers()[5], reg::DOMAIN_4_COUNT);
        assert_eq!(m.registers()[9], reg::TOTAL_COUNT);
        let four: HashrateMonitor<1> = HashrateMonitor::new(4, 1, None);
        assert_eq!(four.registers().len(), 6);
        for r in m.registers() {
            m.on_register(0, *r, 0, 5);
        }
        // Each domain a step, the total eight (what the Gaia showed).
        for d in 0..8u8 {
            assert!(m.on_register(0, reg::domain_count(d), 10, 2_000_005));
        }
        m.on_register(0, reg::TOTAL_COUNT, 80, 2_000_005);
        let unit = |steps: f32| steps * 4.294_967 / 2.0;
        let sum: f32 = (0..8).map(|d| m.domain_ghs(0, d)).sum();
        assert!(
            (sum - unit(80.0)).abs() < 0.01,
            "domains add up to the total, unscaled"
        );
        assert!((m.domain_ghs(0, 7) - unit(10.0)).abs() < 0.01);
        assert_eq!(m.domain_ghs(0, 8), 0.0);
        m.reset();
        assert_eq!(m.total_ghs(), 0.0);
        assert_eq!(m.chips()[0].total.time_us, 0);
    }
}

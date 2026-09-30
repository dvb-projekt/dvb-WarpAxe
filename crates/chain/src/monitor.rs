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
//! The reference reports the raw error counter value, not a rate, as
//! `errorCount`; both are kept here. BM1373 domain counters run at half
//! speed in the reference's reading (`domain_hashrate_scale` 2.0 in
//! ESP-Miner `main/device_config.h`), applied by [`HashrateMonitor::domain_ghs`].

use warpaxe_core::regs::reg;

/// Hashes per counter step.
pub const HASH_COUNT_UNIT: f64 = 4_294_967_296.0;
/// Domains a chip has at most.
pub const MAX_DOMAINS: usize = 4;
/// Readings closer than this are ignored (reference: 1 s).
const MIN_INTERVAL_US: u64 = 1_000_000;

/// One counter and the rate derived from its last two readings.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Measurement {
    /// The last raw counter value.
    pub value: u32,
    /// When it was read, µs; 0 = never.
    pub time_us: u64,
    /// GH/s from the last two readings; 0 until there are two.
    pub ghs: f32,
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
            let hashes = f64::from(steps) * HASH_COUNT_UNIT;
            self.ghs = (hashes / (dt as f64 / 1e6) / 1e9) as f32;
        }
        self.value = value;
        self.time_us = now_us;
    }
}

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
}

impl<const N: usize> HashrateMonitor<N> {
    /// For a chain of chips with `domains` domains each; `domain_scale`
    /// is what a domain reading is multiplied by (1.0, or 2.0 on the
    /// BM1373).
    pub fn new(domains: u8, domain_scale: u8) -> Self {
        HashrateMonitor {
            chips: [ChipMonitor::default(); N],
            count: 0,
            domains: usize::from(domains).min(MAX_DOMAINS),
            domain_scale: f32::from(domain_scale.max(1)),
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

    /// The six registers the driver polls per chip, in polling order.
    pub const REGISTERS: [u8; 6] = [
        reg::ERROR_COUNT,
        reg::DOMAIN_0_COUNT,
        reg::DOMAIN_1_COUNT,
        reg::DOMAIN_2_COUNT,
        reg::DOMAIN_3_COUNT,
        reg::TOTAL_COUNT,
    ];

    /// A register reply from chip `chip`; `true` when it was one of the
    /// counters.
    pub fn on_register(&mut self, chip: usize, register: u8, value: u32, now_us: u64) -> bool {
        if chip >= self.count {
            return false;
        }
        let m = &mut self.chips[chip];
        match register {
            reg::TOTAL_COUNT => m.total.update(value, now_us),
            reg::ERROR_COUNT => m.error.update(value, now_us),
            reg::DOMAIN_0_COUNT..=reg::DOMAIN_3_COUNT => {
                let d = usize::from(register - reg::DOMAIN_0_COUNT);
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
        let mut m: HashrateMonitor<4> = HashrateMonitor::new(4, 1);
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
    fn a_wrapped_counter_still_gives_the_difference() {
        let mut m: HashrateMonitor<1> = HashrateMonitor::new(4, 1);
        m.set_chip_count(1);
        m.on_register(0, reg::TOTAL_COUNT, u32::MAX - 9, 1);
        m.on_register(0, reg::TOTAL_COUNT, 90, 1_000_001);
        let ghs = m.chips()[0].total.ghs;
        assert!((ghs - 429.5).abs() < 0.1, "{ghs}");
    }

    #[test]
    fn domains_errors_and_the_bm1373_scale() {
        let mut m: HashrateMonitor<2> = HashrateMonitor::new(4, 2);
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
        m.reset();
        assert_eq!(m.total_ghs(), 0.0);
        assert_eq!(m.chips()[0].total.time_us, 0);
    }
}

//! What the regulator's STATUS_WORD means for the chain: a fault the
//! part has acted on (or is about to), a warning worth showing, or
//! nothing.
//!
//! AxeOS (`TPS546_check_status`, every power-management round) treats
//! OFF, VOUT_OV, IOUT_OC, VIN_UV and TEMP as faults, tolerates OFF while
//! the part is still coming up after an enable, logs a register snapshot
//! once per fault and reports `power_fault` in the system info; it never
//! switches anything off itself (the TPS546 does that on a hard fault).
//! NerdOS reads the same registers once at init and only logs them.
//!
//! The monitor here follows AxeOS's bits and grace, and adds the
//! reaction WarpAxe already has for an overheat: the core voltage goes
//! off through the [`crate::overheat::Guard`] and comes back after its
//! pause, a fault that keeps returning latches. An over-temperature
//! *warning* (STATUS_TEMPERATURE OTW without OTF) is reported, not acted
//! on: the temperature guard has its own limits for that.
//!
//! Times are seconds of a monotonic clock, as in [`crate::overheat`].

/// STATUS_WORD bits (PMBus 1.3 §17.2, same on TPS546 and TPS5364x).
pub mod status {
    /// Bit 7: the part was busy and could not answer.
    pub const BUSY: u16 = 0x0080;
    /// Bit 6: the output is off.
    pub const OFF: u16 = 0x0040;
    /// Bit 5: output over-voltage fault.
    pub const VOUT_OV: u16 = 0x0020;
    /// Bit 4: output over-current fault.
    pub const IOUT_OC: u16 = 0x0010;
    /// Bit 3: input under-voltage fault.
    pub const VIN_UV: u16 = 0x0008;
    /// Bit 2: a temperature fault or warning; STATUS_TEMPERATURE tells.
    pub const TEMP: u16 = 0x0004;
    /// Bit 1: communication, memory or logic fault.
    pub const CML: u16 = 0x0002;
    /// STATUS_TEMPERATURE bit 7: over-temperature fault.
    pub const TEMP_OTF: u8 = 0x80;
    /// STATUS_TEMPERATURE bit 6: over-temperature warning.
    pub const TEMP_OTW: u8 = 0x40;
}

/// Seconds after an enable during which OFF alone means "still coming
/// up", not a fault (AxeOS's power-good grace).
pub const GRACE_S: u32 = 5;

/// One condition the regulator reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The output is off although it was enabled: the part shut down.
    Off,
    /// Output over-voltage.
    VoutOv,
    /// Output over-current.
    IoutOc,
    /// Input under-voltage.
    VinUv,
    /// Over-temperature fault.
    OverTemp,
    /// Over-temperature warning (a warning, not a fault).
    TempWarning,
}

impl Condition {
    /// Every condition, in the order [`Faults::iter`] yields them.
    pub const ALL: [Condition; 6] = [
        Condition::Off,
        Condition::VoutOv,
        Condition::IoutOc,
        Condition::VinUv,
        Condition::OverTemp,
        Condition::TempWarning,
    ];

    /// A short code for APIs and translations.
    pub fn code(self) -> &'static str {
        match self {
            Condition::Off => "off",
            Condition::VoutOv => "vout_ov",
            Condition::IoutOc => "iout_oc",
            Condition::VinUv => "vin_uv",
            Condition::OverTemp => "temp",
            Condition::TempWarning => "temp_warn",
        }
    }

    /// The text AxeOS logs for it (its `TPS546_parse_status`).
    pub fn describe(self) -> &'static str {
        match self {
            Condition::Off => "The voltage regulator is turned off",
            Condition::VoutOv => "An output overvoltage fault has occurred",
            Condition::IoutOc => "An output overcurrent fault has occurred",
            Condition::VinUv => "An input undervoltage fault has occurred",
            Condition::OverTemp => "Overtemperature fault",
            Condition::TempWarning => "Overtemperature warning",
        }
    }

    /// A fault the chain must not run through, as opposed to a warning.
    pub fn is_fault(self) -> bool {
        !matches!(self, Condition::TempWarning)
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// A set of [`Condition`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults(u8);

impl Faults {
    /// Nothing reported.
    pub const NONE: Faults = Faults(0);

    /// Decode STATUS_WORD; `temperature` is STATUS_TEMPERATURE when the
    /// caller read it (needed to tell a warning from a fault; without it
    /// the TEMP bit counts as a fault, as AxeOS has it). `off_counts`
    /// says whether OFF is a condition now (false during the grace).
    pub fn decode(word: u16, temperature: Option<u8>, off_counts: bool) -> Faults {
        let mut f = Faults::NONE;
        if off_counts && word & status::OFF != 0 {
            f.insert(Condition::Off);
        }
        if word & status::VOUT_OV != 0 {
            f.insert(Condition::VoutOv);
        }
        if word & status::IOUT_OC != 0 {
            f.insert(Condition::IoutOc);
        }
        if word & status::VIN_UV != 0 {
            f.insert(Condition::VinUv);
        }
        if word & status::TEMP != 0 {
            match temperature {
                Some(t) if t & status::TEMP_OTF == 0 && t & status::TEMP_OTW != 0 => {
                    f.insert(Condition::TempWarning)
                }
                _ => f.insert(Condition::OverTemp),
            }
        }
        f
    }

    /// Add a condition.
    pub fn insert(&mut self, c: Condition) {
        self.0 |= c.bit();
    }

    /// Whether `c` is in the set.
    pub fn contains(self, c: Condition) -> bool {
        self.0 & c.bit() != 0
    }

    /// Nothing in the set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// At least one real fault (not only a warning).
    pub fn any_fault(self) -> bool {
        self.iter().any(Condition::is_fault)
    }

    /// The conditions, in [`Condition::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Condition> {
        Condition::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }
}

/// What one status reading means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The output is not enabled: nothing to judge.
    Disabled,
    /// The part was busy; try again next round.
    Busy,
    /// Nothing reported.
    Clear,
    /// Warnings only: report, keep running.
    Warning(Faults),
    /// At least one fault: the core voltage must go off.
    Fault(Faults),
}

/// The regulator's fault state across rounds: enable time for the
/// grace, and the last verdict so a new fault is logged once.
#[derive(Clone, Debug, Default)]
pub struct Monitor {
    enabled_at: Option<u32>,
    last: Faults,
}

impl Monitor {
    /// Output disabled, nothing seen.
    pub const fn new() -> Self {
        Monitor {
            enabled_at: None,
            last: Faults::NONE,
        }
    }

    /// The output was enabled at `now`: the grace starts.
    pub fn enabled(&mut self, now: u32) {
        self.enabled_at = Some(now);
        self.last = Faults::NONE;
    }

    /// The output was switched off on purpose: OFF is expected now.
    pub fn disabled(&mut self) {
        self.enabled_at = None;
        self.last = Faults::NONE;
    }

    /// Whether the output is enabled as far as the monitor knows.
    pub fn is_enabled(&self) -> bool {
        self.enabled_at.is_some()
    }

    /// The conditions of the last reading.
    pub fn faults(&self) -> Faults {
        self.last
    }

    /// Judge a STATUS_WORD read at `now`. Returns the verdict and
    /// whether the set of conditions changed since the last reading
    /// (log a snapshot then).
    pub fn observe(&mut self, now: u32, word: u16, temperature: Option<u8>) -> (Verdict, bool) {
        let Some(since) = self.enabled_at else {
            return (Verdict::Disabled, false);
        };
        if word & status::BUSY != 0 {
            return (Verdict::Busy, false);
        }
        let off_counts = now.saturating_sub(since) >= GRACE_S;
        let f = Faults::decode(word, temperature, off_counts);
        let changed = f != self.last;
        self.last = f;
        let verdict = if f.any_fault() {
            Verdict::Fault(f)
        } else if !f.is_empty() {
            Verdict::Warning(f)
        } else {
            Verdict::Clear
        };
        (verdict, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::status::*;
    use super::*;

    fn set(cs: &[Condition]) -> Faults {
        let mut f = Faults::NONE;
        for c in cs {
            f.insert(*c);
        }
        f
    }

    #[test]
    fn decode_follows_axeos_bits_and_ignores_cml() {
        assert_eq!(Faults::decode(0x0000, None, true), Faults::NONE);
        assert_eq!(Faults::decode(CML, None, true), Faults::NONE);
        assert_eq!(Faults::decode(OFF, None, true), set(&[Condition::Off]));
        assert_eq!(Faults::decode(OFF, None, false), Faults::NONE);
        assert_eq!(
            Faults::decode(OFF | IOUT_OC | CML, None, false),
            set(&[Condition::IoutOc])
        );
        assert_eq!(
            Faults::decode(VOUT_OV | VIN_UV | 0xF800, None, true),
            set(&[Condition::VoutOv, Condition::VinUv])
        );
    }

    #[test]
    fn temp_bit_is_a_warning_only_with_otw_alone() {
        assert_eq!(
            Faults::decode(TEMP, None, true),
            set(&[Condition::OverTemp])
        );
        assert_eq!(
            Faults::decode(TEMP, Some(TEMP_OTW), true),
            set(&[Condition::TempWarning])
        );
        assert_eq!(
            Faults::decode(TEMP, Some(TEMP_OTF | TEMP_OTW), true),
            set(&[Condition::OverTemp])
        );
        assert_eq!(
            Faults::decode(TEMP, Some(0), true),
            set(&[Condition::OverTemp])
        );
        assert!(!set(&[Condition::TempWarning]).any_fault());
        assert!(set(&[Condition::TempWarning, Condition::VinUv]).any_fault());
    }

    #[test]
    fn monitor_tolerates_off_during_the_grace_only() {
        let mut m = Monitor::new();
        assert_eq!(m.observe(0, OFF, None), (Verdict::Disabled, false));
        m.enabled(100);
        assert_eq!(m.observe(101, OFF, None), (Verdict::Clear, false));
        assert_eq!(
            m.observe(100 + GRACE_S - 1, OFF, None),
            (Verdict::Clear, false)
        );
        let (v, new) = m.observe(100 + GRACE_S, OFF, None);
        assert_eq!(v, Verdict::Fault(set(&[Condition::Off])));
        assert!(new, "first sighting");
        let (v, new) = m.observe(100 + GRACE_S + 1, OFF, None);
        assert_eq!(v, Verdict::Fault(set(&[Condition::Off])));
        assert!(!new, "same fault, no new snapshot");
        assert_eq!(m.faults(), set(&[Condition::Off]));
    }

    #[test]
    fn hard_faults_count_during_the_grace() {
        let mut m = Monitor::new();
        m.enabled(0);
        let (v, new) = m.observe(1, OFF | IOUT_OC, None);
        assert_eq!(v, Verdict::Fault(set(&[Condition::IoutOc])));
        assert!(new);
    }

    #[test]
    fn busy_and_disabled_change_nothing() {
        let mut m = Monitor::new();
        m.enabled(0);
        assert_eq!(
            m.observe(10, VIN_UV, None).0,
            Verdict::Fault(set(&[Condition::VinUv]))
        );
        assert_eq!(m.observe(11, BUSY | VIN_UV, None), (Verdict::Busy, false));
        assert_eq!(
            m.faults(),
            set(&[Condition::VinUv]),
            "busy keeps the last verdict"
        );
        m.disabled();
        assert_eq!(m.faults(), Faults::NONE);
        assert_eq!(
            m.observe(12, OFF | VIN_UV, None),
            (Verdict::Disabled, false)
        );
        m.enabled(12);
        assert_eq!(m.observe(13, 0, None), (Verdict::Clear, false));
    }

    #[test]
    fn warning_then_clear_reports_each_change() {
        let mut m = Monitor::new();
        m.enabled(0);
        let (v, new) = m.observe(20, TEMP, Some(TEMP_OTW));
        assert_eq!(v, Verdict::Warning(set(&[Condition::TempWarning])));
        assert!(new);
        let (v, new) = m.observe(21, 0, None);
        assert_eq!(v, Verdict::Clear);
        assert!(new, "the warning went away");
        let (v, new) = m.observe(22, 0, None);
        assert_eq!(v, Verdict::Clear);
        assert!(!new);
    }

    #[test]
    fn conditions_have_codes_and_axeos_texts() {
        let f = set(&[Condition::Off, Condition::VoutOv, Condition::TempWarning]);
        let codes: Vec<&str> = f.iter().map(Condition::code).collect();
        assert_eq!(codes, ["off", "vout_ov", "temp_warn"]);
        assert_eq!(
            Condition::IoutOc.describe(),
            "An output overcurrent fault has occurred"
        );
        let all: Vec<&str> = Condition::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(all.len(), 6);
        assert!(all.iter().all(|c| !c.is_empty()));
    }
}

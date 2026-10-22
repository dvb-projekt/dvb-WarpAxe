//! Whether a board profile fits the board it is about to run on, judged
//! from what answered on the I²C buses before anything is switched on.
//!
//! The first-start detection ([`crate::detect`]) names the candidates;
//! this module turns the same read-only evidence into a verdict per
//! profile, so a board picked by hand in the web UI (or set through
//! `PATCH /api/system`) is refused before its GPIOs, its regulator
//! window or its voltage reach a board of another kind. For the pairs
//! the bus cannot tell apart (siblings of one family: Gamma / Gamma Duo,
//! Naja Duo / Gamma Hex, NerdQAxe++ / NerdOctaxe with the four-phase
//! regulator, Q1370 / Q1373) it names the lowest voltage any of them is
//! brought up at ([`cautious_core_mv`]), so the first chain start after
//! a choice cannot over-volt the sibling; the chip count and the chip id
//! read at enumeration then decide ([`first_start_verdict`]).
//!
//! The requirements are derived from the profile (its regulator, sensor,
//! fan controller, expander, input class), not written per board, so a
//! new profile is covered as soon as it exists; the test
//! `every_wrong_choice_is_refused_or_a_harmless_sibling` walks every
//! ordered pair of profiles and fails on a new ambiguity.

use crate::detect::addr;
use crate::profile::{Profile, Thermal, Vr, ALL, T_DISPLAY_S3_DEVKIT};
use crate::tps53647;

/// A part the probe can see on a bus (or, for the TPS53667, in the
/// regulator's device code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// TPS546 core regulator at 0x24.
    Tps546,
    /// TPS53647 / TPS53667 at 0x71.
    Tps5364x,
    /// The six-phase TPS53667, told from the TPS53647 by its device
    /// code (same address).
    Tps53667,
    /// EMC2101 at 0x4C.
    Emc2101,
    /// EMC2103 at 0x2E (the Bitaxe Color boards and the Gamma Turbo).
    Emc2103,
    /// EMC2302 at 0x2E (the Nerd multi-chip boards).
    Emc2302,
    /// TMP1075 at 0x48.
    Tmp1075,
    /// FXL6408 expander at 0x43 (Q1370 / Q1373).
    Fxl6408,
    /// PCA9544 mux at 0x70 (BitForge Nano).
    Pca9544,
    /// INA260 at 0x40 (BitForge Nano).
    Ina260,
}

impl Part {
    /// The I²C address the probe asks for.
    pub fn addr(self) -> u8 {
        match self {
            Part::Tps546 => addr::TPS546,
            Part::Tps5364x | Part::Tps53667 => addr::TPS5364X,
            Part::Emc2101 => addr::EMC2101,
            Part::Emc2103 => addr::EMC2103,
            Part::Emc2302 => addr::EMC2302,
            Part::Tmp1075 => addr::TMP1075,
            Part::Fxl6408 => addr::FXL6408,
            Part::Pca9544 => addr::PCA9544,
            Part::Ina260 => addr::INA260,
        }
    }

    /// The part's name, for the log and the web UI.
    pub fn name(self) -> &'static str {
        match self {
            Part::Tps546 => "TPS546",
            Part::Tps5364x => "TPS53647",
            Part::Tps53667 => "TPS53667",
            Part::Emc2101 => "EMC2101",
            Part::Emc2103 => "EMC2103",
            Part::Emc2302 => "EMC2302",
            Part::Tmp1075 => "TMP1075",
            Part::Fxl6408 => "FXL6408",
            Part::Pca9544 => "PCA9544",
            Part::Ina260 => "INA260",
        }
    }
}

/// Which pin pair a profile's parts sit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// GPIO47 / 48: the Bitaxe boards and the BitForge Nano.
    Bitaxe,
    /// GPIO44 / 43: every Nerd carrier and the Bitaxe Color boards.
    Nerd,
    /// No I²C: the NerdNOS, the bare T-Display-S3, the undetected state.
    None,
}

/// The bus a profile's parts answer on.
pub fn bus_of(p: &Profile) -> Bus {
    match p.i2c {
        Some(pins) if (pins.sda, pins.scl) == (47, 48) => Bus::Bitaxe,
        Some(pins) if (pins.sda, pins.scl) == (44, 43) => Bus::Nerd,
        _ => Bus::None,
    }
}

/// What the probe found, all of it read-only: the addresses that
/// answered on each pin pair, the TPS546's READ_VIN, the TPS5364x's
/// device code, and (only when nothing answered anywhere) whether the
/// NerdNOS's thermistor divider reads plausibly on GPIO2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Evidence<'a> {
    /// Addresses that answered on GPIO47 / 48.
    pub bitaxe_bus: &'a [u8],
    /// Addresses that answered on GPIO44 / 43.
    pub nerd_bus: &'a [u8],
    /// The TPS546's input voltage, V, where one answered.
    pub vin_v: Option<f32>,
    /// MFR_SPECIFIC_44 of a TPS53647 / TPS53667 that answered.
    pub tps5364x_code: Option<u16>,
    /// A thermistor divider on GPIO2.
    pub thermistor: bool,
}

/// What a profile must and must not see on its own bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needs {
    /// Parts that must answer.
    pub present: [Option<Part>; 4],
    /// Parts that must not answer (they belong to another family).
    pub absent: [Option<Part>; 4],
    /// Input class to confirm from READ_VIN, V; 0 = not checked (no
    /// TPS546 to read it from).
    pub vin_v: u8,
    /// A TPS53667 device code rules the board out (the NerdQAxe++ only
    /// ever carries the three-phase TPS53647).
    pub no_tps53667: bool,
}

impl Needs {
    fn none() -> Self {
        Needs {
            present: [None; 4],
            absent: [None; 4],
            vin_v: 0,
            no_tps53667: false,
        }
    }

    /// The parts that must answer.
    pub fn present(&self) -> impl Iterator<Item = Part> + '_ {
        self.present.iter().flatten().copied()
    }

    /// The parts that must not answer.
    pub fn absent(&self) -> impl Iterator<Item = Part> + '_ {
        self.absent.iter().flatten().copied()
    }
}

fn push(list: &mut [Option<Part>; 4], p: Part) {
    if let Some(slot) = list.iter_mut().find(|s| s.is_none()) {
        *slot = Some(p);
    }
}

/// The requirements, derived from the profile: its regulator and the
/// input monitor, its sensor and fan controller, its expander; and the
/// tell-tale parts of the other families on the same bus, which must be
/// silent. A profile without a bus has none (see [`check`]).
pub fn needs(p: &Profile) -> Needs {
    let bus = bus_of(p);
    let mut n = Needs::none();
    if bus == Bus::None {
        return n;
    }
    let expander = matches!(
        p.vr,
        Vr::Tps5364x {
            via_expander: true,
            ..
        }
    );
    match p.vr {
        Vr::Tps546(_) | Vr::Tps546Nerd { .. } | Vr::Tps546Ina260 { .. } => {
            push(&mut n.present, Part::Tps546);
            n.vin_v = p.nominal_voltage_v;
        }
        Vr::Tps5364x { .. } => push(&mut n.absent, Part::Tps546),
        Vr::None => {}
    }
    if let Vr::Tps546Ina260 { .. } = p.vr {
        push(&mut n.present, Part::Ina260);
    }
    if expander {
        push(&mut n.present, Part::Fxl6408);
    } else {
        push(&mut n.absent, Part::Fxl6408);
    }
    // The other fan controller at 0x2E: on the Nerd pins an EMC2302 (the
    // multi-chip carriers), on the Bitaxe pins an EMC2103 (the Turbo).
    let other_2e = if bus == Bus::Nerd {
        Part::Emc2302
    } else {
        Part::Emc2103
    };
    match p.thermal {
        Thermal::Emc2101(_) => {
            push(&mut n.present, Part::Emc2101);
            push(&mut n.absent, other_2e);
            push(&mut n.absent, Part::Tmp1075);
            push(&mut n.absent, Part::Pca9544);
        }
        Thermal::Emc2103(_) => {
            push(&mut n.present, Part::Emc2103);
            push(&mut n.absent, Part::Emc2101);
            push(&mut n.absent, Part::Tmp1075);
            push(&mut n.absent, Part::Pca9544);
        }
        Thermal::Tmp1075Emc2302 => {
            push(&mut n.present, Part::Emc2302);
            push(&mut n.present, Part::Tmp1075);
            push(&mut n.absent, Part::Emc2101);
            push(&mut n.absent, Part::Pca9544);
        }
        Thermal::DualEmc2101Mux { .. } => push(&mut n.present, Part::Pca9544),
        Thermal::None | Thermal::Ntc(_) => push(&mut n.absent, Part::Pca9544),
    }
    // The one fact the profile does not carry: NerdOS fits the
    // NerdQAxe++ with the TPS53647 only; a TPS53667 means the Octaxe
    // (or a Q1373).
    n.no_tps53667 = p.name == "nerdqaxe-plusplus";
    n
}

/// Why a profile does not fit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reason {
    /// A part the board must have did not answer.
    Missing(Part),
    /// A part of another family answered.
    Unexpected(Part),
    /// READ_VIN is not the board's input class.
    InputVoltage {
        /// What the regulator read, tenths of a volt.
        read_dv: u16,
        /// The board's input class, V.
        nominal_v: u8,
    },
    /// Parts answered on a bus, but the profile has none (the NerdNOS,
    /// the bare T-Display-S3).
    PartsAnswered,
    /// The NerdNOS without its thermistor divider on GPIO2.
    NoThermistor,
}

impl Reason {
    /// A code for the API and the web UI's texts.
    pub fn code(&self) -> &'static str {
        match self {
            Reason::Missing(_) => "missing",
            Reason::Unexpected(_) => "unexpected",
            Reason::InputVoltage { .. } => "inputVoltage",
            Reason::PartsAnswered => "partsAnswered",
            Reason::NoThermistor => "noThermistor",
        }
    }

    /// The part concerned, if the reason names one.
    pub fn part(&self) -> Option<Part> {
        match self {
            Reason::Missing(p) | Reason::Unexpected(p) => Some(*p),
            _ => None,
        }
    }
}

/// The input voltage fits the board's class: 5 V boards read 3.75–6.75,
/// 12 V boards 9–16.2 (a 12 V supply reads 12.0–12.3 on the fleet, USB
/// PD gives a 5 V board 5.0–5.3).
pub fn vin_fits(nominal_v: u8, read_v: f32) -> bool {
    let n = f32::from(nominal_v);
    read_v.is_finite() && read_v >= n * 0.75 && read_v <= n * 1.35
}

/// Whether `profile` fits what answered. Only the profile's own bus is
/// judged (the other pair may not have been probed); a profile without
/// a bus needs both pairs silent.
pub fn check(profile: &Profile, e: &Evidence) -> Result<(), Reason> {
    let bus = match bus_of(profile) {
        Bus::Bitaxe => e.bitaxe_bus,
        Bus::Nerd => e.nerd_bus,
        Bus::None => {
            if !e.bitaxe_bus.is_empty() || !e.nerd_bus.is_empty() {
                return Err(Reason::PartsAnswered);
            }
            if matches!(profile.thermal, Thermal::Ntc(_)) && !e.thermistor {
                return Err(Reason::NoThermistor);
            }
            return Ok(());
        }
    };
    let n = needs(profile);
    // Another family's part first: "this is a Nano" says more than
    // "the EMC2101 did not answer".
    if let Some(p) = n.absent().find(|p| bus.contains(&p.addr())) {
        return Err(Reason::Unexpected(p));
    }
    if n.no_tps53667 && e.tps5364x_code == Some(tps53647::DEVICE_CODE_667) {
        return Err(Reason::Unexpected(Part::Tps53667));
    }
    if let Some(p) = n.present().find(|p| !bus.contains(&p.addr())) {
        return Err(Reason::Missing(p));
    }
    // The input class only where it tells two profiles apart (the
    // NerdAxe Gamma and the Gaia share their parts); elsewhere a sagging
    // supply or a reading the part has not settled must not refuse a
    // board its parts already named.
    if n.vin_v > 0 && class_ambiguous(profile) {
        if let Some(v) = e.vin_v {
            if !vin_fits(n.vin_v, v) {
                return Err(Reason::InputVoltage {
                    read_dv: (v.max(0.0) * 10.0 + 0.5) as u16,
                    nominal_v: n.vin_v,
                });
            }
        }
    }
    Ok(())
}

/// Another profile on the same bus shows the same parts but runs on
/// another input class: only then does READ_VIN decide.
pub fn class_ambiguous(p: &Profile) -> bool {
    let mine = needs(p);
    ALL.iter().any(|q| {
        q.name != p.name
            && bus_of(q) == bus_of(p)
            && q.nominal_voltage_v != p.nominal_voltage_v
            && needs(q).present == mine.present
    })
}

/// Every profile that fits the evidence, boards with chips first and
/// the chipless T-Display-S3 last (it fits whenever nothing answered,
/// but a carrier with a thermistor is a NerdNOS before it is a devkit).
pub fn candidates<'a>(e: &'a Evidence<'a>) -> impl Iterator<Item = &'static Profile> + 'a {
    ALL.iter()
        .filter(move |p| p.asic_count > 0 && check(p, e).is_ok())
        .chain(core::iter::once(&T_DISPLAY_S3_DEVKIT).filter(move |p| check(p, e).is_ok()))
}

/// The voltage a profile brings its chain up with: NerdOS's bring-up
/// voltage where the profile has one, else the factory point.
pub fn bring_up_mv(p: &Profile) -> u16 {
    if p.init_core_mv > 0 {
        p.init_core_mv
    } else {
        p.default_core_mv
    }
}

/// The core voltage for the first chain start after a choice: the
/// profile's own bring-up voltage (or the configured point, if that is
/// lower than the bring-up and above the init voltage), never above the
/// bring-up voltage of any sibling the bus cannot tell it from, and
/// never below the regulator's floor. A configured point above the
/// profile's bring-up (tuned for the board that was configured before,
/// or an overclock) waits until the choice is verified. So a Gamma Hex
/// chosen on a Naja Duo starts at the Naja's 1000 mV, a Q1370 on a Q1373
/// at the Q1373's 1050 mV; the chip count and the chip id then decide
/// whether the choice stands. 0 without a regulator.
pub fn cautious_core_mv(profile: &Profile, e: &Evidence, configured_mv: u16) -> u16 {
    if profile.vr == Vr::None {
        return 0;
    }
    let normal = profile
        .init_core_mv
        .max(configured_mv.min(bring_up_mv(profile)));
    let siblings_low = candidates(e)
        .filter(|c| c.name != profile.name && c.vr != Vr::None)
        .map(bring_up_mv)
        .min();
    let low = siblings_low.map_or(normal, |s| s.min(normal));
    low.max(profile.regulator_min_core_mv())
}

/// What `boardok` in NVS holds for a choice made under this firmware
/// that has not passed its first start yet (never a profile name).
pub const PENDING: &str = "pending";

/// How a boot treats the board named in the settings, from what the
/// NVS entry `boardok` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The entry names this board: it passed its first start here.
    Verified,
    /// No entry at all: a `boardModel` saved by a firmware before the
    /// guard existed, which has run on this board. Taken as verified
    /// and written.
    Migrate,
    /// The entry is present but not this name (the detection and a
    /// choice write [`PENDING`]): the bus check and the counted first
    /// start.
    Check,
}

/// The start mode for the board `chosen` by the settings.
pub fn start_mode(verified: Option<&str>, chosen: &str) -> Start {
    match verified {
        Some(v) if v == chosen => Start::Verified,
        None => Start::Migrate,
        Some(_) => Start::Check,
    }
}

/// Whether a `PATCH boardModel` must mark the choice pending: always for
/// another board than the verified one; for the verified board itself
/// only on request (`reverify`), which is how a fleet board is put
/// through the check on purpose (the hardware test of 2026-10-22: a
/// plain PATCH of the verified name was a no-op).
pub fn choice_needs_check(verified: Option<&str>, chosen: &str, reverify: bool) -> bool {
    reverify || verified != Some(chosen)
}

/// What the first chain start after a choice found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstStart {
    /// The chips answered as the profile says: the choice stands.
    Verified {
        /// How many answered.
        chips: u8,
    },
    /// A chip of another type answered (the BM1373 of a Naja Duo under
    /// the Hex profile, say).
    WrongChip {
        /// The id it answered with.
        chip_id: u16,
    },
    /// Nothing answered.
    NoChips,
    /// The count is another board's: more chips than the profile has,
    /// or exactly a sibling's count.
    WrongCount {
        /// How many answered.
        found: u8,
        /// How many the profile has.
        expected: u8,
    },
    /// Fewer chips than the profile has, and no sibling has that count:
    /// a dead chip or a bad contact, not another board. The choice
    /// stands, with a warning.
    ShortCount {
        /// How many answered.
        found: u8,
        /// How many the profile has.
        expected: u8,
    },
}

/// Judge the enumeration: a chip id the profile does not carry rules the
/// choice out first, then no chips at all, then the count against the
/// profile's (0 = discover: any count passes). `sibling_counts` are the
/// chip counts of the other boards that fit the bus: a count that is one
/// of those, or more chips than the profile has, means another board; a
/// smaller count that is nobody's means missing chips (ShortCount).
pub fn first_start_verdict(
    found: u8,
    expected: u8,
    unexpected_chip: Option<u16>,
    sibling_counts: &[u8],
) -> FirstStart {
    if let Some(chip_id) = unexpected_chip {
        return FirstStart::WrongChip { chip_id };
    }
    if found == 0 {
        return FirstStart::NoChips;
    }
    if expected > 0 && found != expected {
        if found > expected || sibling_counts.contains(&found) {
            return FirstStart::WrongCount { found, expected };
        }
        return FirstStart::ShortCount { found, expected };
    }
    FirstStart::Verified { chips: found }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::*;

    /// What each real board shows the probe (from the schematics and the
    /// bench: README board table, `detect.rs`), written by hand so the
    /// test does not read the requirements back from the profiles.
    struct Real {
        board: &'static Profile,
        bitaxe: &'static [u8],
        nerd: &'static [u8],
        vin: Option<f32>,
        code: Option<u16>,
        thermistor: bool,
        /// The regulator population, where a board has two (Octaxe with
        /// the four-phase TPS53647, Q1373 with the six-phase TPS53667).
        note: &'static str,
    }

    const T546: u8 = 0x24;
    const T5364: u8 = 0x71;
    const E2101: u8 = 0x4C;
    const E2103: u8 = 0x2E; // also the EMC2302
    const TMP: u8 = 0x48;
    const TMP2: u8 = 0x49;
    const FXL: u8 = 0x43;
    const OLED: u8 = 0x3C;
    const INA: u8 = 0x40;
    const MUX: u8 = 0x70;

    fn reals() -> Vec<Real> {
        let r = |board, bitaxe, nerd, vin, code, thermistor, note| Real {
            board,
            bitaxe,
            nerd,
            vin,
            code,
            thermistor,
            note,
        };
        vec![
            r(
                &BITAXE_GAMMA,
                &[T546, E2101, OLED],
                &[],
                Some(5.1),
                None,
                false,
                "",
            ),
            r(
                &BITAXE_GAMMA_DUO,
                &[T546, E2101, OLED],
                &[],
                Some(5.1),
                None,
                false,
                "",
            ),
            r(
                &BITAXE_GAMMA_TURBO,
                &[T546, E2103, OLED],
                &[],
                Some(12.1),
                None,
                false,
                "",
            ),
            r(
                &BITFORGE_NANO,
                &[T546, INA, MUX],
                &[],
                Some(12.2),
                None,
                false,
                "",
            ),
            r(
                &BITAXE_NAJA_DUO,
                &[],
                &[T546, E2103],
                Some(12.0),
                None,
                false,
                "",
            ),
            r(
                &BITAXE_GAMMA_HEX,
                &[],
                &[T546, E2103],
                Some(12.0),
                None,
                false,
                "",
            ),
            r(
                &NERDAXE_GAMMA,
                &[],
                &[T546, E2101],
                Some(5.0),
                None,
                false,
                "",
            ),
            r(
                &NERDAXE_GAIA,
                &[],
                &[T546, E2101],
                Some(12.1),
                None,
                false,
                "",
            ),
            r(
                &NERDQAXE_PLUSPLUS,
                &[],
                &[E2103, TMP, TMP2, T5364],
                None,
                Some(tps53647::DEVICE_CODE),
                false,
                "",
            ),
            r(
                &NERDQAXE_PLUSPLUS_REV7,
                &[],
                &[T546, E2103, TMP, TMP2],
                Some(12.0),
                None,
                false,
                "",
            ),
            r(
                &NERDOCTAXE_GAMMA,
                &[],
                &[E2103, TMP, TMP2, T5364],
                None,
                Some(tps53647::DEVICE_CODE_667),
                false,
                "6p",
            ),
            r(
                &NERDOCTAXE_GAMMA,
                &[],
                &[E2103, TMP, TMP2, T5364],
                None,
                Some(tps53647::DEVICE_CODE),
                false,
                "4p",
            ),
            r(
                &Q1370,
                &[],
                &[E2103, TMP, TMP2, FXL, T5364],
                None,
                Some(tps53647::DEVICE_CODE),
                false,
                "",
            ),
            r(
                &Q1373,
                &[],
                &[E2103, TMP, TMP2, FXL, T5364],
                None,
                Some(tps53647::DEVICE_CODE),
                false,
                "4p",
            ),
            r(
                &Q1373,
                &[],
                &[E2103, TMP, TMP2, FXL, T5364],
                None,
                Some(tps53647::DEVICE_CODE_667),
                false,
                "6p",
            ),
            r(&NERDNOS, &[], &[], None, None, true, ""),
            r(&T_DISPLAY_S3_DEVKIT, &[], &[], None, None, false, ""),
        ]
    }

    fn evidence(r: &Real) -> Evidence<'static> {
        Evidence {
            bitaxe_bus: r.bitaxe,
            nerd_bus: r.nerd,
            vin_v: r.vin,
            tps5364x_code: r.code,
            thermistor: r.thermistor,
        }
    }

    /// Wrong choices the bus cannot refuse: siblings of one family. For
    /// each, the chip count or the chip id refuses the choice at
    /// enumeration, and the cautious voltage keeps the sibling safe
    /// until then (asserted below). The devkit on a NerdNOS is the one
    /// pair with no regulator at all: the CPU hashes, the chip stays in
    /// reset.
    const SIBLINGS: &[(&str, &str)] = &[
        ("bitaxe-gamma", "bitaxe-gamma-duo"),
        ("bitaxe-gamma-duo", "bitaxe-gamma"),
        ("bitaxe-naja-duo", "bitaxe-gamma-hex"),
        ("bitaxe-gamma-hex", "bitaxe-naja-duo"),
        ("nerdqaxe-plusplus", "nerdoctaxe-gamma"),
        ("nerdoctaxe-gamma", "nerdqaxe-plusplus"),
        ("q1370", "q1373"),
        ("q1373", "q1370"),
        ("t-display-s3", "nerdnos"),
    ];

    #[test]
    fn every_board_fits_its_own_evidence() {
        for r in reals() {
            assert_eq!(
                check(r.board, &evidence(&r)),
                Ok(()),
                "{} {}",
                r.board.name,
                r.note
            );
            // And is among the candidates, first unless a sibling comes
            // earlier in ALL.
            assert!(
                candidates(&evidence(&r)).any(|c| c.name == r.board.name),
                "{}",
                r.board.name
            );
        }
    }

    #[test]
    fn every_wrong_choice_is_refused_or_a_harmless_sibling() {
        let mut accepted = Vec::new();
        for real in reals() {
            let e = evidence(&real);
            for chosen in ALL {
                if chosen.name == real.board.name {
                    continue;
                }
                match check(chosen, &e) {
                    Err(_) => {}
                    Ok(()) => {
                        accepted.push((chosen.name, real.board.name));
                        assert!(
                            SIBLINGS.contains(&(chosen.name, real.board.name)),
                            "{} accepted on a {} ({}): a new ambiguity",
                            chosen.name,
                            real.board.name,
                            real.note
                        );
                        // The sibling never gets more voltage domains in
                        // series than it has, and the first start sits at
                        // or below its own bring-up voltage.
                        assert_eq!(
                            chosen.voltage_domains, real.board.voltage_domains,
                            "{} on {}",
                            chosen.name, real.board.name
                        );
                        let mv = cautious_core_mv(chosen, &e, chosen.default_core_mv);
                        let vout = chosen.vout_for_core_mv(mv);
                        let real_vout = real.board.vout_for_core_mv(bring_up_mv(real.board));
                        assert!(
                            vout <= real_vout + 1e-6,
                            "{} on {}: {vout} V > {real_vout} V",
                            chosen.name,
                            real.board.name
                        );
                        // Enumeration tells them apart: another chip type
                        // or another count, unless neither has a
                        // regulator to protect.
                        assert!(
                            chosen.asic != real.board.asic
                                || chosen.asic_count != real.board.asic_count
                                || (chosen.vr == Vr::None && real.board.vr == Vr::None),
                            "{} on {}",
                            chosen.name,
                            real.board.name
                        );
                    }
                }
            }
        }
        // The list above is exact: a pair that stops being accepted is
        // worth knowing too.
        for (c, r) in SIBLINGS {
            assert!(accepted.contains(&(c, r)), "{c} on {r} is refused now");
        }
    }

    #[test]
    fn the_cautious_voltages_are_the_siblings_bring_up() {
        let real = |name: &str| reals().into_iter().find(|r| r.board.name == name).unwrap();
        let mv = |chosen: &Profile, on: &str| {
            let r = real(on);
            cautious_core_mv(chosen, &evidence(&r), chosen.default_core_mv)
        };
        assert_eq!(mv(&BITAXE_GAMMA_HEX, "bitaxe-naja-duo"), 1000);
        assert_eq!(mv(&BITAXE_NAJA_DUO, "bitaxe-gamma-hex"), 1000);
        assert_eq!(mv(&Q1370, "q1373"), 1050);
        assert_eq!(mv(&Q1373, "q1370"), 1050);
        assert_eq!(mv(&NERDOCTAXE_GAMMA, "nerdqaxe-plusplus"), 1200);
        assert_eq!(mv(&NERDQAXE_PLUSPLUS, "nerdoctaxe-gamma"), 1200);
        assert_eq!(mv(&BITAXE_GAMMA_DUO, "bitaxe-gamma"), 1150);
        // A board the bus identifies on its own starts as it always did.
        assert_eq!(mv(&BITFORGE_NANO, "bitforge-nano"), 1150);
        assert_eq!(mv(&NERDAXE_GAIA, "nerdaxe-gaia"), 1000);
        assert_eq!(mv(&NERDNOS, "nerdnos"), 0);
        // A configured point above the profile's bring-up voltage (a
        // Turbo tuned to 1250, or a point left over from another board)
        // waits until the choice is verified; one below the init voltage
        // does not pull the bring-up down.
        let r = real("bitaxe-gamma-turbo");
        assert_eq!(
            cautious_core_mv(&BITAXE_GAMMA_TURBO, &evidence(&r), 1250),
            1150
        );
        assert_eq!(
            cautious_core_mv(&BITAXE_GAMMA_TURBO, &evidence(&r), 1100),
            1100
        );
        let r = real("bitforge-nano");
        assert_eq!(
            cautious_core_mv(&BITFORGE_NANO, &evidence(&r), 1300),
            1150,
            "a Rev 7's 1300 mV left in the config"
        );
        let r = real("nerdaxe-gaia");
        assert_eq!(
            cautious_core_mv(&NERDAXE_GAIA, &evidence(&r), 700),
            1000,
            "init 1000 above the configured 700"
        );
    }

    #[test]
    fn the_reasons_name_the_tell_tale_part() {
        let real = |name: &str| reals().into_iter().find(|r| r.board.name == name).unwrap();
        let on = |chosen: &Profile, name: &str| check(chosen, &evidence(&real(name)));
        assert_eq!(
            on(&BITAXE_GAMMA, "bitforge-nano"),
            Err(Reason::Unexpected(Part::Pca9544))
        );
        assert_eq!(
            on(&BITAXE_GAMMA, "bitaxe-gamma-turbo"),
            Err(Reason::Unexpected(Part::Emc2103))
        );
        assert_eq!(
            on(&BITAXE_GAMMA_TURBO, "bitaxe-gamma"),
            Err(Reason::Unexpected(Part::Emc2101))
        );
        assert_eq!(
            on(&BITAXE_GAMMA_HEX, "nerdaxe-gamma"),
            Err(Reason::Unexpected(Part::Emc2101))
        );
        assert_eq!(
            on(&NERDAXE_GAMMA, "bitaxe-gamma-hex"),
            Err(Reason::Unexpected(Part::Emc2302))
        );
        assert_eq!(
            on(&NERDQAXE_PLUSPLUS_REV7, "nerdqaxe-plusplus"),
            Err(Reason::Missing(Part::Tps546))
        );
        assert_eq!(
            on(&NERDQAXE_PLUSPLUS, "nerdqaxe-plusplus-rev7"),
            Err(Reason::Unexpected(Part::Tps546))
        );
        assert_eq!(
            on(&Q1370, "nerdqaxe-plusplus"),
            Err(Reason::Missing(Part::Fxl6408))
        );
        assert_eq!(
            on(&NERDQAXE_PLUSPLUS, "q1370"),
            Err(Reason::Unexpected(Part::Fxl6408))
        );
        // A six-phase part rules the NerdQAxe++ out, not the Octaxe.
        let octaxe6 = reals()
            .into_iter()
            .find(|r| r.board.name == "nerdoctaxe-gamma" && r.note == "6p")
            .unwrap();
        assert_eq!(
            check(&NERDQAXE_PLUSPLUS, &evidence(&octaxe6)),
            Err(Reason::Unexpected(Part::Tps53667))
        );
        assert_eq!(check(&NERDOCTAXE_GAMMA, &evidence(&octaxe6)), Ok(()));
        // Gamma on a Gaia and the other way round: the input class.
        assert_eq!(
            on(&NERDAXE_GAMMA, "nerdaxe-gaia"),
            Err(Reason::InputVoltage {
                read_dv: 121,
                nominal_v: 5
            })
        );
        assert_eq!(
            on(&NERDAXE_GAIA, "nerdaxe-gamma"),
            Err(Reason::InputVoltage {
                read_dv: 50,
                nominal_v: 12
            })
        );
        // Only that pair is judged by the input class: a Nano, a Turbo
        // or a Hex whose regulator reads an odd VIN keeps running on
        // what its parts say.
        assert!(class_ambiguous(&NERDAXE_GAMMA) && class_ambiguous(&NERDAXE_GAIA));
        for p in [
            &BITFORGE_NANO,
            &BITAXE_GAMMA,
            &BITAXE_GAMMA_TURBO,
            &BITAXE_GAMMA_HEX,
            &NERDQAXE_PLUSPLUS_REV7,
        ] {
            assert!(!class_ambiguous(p), "{}", p.name);
            let r = reals()
                .into_iter()
                .find(|r| r.board.name == p.name)
                .unwrap();
            let e = Evidence {
                vin_v: Some(7.5),
                ..evidence(&r)
            };
            assert_eq!(check(p, &e), Ok(()), "{}", p.name);
        }
        // A bare profile on a board with chips, and a NerdNOS on a bare
        // T-Display.
        assert_eq!(
            on(&T_DISPLAY_S3_DEVKIT, "bitaxe-gamma"),
            Err(Reason::PartsAnswered)
        );
        assert_eq!(
            on(&NERDNOS, "nerdqaxe-plusplus"),
            Err(Reason::PartsAnswered)
        );
        assert_eq!(on(&NERDNOS, "t-display-s3"), Err(Reason::NoThermistor));
        // Anything on the other pair (the Bitaxe board's unprobed Nerd
        // pins) is not held against a Bitaxe profile.
        let e = Evidence {
            bitaxe_bus: &[T546, E2101],
            nerd_bus: &[E2103, TMP],
            vin_v: None,
            tps5364x_code: None,
            thermistor: false,
        };
        assert_eq!(check(&BITAXE_GAMMA, &e), Ok(()));
        // Without a VIN reading the class is not judged.
        let e = Evidence {
            bitaxe_bus: &[],
            nerd_bus: &[T546, E2101],
            vin_v: None,
            tps5364x_code: None,
            thermistor: false,
        };
        assert_eq!(check(&NERDAXE_GAMMA, &e), Ok(()));
        assert_eq!(check(&NERDAXE_GAIA, &e), Ok(()));
        assert_eq!(
            candidates(&e).map(|p| p.name).collect::<Vec<_>>(),
            ["nerdaxe-gamma", "nerdaxe-gaia"]
        );
    }

    #[test]
    fn candidates_follow_the_evidence() {
        let names = |name: &str| {
            let r = reals().into_iter().find(|r| r.board.name == name).unwrap();
            candidates(&evidence(&r))
                .map(|p| p.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("bitaxe-gamma"), ["bitaxe-gamma", "bitaxe-gamma-duo"]);
        assert_eq!(names("bitforge-nano"), ["bitforge-nano"]);
        assert_eq!(names("nerdaxe-gaia"), ["nerdaxe-gaia"]);
        assert_eq!(names("nerdaxe-gamma"), ["nerdaxe-gamma"]);
        assert_eq!(
            names("bitaxe-gamma-hex"),
            ["bitaxe-naja-duo", "bitaxe-gamma-hex"]
        );
        assert_eq!(
            names("nerdqaxe-plusplus"),
            ["nerdqaxe-plusplus", "nerdoctaxe-gamma"]
        );
        assert_eq!(names("nerdqaxe-plusplus-rev7"), ["nerdqaxe-plusplus-rev7"]);
        assert_eq!(names("q1370"), ["q1370", "q1373"]);
        assert_eq!(names("nerdnos"), ["nerdnos", "t-display-s3"]);
        assert_eq!(names("t-display-s3"), ["t-display-s3"]);
        let nothing = Evidence {
            bitaxe_bus: &[],
            nerd_bus: &[],
            vin_v: None,
            tps5364x_code: None,
            thermistor: false,
        };
        assert_eq!(
            candidates(&nothing).map(|p| p.name).collect::<Vec<_>>(),
            ["t-display-s3"]
        );
    }

    #[test]
    fn every_profile_with_a_bus_needs_its_regulator_or_its_sensor() {
        for p in ALL {
            let n = needs(p);
            match bus_of(p) {
                Bus::None => assert_eq!(n.present().count(), 0, "{}", p.name),
                _ => {
                    assert!(n.present().count() >= 2, "{}", p.name);
                    assert!(n.absent().count() >= 1, "{}", p.name);
                    // Nothing is both required and forbidden.
                    assert!(
                        n.present()
                            .all(|a| n.absent().all(|b| a.addr() != b.addr())),
                        "{}",
                        p.name
                    );
                }
            }
        }
        assert_eq!(bus_of(&BITAXE_GAMMA), Bus::Bitaxe);
        assert_eq!(bus_of(&BITFORGE_NANO), Bus::Bitaxe);
        assert_eq!(bus_of(&BITAXE_GAMMA_HEX), Bus::Nerd);
        assert_eq!(bus_of(&Q1373), Bus::Nerd);
        assert_eq!(bus_of(&NERDNOS), Bus::None);
        assert_eq!(bus_of(&UNDETECTED), Bus::None);
    }

    #[test]
    fn the_input_class_window() {
        for v in [3.75, 5.0, 5.3, 6.75] {
            assert!(vin_fits(5, v), "{v}");
        }
        for v in [3.7, 6.8, 12.0, 0.0, f32::NAN] {
            assert!(!vin_fits(5, v), "{v}");
        }
        for v in [9.0, 12.0, 12.3, 16.2] {
            assert!(vin_fits(12, v), "{v}");
        }
        for v in [5.2, 8.9, 16.3] {
            assert!(!vin_fits(12, v), "{v}");
        }
    }

    #[test]
    fn the_first_start_verdict() {
        assert_eq!(
            first_start_verdict(4, 4, None, &[8]),
            FirstStart::Verified { chips: 4 }
        );
        assert_eq!(
            first_start_verdict(2, 0, None, &[]),
            FirstStart::Verified { chips: 2 }
        );
        // An Octaxe chosen on a NerdQAxe++: four chips, the sibling's count.
        assert_eq!(
            first_start_verdict(4, 8, None, &[4]),
            FirstStart::WrongCount {
                found: 4,
                expected: 8
            }
        );
        // A NerdQAxe++ chosen on an Octaxe: more chips than it has.
        assert_eq!(
            first_start_verdict(8, 4, None, &[8]),
            FirstStart::WrongCount {
                found: 8,
                expected: 4
            }
        );
        // A Gamma Duo chosen on a Gamma: one chip, the Gamma's count.
        assert_eq!(
            first_start_verdict(1, 2, None, &[1]),
            FirstStart::WrongCount {
                found: 1,
                expected: 2
            }
        );
        // An Octaxe with a dead chip: seven is nobody's count.
        assert_eq!(
            first_start_verdict(7, 8, None, &[4]),
            FirstStart::ShortCount {
                found: 7,
                expected: 8
            }
        );
        assert_eq!(first_start_verdict(0, 1, None, &[]), FirstStart::NoChips);
        assert_eq!(
            first_start_verdict(2, 2, Some(0x1373), &[]),
            FirstStart::WrongChip { chip_id: 0x1373 }
        );
        assert_eq!(
            first_start_verdict(0, 2, Some(0x1373), &[]),
            FirstStart::WrongChip { chip_id: 0x1373 },
            "the chip id before the count"
        );
    }

    /// For every sibling pair the enumeration tells the boards apart
    /// with the rule above: the real board's count is either more than
    /// the chosen profile's or exactly a sibling's.
    #[test]
    fn every_sibling_pair_is_told_apart_by_count_or_chip_type() {
        for (chosen, real) in SIBLINGS {
            let c = ALL.iter().find(|p| p.name == *chosen).unwrap();
            let r = reals().into_iter().find(|r| r.board.name == *real).unwrap();
            if c.asic == r.board.asic && c.vr != Vr::None {
                let e = evidence(&r);
                let siblings: Vec<u8> = candidates(&e)
                    .filter(|p| p.name != c.name)
                    .map(|p| p.asic_count)
                    .collect();
                assert!(
                    matches!(
                        first_start_verdict(r.board.asic_count, c.asic_count, None, &siblings),
                        FirstStart::WrongCount { .. }
                    ),
                    "{chosen} on {real}"
                );
            }
        }
    }

    #[test]
    fn the_start_mode_and_the_patch_rule() {
        assert_eq!(
            start_mode(Some("nerdaxe-gaia"), "nerdaxe-gaia"),
            Start::Verified
        );
        assert_eq!(start_mode(None, "nerdaxe-gaia"), Start::Migrate);
        assert_eq!(start_mode(Some(PENDING), "nerdaxe-gaia"), Start::Check);
        assert_eq!(
            start_mode(Some("bitaxe-gamma"), "nerdaxe-gaia"),
            Start::Check,
            "another board verified: this one is checked"
        );
        // A PATCH of another board always marks it pending; the verified
        // board only when asked to re-verify.
        assert!(choice_needs_check(
            Some("nerdaxe-gaia"),
            "bitaxe-gamma",
            false
        ));
        assert!(!choice_needs_check(
            Some("nerdaxe-gaia"),
            "nerdaxe-gaia",
            false
        ));
        assert!(choice_needs_check(
            Some("nerdaxe-gaia"),
            "nerdaxe-gaia",
            true
        ));
        assert!(
            choice_needs_check(None, "nerdaxe-gaia", false),
            "nothing verified yet"
        );
        assert!(choice_needs_check(Some(PENDING), "nerdaxe-gaia", false));
        // The marker is no profile name.
        assert!(ALL.iter().all(|p| p.name != PENDING));
        assert_eq!(
            start_mode(Some(PENDING), PENDING),
            Start::Verified,
            "never reached: not in ALL"
        );
    }

    #[test]
    fn the_reason_codes_and_part_names_are_stable() {
        assert_eq!(Reason::Missing(Part::Tps546).code(), "missing");
        assert_eq!(Reason::Unexpected(Part::Pca9544).code(), "unexpected");
        assert_eq!(
            Reason::InputVoltage {
                read_dv: 121,
                nominal_v: 5
            }
            .code(),
            "inputVoltage"
        );
        assert_eq!(Reason::PartsAnswered.code(), "partsAnswered");
        assert_eq!(Reason::NoThermistor.code(), "noThermistor");
        assert_eq!(Reason::Missing(Part::Emc2302).part(), Some(Part::Emc2302));
        assert_eq!(Reason::PartsAnswered.part(), None);
        assert_eq!(Part::Tps5364x.addr(), 0x71);
        assert_eq!(Part::Tps53667.addr(), 0x71);
        assert_eq!(Part::Emc2103.addr(), Part::Emc2302.addr());
        assert_eq!(Part::Ina260.name(), "INA260");
    }
}

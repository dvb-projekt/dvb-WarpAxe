//! A clock of its own per chip: what may be asked for.
//!
//! The chips of a chain differ: at one core voltage one of them shows
//! hash errors, or latches a domain, at a clock the others run cleanly
//! (a NerdOctaxe, 2026-11-12: chip 2 at 2.2 % errors and chip 4 with a
//! latched domain at 625 MHz / 1130 mV, chips 1, 3 and 7 at 0.0–0.3 %).
//! The chain's clock therefore takes an offset per chip, in MHz, which
//! [`crate::Chain::set_chip_offsets`] carries out. Which chip gets how
//! much is searched by a tuner outside the firmware; here are only the
//! rules an offset has to keep, as the API checks them before anything
//! is stored:
//!
//! - every chip's own clock (the chain's plus its offset) has a PLL
//!   setting,
//! - none is above the board's highest clock,
//! - none is below [`floor_mhz`]: half the chain's clock, and never
//!   under the lowest clock the firmware sets at all,
//! - and on a board whose chips sit in series across the regulator's
//!   output, the clocks of the chips lie no further apart than
//!   [`series_spread_mhz`].
//!
//! Chips in series share one current. A chip that is slowed takes less
//! of it and a larger share of the voltage, and the others are left
//! with less. On a Naja Duo (two BM1373 in series, the midpoint not
//! regulated, 327 MHz, 2026-11-12): 25 MHz apart, the faster chip's
//! error rate rose from 0.2 % to 1.36 % (the slower one's fell); 60 MHz
//! apart, the faster chip's counters started over within two minutes: a
//! chip that reset itself for want of voltage, its PLL never written.
//! The same effect is what a tuner can use to balance such a pair, in
//! small steps.
//!
//! The list goes by chain index. One that is longer than the chain is
//! cut, a shorter one filled with zeros, and a list of zeros is no list
//! ([`normalise`]): the stored form is empty or has one entry per chip,
//! so a list that does not fit the board's chip count is one from
//! another board and is dropped.

use core::fmt;

use warpaxe_core::pll::{self, PllSpec};

use crate::MAX_CHIPS;

/// Why a list of offsets is refused. Chips from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A chip's clock would be above the board's highest.
    Above {
        /// The chip.
        chip: u8,
        /// Its clock with the offset, MHz.
        mhz: i64,
        /// The board's highest clock, MHz.
        max: u32,
    },
    /// A chip's clock would be below [`floor_mhz`].
    Below {
        /// The chip.
        chip: u8,
        /// Its clock with the offset, MHz.
        mhz: i64,
        /// The floor, MHz.
        min: u32,
    },
    /// The chip's PLL has no setting for its clock.
    NoPll {
        /// The chip.
        chip: u8,
        /// Its clock with the offset, MHz.
        mhz: i64,
    },
    /// The chips are in series and their clocks would lie further apart
    /// than [`series_spread_mhz`].
    Apart {
        /// The distance between the fastest and the slowest chip, MHz.
        mhz: i64,
        /// The most this board allows, MHz.
        max: u32,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Refusal::Apart { mhz, max } = *self {
            return write!(
                f,
                "chipFreqOffsets: {mhz} MHz between the chips, this board's chips are in series: at most {max} MHz"
            );
        }
        let (chip, mhz, why, limit) = match *self {
            Refusal::Above { chip, mhz, max } => (chip, mhz, "above the board's", max),
            Refusal::Below { chip, mhz, min } => (chip, mhz, "below", min),
            Refusal::NoPll { chip, mhz } => (chip, mhz, "has no PLL setting", 0),
            Refusal::Apart { .. } => return Ok(()),
        };
        write!(f, "chipFreqOffsets: chip {chip} at {mhz} MHz {why}")?;
        if limit > 0 {
            write!(f, " {limit} MHz")?;
        }
        Ok(())
    }
}

/// The lowest clock an offset may take a chip to, MHz: half the chain's
/// clock (rounded up), and not under `min_mhz`, the lowest clock the
/// firmware sets at all. Half is far below anything a search needs (the
/// chips of one board lie within a few per cent of each other) and
/// keeps a slip of the hand from parking a chip at next to nothing.
pub const fn floor_mhz(base_mhz: u32, min_mhz: u32) -> u32 {
    let half = base_mhz.div_ceil(2);
    if half > min_mhz {
        half
    } else {
        min_mhz
    }
}

/// How far apart the clocks of chips in series may lie, MHz: 25 MHz, or
/// 8 % of the chain's clock where that is less. At that distance a Naja
/// Duo's faster chip showed errors (1.36 %) and kept hashing; see the
/// module's head.
pub const fn series_spread_mhz(base_mhz: u32) -> u32 {
    let share = base_mhz / 100 * 8 + base_mhz % 100 * 8 / 100;
    if share < 25 {
        share
    } else {
        25
    }
}

/// Check `offsets` (MHz per chip, chain order, in the stored form: one
/// entry per chip, or none) against the chain's clock `base_mhz`: every
/// chip's own clock between [`floor_mhz`] and `max_mhz` (0 = the board
/// names no highest clock) and with a PLL setting; and, where the
/// board's chips are `in_series`, the offsets no further apart than
/// [`series_spread_mhz`]. Zeros are not looked at for the first three:
/// the chain's clock has its own check.
pub fn check(
    spec: &PllSpec,
    base_mhz: u32,
    offsets: &[i16],
    min_mhz: u32,
    max_mhz: u32,
    in_series: bool,
) -> Result<(), Refusal> {
    if in_series {
        let list = &offsets[..offsets.len().min(MAX_CHIPS)];
        let lo = list.iter().copied().min().unwrap_or(0);
        let hi = list.iter().copied().max().unwrap_or(0);
        let apart = i64::from(hi) - i64::from(lo);
        let max = series_spread_mhz(base_mhz);
        if apart > i64::from(max) {
            return Err(Refusal::Apart { mhz: apart, max });
        }
    }
    let min = floor_mhz(base_mhz, min_mhz);
    for (i, &offset) in offsets.iter().enumerate().take(MAX_CHIPS) {
        if offset == 0 {
            continue;
        }
        let chip = i as u8;
        let mhz = i64::from(base_mhz) + i64::from(offset);
        if max_mhz > 0 && mhz > i64::from(max_mhz) {
            return Err(Refusal::Above {
                chip,
                mhz,
                max: max_mhz,
            });
        }
        if mhz < i64::from(min) {
            return Err(Refusal::Below { chip, mhz, min });
        }
        let solved = u32::try_from(mhz)
            .ok()
            .and_then(|m| m.checked_mul(1_000_000))
            .and_then(|hz| pll::solve(spec, hz));
        if solved.is_none() {
            return Err(Refusal::NoPll { chip, mhz });
        }
    }
    Ok(())
}

/// The list as it is stored for a chain of `chips`: cut to the chain,
/// filled with zeros, and empty when no chip has an offset. Returns the
/// entries and how many of them count (0 or `chips`).
pub fn normalise(offsets: &[i16], chips: usize) -> ([i16; MAX_CHIPS], usize) {
    let n = chips.min(MAX_CHIPS);
    let mut out = [0i16; MAX_CHIPS];
    for (o, &v) in out.iter_mut().zip(offsets).take(n) {
        *o = v;
    }
    if out.iter().all(|&v| v == 0) {
        (out, 0)
    } else {
        (out, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use warpaxe_core::chip::bm1370::BM1370;
    use warpaxe_core::chip::bm1373::BM1373;

    #[test]
    fn an_empty_list_and_a_list_of_zeros_pass_whatever_the_clock() {
        for base in [0, 50, 625, 5000] {
            assert_eq!(check(&BM1370.pll, base, &[], 50, 700, false), Ok(()));
            assert_eq!(check(&BM1370.pll, base, &[0; 8], 50, 700, true), Ok(()));
        }
    }

    #[test]
    fn the_octaxe_case_passes() {
        // Chips 2 and 4 down, the rest at the chain's 625 MHz.
        let offsets = [0, 0, -25, 0, -50, 0, 0, 0];
        assert_eq!(check(&BM1370.pll, 625, &offsets, 50, 700, false), Ok(()));
        // Two BM1373 in series, the second one down.
        assert_eq!(check(&BM1373.pll, 600, &[0, -20], 50, 690, false), Ok(()));
    }

    #[test]
    fn every_chips_own_clock_is_held_to_the_boards_highest() {
        assert_eq!(
            check(&BM1370.pll, 690, &[0, 0, 15, 0], 50, 700, false),
            Err(Refusal::Above {
                chip: 2,
                mhz: 705,
                max: 700
            })
        );
        assert_eq!(
            check(&BM1370.pll, 690, &[0, 0, 10, 0], 50, 700, false),
            Ok(())
        );
        // A board that names no highest clock: the PLL decides.
        assert_eq!(
            check(&BM1370.pll, 690, &[0, 0, 15, 0], 50, 0, false),
            Ok(())
        );
    }

    #[test]
    fn no_chip_goes_below_half_the_chains_clock_or_the_lowest_clock() {
        assert_eq!(floor_mhz(625, 50), 313);
        assert_eq!(floor_mhz(600, 50), 300);
        assert_eq!(floor_mhz(80, 50), 50);
        assert_eq!(check(&BM1370.pll, 600, &[-300, 0], 50, 0, false), Ok(()));
        assert_eq!(
            check(&BM1370.pll, 600, &[-301, 0], 50, 0, false),
            Err(Refusal::Below {
                chip: 0,
                mhz: 299,
                min: 300
            })
        );
        // Far below zero: refused as too low, not wrapped into a clock.
        assert_eq!(
            check(&BM1370.pll, 600, &[0, i16::MIN], 50, 0, false),
            Err(Refusal::Below {
                chip: 1,
                mhz: 600 - 32768,
                min: 300
            })
        );
        assert_eq!(
            check(&BM1370.pll, 80, &[-31], 50, 0, false),
            Err(Refusal::Below {
                chip: 0,
                mhz: 49,
                min: 50
            })
        );
    }

    #[test]
    fn a_clock_without_a_pll_setting_is_refused_by_chip() {
        // Above every PLL setting of the BM1370, on a board that names
        // no highest clock.
        let r = check(&BM1370.pll, 3000, &[0, 0, 0, 30000], 50, 0, false);
        assert_eq!(
            r,
            Err(Refusal::NoPll {
                chip: 3,
                mhz: 33000
            })
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            "chipFreqOffsets: chip 3 at 33000 MHz has no PLL setting"
        );
    }

    /// Chips in series: what a Naja Duo showed (327 MHz) is the line.
    #[test]
    fn chips_in_series_stay_within_25_mhz_or_8_percent_of_each_other() {
        assert_eq!(series_spread_mhz(327), 25);
        assert_eq!(series_spread_mhz(600), 25);
        assert_eq!(series_spread_mhz(313), 25);
        assert_eq!(series_spread_mhz(312), 24);
        assert_eq!(series_spread_mhz(250), 20);
        assert_eq!(series_spread_mhz(0), 0);
        let naja = |o: &[i16]| check(&BM1373.pll, 327, o, 50, 690, true);
        assert_eq!(naja(&[]), Ok(()));
        assert_eq!(naja(&[-25, 0]), Ok(()));
        assert_eq!(naja(&[0, -25]), Ok(()));
        assert_eq!(naja(&[-10, 15]), Ok(()));
        // The run that reset chip 1.
        assert_eq!(naja(&[-60, 0]), Err(Refusal::Apart { mhz: 60, max: 25 }));
        assert_eq!(naja(&[0, -26]), Err(Refusal::Apart { mhz: 26, max: 25 }));
        assert_eq!(naja(&[-13, 13]), Err(Refusal::Apart { mhz: 26, max: 25 }));
        // Six chips in two domains (a Gamma Hex): the whole list counts.
        let hex = |o: &[i16]| check(&BM1370.pll, 500, o, 50, 0, true);
        assert_eq!(hex(&[0, 0, -25, 0, 0, 0]), Ok(()));
        assert_eq!(
            hex(&[5, 0, -25, 0, 0, 0]),
            Err(Refusal::Apart { mhz: 30, max: 25 })
        );
        // Chips in parallel keep the wide limits.
        assert_eq!(
            check(&BM1370.pll, 625, &[0, 0, -60, 0], 50, 700, false),
            Ok(())
        );
        assert_eq!(
            naja(&[-60, 0]).unwrap_err().to_string(),
            "chipFreqOffsets: 60 MHz between the chips, this board's chips are in series: at most 25 MHz"
        );
    }

    #[test]
    fn the_refusals_read_as_one_line() {
        let line = |r: Refusal| r.to_string();
        assert_eq!(
            line(Refusal::Above {
                chip: 2,
                mhz: 705,
                max: 700
            }),
            "chipFreqOffsets: chip 2 at 705 MHz above the board's 700 MHz"
        );
        assert_eq!(
            line(Refusal::Below {
                chip: 0,
                mhz: 299,
                min: 300
            }),
            "chipFreqOffsets: chip 0 at 299 MHz below 300 MHz"
        );
    }

    #[test]
    fn the_stored_list_is_empty_or_has_one_entry_per_chip() {
        let n = |o: &[i16], chips| {
            let (list, len) = normalise(o, chips);
            list[..len].to_vec()
        };
        assert_eq!(n(&[], 8), [0i16; 0]);
        assert_eq!(n(&[0, 0, 0], 8), [0i16; 0]);
        // Shorter: filled with zeros.
        assert_eq!(n(&[0, 0, -25], 8), [0, 0, -25, 0, 0, 0, 0, 0]);
        // Longer: the rest is no chip's.
        assert_eq!(n(&[0, -25, 0, 0, -50, 10], 4), [0, -25, 0, 0]);
        // Nothing left within the chain: no list.
        assert_eq!(n(&[0, 0, 0, 0, -50], 4), [0i16; 0]);
        assert_eq!(n(&[-5; 40], 40).len(), MAX_CHIPS);
        // One chip: its offset is the chain's clock by another name,
        // kept all the same.
        assert_eq!(n(&[-10], 1), [-10]);
        assert_eq!(n(&[-10], 0), [0i16; 0]);
    }
}

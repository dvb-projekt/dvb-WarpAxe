//! The hash-clock PLL: solving for divider settings, not looking them up.
//!
//! ```text
//! f_out = f_xtal · fb / (refdiv · postdiv1 · postdiv2)
//! f_vco = f_xtal · fb / refdiv
//! ```
//!
//! For a target frequency the solver walks every legal divider triple,
//! picks the feedback divider that lands closest, and keeps the best by
//! three criteria in order: smallest deviation from the target, then the
//! lowest VCO frequency, then the smallest `postdiv1 · postdiv2`. That is
//! exactly ESP-Miner's `pll_get_parameters()` (`components/asic/pll.c`),
//! including its loop order, which decides ties — and since the result
//! goes into a chip register, reproducing it bit for bit is the point,
//! not an aesthetic choice. The test module carries a port of the C code
//! as an oracle.
//!
//! Arithmetic is integer, in hertz. The C solver uses `float`, but on the
//! grid these dividers produce, two distinct frequencies are never closer
//! than `f_xtal / (max_div)²` ≈ 2.6 kHz, far above its 100 Hz tie
//! epsilon, so exact integer comparison and the C comparison agree. No
//! `libm`, no rounding-mode surprises, same answer on every core.
//!
//! What the chip actually runs at is [`PllWord::frequency_hz`], not the
//! target. Report that one: a firmware that shows 640 MHz for a PLL that
//! can only do 640.625 is lying about every efficiency figure derived
//! from it.

use core::ops::RangeInclusive;

/// Which search picks the dividers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Solver {
    /// ESP-Miner `pll_get_parameters()`: every legal triple, closest
    /// frequency, then lowest VCO, then smallest post-divider product.
    EspMiner,
    /// cgminer `driver-gekko.c` `calc_gsf_freq()`, the BM1397 formula
    /// NerdNOS runs (`BM1397_send_hash_frequency`): the target is
    /// clamped to 50–650 MHz, the dividers are fixed by the frequency
    /// band (refdiv 1 above 500 MHz else 2; postdiv1 3 up to 150 MHz, 2
    /// up to 250, else 1; postdiv2 always 5), and the feedback divider
    /// is `ceil(f · divider / 25 MHz)` — the result is never below the
    /// target. 300 MHz → fb 120, refdiv 2, postdivs 1·5 (`40 78 02 15`).
    Gekko,
}

/// How the post-dividers go into the register's last byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostdivEncoding {
    /// `(postdiv1 − 1) << 4 | (postdiv2 − 1)` (BM1366/68/70/73).
    MinusOne,
    /// `postdiv1 << 4 | postdiv2`, three bits each (BM1397: ESP-Miner
    /// `BM1397_send_hash_frequency`, cgminer `calc_gsf_freq`).
    Direct,
}

/// The PLL's legal ranges. Per chip; `f_xtal` is 25 MHz on every BM13xx.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PllSpec {
    /// Crystal / reference input, Hz.
    pub xtal_hz: u32,
    /// Feedback divider window. The one thing that differs between chips
    /// in the reference driver: BM1366 60–200, BM1370/BM1373 160–239.
    pub fb: RangeInclusive<u16>,
    /// Reference divider window; the reference driver walks 2 down to 1.
    pub refdiv: RangeInclusive<u8>,
    /// Post-divider window for both stages; the reference driver walks
    /// 7 down to 1 and requires `postdiv1 > postdiv2`.
    pub postdiv: RangeInclusive<u8>,
    /// VCO frequency (Hz) at or above which the "VCO scale" bit is set in
    /// the PLL register. `bm1370.c`: `fb·25/refdiv >= 2400 ? 0x50 : 0x40`.
    /// `u64::MAX` never sets it (the BM1397 writes `0x40` always).
    pub vco_scale_from_hz: u64,
    /// The divider search.
    pub solver: Solver,
    /// The register's post-divider byte.
    pub postdiv_encoding: PostdivEncoding,
}

impl PllSpec {
    /// BM1370 as `bm1370.c` drives it: fb 160–239, VCO-scale bit from 2400 MHz.
    pub const fn bm1370() -> Self {
        PllSpec {
            xtal_hz: 25_000_000,
            fb: 160..=239,
            refdiv: 1..=2,
            postdiv: 1..=7,
            vco_scale_from_hz: 2_400_000_000,
            solver: Solver::EspMiner,
            postdiv_encoding: PostdivEncoding::MinusOne,
        }
    }

    /// BM1368 as `bm1368.c` drives it: fb 144–235, otherwise the BM1370's
    /// rules (`BM1368_send_hash_frequency`, ESP-Miner v2.15.4).
    pub const fn bm1368() -> Self {
        PllSpec {
            fb: 144..=235,
            ..Self::bm1370()
        }
    }

    /// The window `test_pll.c` exercises (fb 60–200, matches BM1366).
    pub const fn bm1366() -> Self {
        PllSpec {
            fb: 60..=200,
            ..Self::bm1370()
        }
    }

    /// BM1397 as NerdNOS and cgminer drive it: the `calc_gsf_freq`
    /// bands, feedback divider up to 0x104 (the formula's `famax`),
    /// post-dividers written as they are, VCO-scale byte fixed at 0x40.
    /// ESP-Miner's `bm1397.c` searches fb 60–200 with the generic solver
    /// instead (300 MHz → `40 48 02 31`); this is the one the .46 runs.
    pub const fn bm1397() -> Self {
        PllSpec {
            xtal_hz: 25_000_000,
            fb: 16..=260,
            refdiv: 1..=2,
            postdiv: 1..=7,
            vco_scale_from_hz: u64::MAX,
            solver: Solver::Gekko,
            postdiv_encoding: PostdivEncoding::Direct,
        }
    }
}

/// One divider setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllWord {
    /// Feedback divider.
    pub fb: u16,
    /// Reference divider.
    pub refdiv: u8,
    /// First post-divider.
    pub postdiv1: u8,
    /// Second post-divider.
    pub postdiv2: u8,
}

impl PllWord {
    /// Total output divider `refdiv · postdiv1 · postdiv2`.
    pub const fn divider(&self) -> u32 {
        self.refdiv as u32 * self.postdiv1 as u32 * self.postdiv2 as u32
    }

    /// Output frequency in Hz, rounded to nearest.
    pub const fn frequency_hz(&self, xtal_hz: u32) -> u64 {
        let num = xtal_hz as u64 * self.fb as u64;
        let div = self.divider() as u64;
        (num + div / 2) / div
    }

    /// VCO frequency in Hz, rounded to nearest.
    pub const fn vco_hz(&self, xtal_hz: u32) -> u64 {
        let num = xtal_hz as u64 * self.fb as u64;
        let div = self.refdiv as u64;
        (num + div / 2) / div
    }

    /// Output frequency in MHz, for display. Not for arithmetic.
    pub fn frequency_mhz(&self, xtal_hz: u32) -> f32 {
        self.frequency_hz(xtal_hz) as f32 / 1_000_000.0
    }

    /// The four payload bytes the reference driver writes to the PLL
    /// register (`0x08`): `[vco_scale, fb, refdiv, postdivs]` with
    /// `postdivs` per [`PllSpec::postdiv_encoding`].
    ///
    /// Layout from `BM1370_send_hash_frequency()`. `fb` is truncated to a
    /// byte as the driver does; on the BM1397 (cgminer `calc_gsf_freq`)
    /// its ninth bit goes into the first byte (`0x40 + (fa >> 8)`).
    pub fn register_bytes(&self, spec: &PllSpec) -> [u8; 4] {
        let vco_scale = if self.vco_hz(spec.xtal_hz) >= spec.vco_scale_from_hz {
            0x50
        } else {
            0x40
        };
        match spec.postdiv_encoding {
            PostdivEncoding::MinusOne => [
                vco_scale,
                self.fb as u8,
                self.refdiv,
                ((self.postdiv1 - 1) & 0x0F) << 4 | ((self.postdiv2 - 1) & 0x0F),
            ],
            PostdivEncoding::Direct => [
                vco_scale | ((self.fb >> 8) & 0x01) as u8,
                self.fb as u8,
                self.refdiv,
                (self.postdiv1 & 0x07) << 4 | (self.postdiv2 & 0x07),
            ],
        }
    }

    /// Read a divider setting back from the four register bytes
    /// ([`PllWord::register_bytes`] inverted). The VCO-scale bit is
    /// derived, not stored, so it is ignored here. `None` if a divider
    /// decodes to zero (illegal: dividers are 1..).
    pub const fn from_register_bytes(b: [u8; 4], spec: &PllSpec) -> Option<Self> {
        let (fb, postdiv1, postdiv2) = match spec.postdiv_encoding {
            PostdivEncoding::MinusOne => (b[1] as u16, (b[3] >> 4) + 1, (b[3] & 0x0F) + 1),
            PostdivEncoding::Direct => (
                b[1] as u16 | ((b[0] & 0x01) as u16) << 8,
                b[3] >> 4,
                b[3] & 0x0F,
            ),
        };
        if b[2] == 0 || postdiv1 == 0 || postdiv2 == 0 || fb == 0 {
            return None;
        }
        Some(PllWord {
            fb,
            refdiv: b[2],
            postdiv1,
            postdiv2,
        })
    }
}

/// Find the divider setting for `target_hz` under `spec`, with the
/// spec's solver.
///
/// `None` only when no `(refdiv, postdiv1, postdiv2)` triple puts a
/// feedback divider inside the window — for the BM1370 window that does
/// not happen anywhere between 50 and 1200 MHz; the gekko formula always
/// has an answer (it clamps the target).
pub fn solve(spec: &PllSpec, target_hz: u32) -> Option<PllWord> {
    match spec.solver {
        Solver::EspMiner => solve_esp_miner(spec, target_hz),
        Solver::Gekko => Some(solve_gekko(spec, target_hz)),
    }
}

/// cgminer `calc_gsf_freq()` for the BM1397, as WantClue's
/// `BM1397_send_hash_frequency` has it. Works in whole MHz like the C
/// (`frequency` is a float there, but NerdNOS passes an integer);
/// sub-MHz parts of `target_hz` are rounded to the nearest MHz first.
pub fn solve_gekko(spec: &PllSpec, target_hz: u32) -> PllWord {
    let mhz = (u64::from(target_hz) + 500_000) / 1_000_000;
    let f1 = mhz.clamp(50, 650);
    let (mut fb, mut fc1, fc2) = (2u64, 1u64, 5u64);
    if f1 >= 500 {
        fb = 1;
    } else if f1 <= 150 {
        fc1 = 3;
    } else if f1 <= 250 {
        fc1 = 2;
    }
    // basef = 25 · ceil(f1 · fb · fc1 · fc2 / 25); fa = basef / 25
    let prod = f1 * fb * fc1 * fc2;
    let mut fa = prod.div_ceil(25);
    // "code failure ... basef isn't 400 to 6000": the C falls back to
    // 200 MHz (fa 80, fb 2, fc1 1, fc2 5 — `freqbuf` default).
    if fa < u64::from(*spec.fb.start()) || fa > u64::from(*spec.fb.end()) {
        fa = 80;
        fb = 2;
        fc1 = 1;
    }
    PllWord {
        fb: fa as u16,
        refdiv: fb as u8,
        postdiv1: fc1 as u8,
        postdiv2: fc2 as u8,
    }
}

/// ESP-Miner's search (see [`Solver::EspMiner`]).
pub fn solve_esp_miner(spec: &PllSpec, target_hz: u32) -> Option<PllWord> {
    let xtal = u64::from(spec.xtal_hz);
    let target = u64::from(target_hz);

    // Best so far, with the numbers the comparison needs. Deviation is
    // kept as a fraction |target·div − xtal·fb| / div and compared
    // cross-multiplied, so nothing is divided and nothing is rounded.
    let mut best: Option<(PllWord, u64, u64, u64)> = None; // (word, dev_num, dev_den, vco)

    // Same walk as the C code — high to low — so that on a full tie the
    // same candidate wins.
    for refdiv in spec.refdiv.clone().rev() {
        for postdiv1 in spec.postdiv.clone().rev() {
            for postdiv2 in spec.postdiv.clone().rev() {
                if postdiv1 <= postdiv2 {
                    continue;
                }
                let div = u64::from(refdiv) * u64::from(postdiv1) * u64::from(postdiv2);
                // fb = round(target · div / xtal), half up.
                let fb = (target * div + xtal / 2) / xtal;
                if fb < u64::from(*spec.fb.start()) || fb > u64::from(*spec.fb.end()) {
                    continue;
                }
                let dev_num = (target * div).abs_diff(xtal * fb);
                let vco = (xtal * fb + u64::from(refdiv) / 2) / u64::from(refdiv);
                let word = PllWord {
                    fb: fb as u16,
                    refdiv,
                    postdiv1,
                    postdiv2,
                };
                let better = match &best {
                    None => true,
                    Some((b, b_num, b_den, b_vco)) => {
                        // dev = dev_num/div  vs  b_dev = b_num/b_den
                        let lhs = dev_num * b_den;
                        let rhs = *b_num * div;
                        if lhs != rhs {
                            lhs < rhs
                        } else if vco != *b_vco {
                            vco < *b_vco
                        } else {
                            u32::from(postdiv1) * u32::from(postdiv2)
                                < u32::from(b.postdiv1) * u32::from(b.postdiv2)
                        }
                    }
                };
                if better {
                    best = Some((word, dev_num, div, vco));
                }
            }
        }
    }
    best.map(|(w, _, _, _)| w)
}

/// Convenience: `solve` for a target in MHz.
pub fn solve_mhz(spec: &PllSpec, target_mhz: u32) -> Option<PllWord> {
    solve(spec, target_mhz.checked_mul(1_000_000)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MHZ: u32 = 1_000_000;

    /// `test_pll.c`: 450 MHz in the 60–200 window → fb 72, refdiv 2,
    /// postdiv 2/1, exactly 450 MHz.
    #[test]
    fn reproduces_the_esp_miner_pll_test_case() {
        let w = solve_mhz(&PllSpec::bm1366(), 450).unwrap();
        assert_eq!(
            w,
            PllWord {
                fb: 72,
                refdiv: 2,
                postdiv1: 2,
                postdiv2: 1
            }
        );
        assert_eq!(w.frequency_hz(25 * MHZ), 450 * MHZ as u64);
    }

    /// BM1370 window, pinned from a port of the C solver (see the
    /// oracle below). Columns: target, fb, refdiv, pd1, pd2, f_hz, reg08.
    /// (target MHz, fb, refdiv, postdiv1, postdiv2, f_hz, register bytes)
    type Row = (u32, u16, u8, u8, u8, u64, [u8; 4]);

    #[rustfmt::skip]
    const BM1370_TABLE: &[Row] = &[
        (400, 160, 2, 5, 1, 400_000_000, [0x40, 0xA0, 0x02, 0x40]),
        (440, 176, 2, 5, 1, 440_000_000, [0x40, 0xB0, 0x02, 0x40]),
        (490, 196, 2, 5, 1, 490_000_000, [0x50, 0xC4, 0x02, 0x40]),
        (525, 168, 2, 4, 1, 525_000_000, [0x40, 0xA8, 0x02, 0x30]), // Gamma stock
        (550, 176, 2, 4, 1, 550_000_000, [0x40, 0xB0, 0x02, 0x30]),
        (575, 184, 2, 4, 1, 575_000_000, [0x40, 0xB8, 0x02, 0x30]),
        (600, 192, 2, 4, 1, 600_000_000, [0x50, 0xC0, 0x02, 0x30]), // VCO exactly 2400: scale bit set
        (625, 200, 2, 4, 1, 625_000_000, [0x50, 0xC8, 0x02, 0x30]),
        (640, 205, 2, 4, 1, 640_625_000, [0x50, 0xCD, 0x02, 0x30]), // the hand-tuned Gamma: 640.625, not 640
        (650, 208, 2, 4, 1, 650_000_000, [0x50, 0xD0, 0x02, 0x30]),
        (690, 221, 2, 4, 1, 690_625_000, [0x50, 0xDD, 0x02, 0x30]),
    ];

    #[test]
    fn bm1370_ladder_points_match_the_c_solver() {
        let spec = PllSpec::bm1370();
        for &(t, fb, r, p1, p2, hz, reg) in BM1370_TABLE {
            let w = solve_mhz(&spec, t).unwrap_or_else(|| panic!("{t} MHz: no solution"));
            assert_eq!(
                (w.fb, w.refdiv, w.postdiv1, w.postdiv2),
                (fb, r, p1, p2),
                "{t} MHz"
            );
            assert_eq!(w.frequency_hz(spec.xtal_hz), hz, "{t} MHz");
            assert_eq!(w.register_bytes(&spec), reg, "{t} MHz register");
        }
    }

    /// The claim in the crate docs, with numbers: most integer-MHz
    /// targets are not on the PLL grid.
    #[test]
    fn most_integer_targets_are_not_exactly_reachable() {
        let spec = PllSpec::bm1370();
        let off = (100..=700u32)
            .filter(|&t| {
                solve_mhz(&spec, t).unwrap().frequency_hz(spec.xtal_hz) != u64::from(t) * 1_000_000
            })
            .count();
        assert_eq!(off, 496);
    }

    #[test]
    fn bm1370_window_has_a_solution_everywhere_it_matters() {
        let spec = PllSpec::bm1370();
        for t in 50..=1200u32 {
            assert!(solve_mhz(&spec, t).is_some(), "{t} MHz");
        }
    }

    /// BM1373 oddity for the record (stage 2): between 320 and 380 the
    /// window forces refdiv 1 and a VCO around 5 GHz, while 300/350/400
    /// sit at ~2.1 GHz. Whether the silicon likes that is unknown; it
    /// may be why NerdOS defaults to 350 and ESP-Miner's 327 comes out
    /// as 326.786.
    #[test]
    fn bm1373_window_shows_the_high_vco_band() {
        let spec = PllSpec::bm1370(); // same fb window on BM1373
        let w320 = solve_mhz(&spec, 320).unwrap();
        assert_eq!((w320.refdiv, w320.postdiv1, w320.postdiv2), (1, 5, 3));
        assert_eq!(w320.vco_hz(spec.xtal_hz), 4_800_000_000);
        let w350 = solve_mhz(&spec, 350).unwrap();
        assert_eq!((w350.refdiv, w350.postdiv1, w350.postdiv2), (2, 6, 1));
        assert_eq!(w350.vco_hz(spec.xtal_hz), 2_100_000_000);
        let w327 = solve_mhz(&spec, 327).unwrap();
        assert_eq!(w327.frequency_hz(spec.xtal_hz), 326_785_714);
    }

    #[test]
    fn register_bytes_encode_postdivs_minus_one_in_nibbles() {
        let spec = PllSpec::bm1370();
        let w = PllWord {
            fb: 200,
            refdiv: 1,
            postdiv1: 7,
            postdiv2: 3,
        };
        assert_eq!(w.register_bytes(&spec), [0x50, 200, 1, 0x62]);
    }

    /// A port of `pll_get_parameters()` in `f32`, as the C code computes
    /// it, kept here as the oracle the integer solver must agree with.
    fn c_solver(spec: &PllSpec, target_mhz: f32) -> Option<PllWord> {
        const FREQ_MULT: f32 = 25.0;
        const EPSILON: f32 = 0.0001;
        let mut best = None;
        let mut min_diff = f32::MAX;
        let mut min_vco = f32::MAX;
        let mut min_pd = u16::MAX;
        for refdiv in spec.refdiv.clone().rev() {
            for pd1 in spec.postdiv.clone().rev() {
                for pd2 in spec.postdiv.clone().rev() {
                    let divider = u16::from(refdiv) * u16::from(pd1) * u16::from(pd2);
                    let fb = (target_mhz / FREQ_MULT * f32::from(divider)).round() as u16;
                    if pd1 > pd2 && spec.fb.contains(&fb) {
                        let f = FREQ_MULT * f32::from(fb) / f32::from(divider);
                        let diff = (target_mhz - f).abs();
                        let vco = FREQ_MULT * f32::from(fb) / f32::from(refdiv);
                        if diff < min_diff
                            || ((diff - min_diff).abs() < EPSILON && vco < min_vco)
                            || ((diff - min_diff).abs() < EPSILON
                                && (vco - min_vco).abs() < EPSILON
                                && u16::from(pd1) * u16::from(pd2) < min_pd)
                        {
                            min_diff = diff;
                            min_vco = vco;
                            min_pd = u16::from(pd1) * u16::from(pd2);
                            best = Some(PllWord {
                                fb,
                                refdiv,
                                postdiv1: pd1,
                                postdiv2: pd2,
                            });
                        }
                    }
                }
            }
        }
        best
    }

    #[test]
    fn oracle_reproduces_the_esp_miner_test_case_too() {
        let w = c_solver(&PllSpec::bm1366(), 450.0).unwrap();
        assert_eq!((w.fb, w.refdiv, w.postdiv1, w.postdiv2), (72, 2, 2, 1));
    }

    #[test]
    fn register_bytes_round_trip_through_from_register_bytes() {
        let spec = PllSpec::bm1370();
        for t in [400u32, 525, 600, 640, 690] {
            let w = solve_mhz(&spec, t).unwrap();
            let back = PllWord::from_register_bytes(w.register_bytes(&spec), &spec).unwrap();
            assert_eq!(back, w, "{t} MHz");
        }
        assert_eq!(
            PllWord::from_register_bytes([0x40, 0xA8, 0x00, 0x30], &spec),
            None,
            "refdiv 0"
        );
        let g = PllSpec::bm1397();
        for t in [50u32, 100, 200, 300, 400, 500, 650] {
            let w = solve_mhz(&g, t).unwrap();
            let back = PllWord::from_register_bytes(w.register_bytes(&g), &g).unwrap();
            assert_eq!(back, w, "{t} MHz");
        }
        assert_eq!(
            PllWord::from_register_bytes([0x40, 0x78, 0x02, 0x05], &g),
            None,
            "postdiv1 0"
        );
    }

    /// `calc_gsf_freq` by hand, as cgminer's comments walk it: the
    /// stock 300 MHz of the NerdNOS gives `fa` 120, `fb` 2, `fc1` 1,
    /// `fc2` 5 → `00 08 40 78 02 15`, exactly 300 MHz. The bands: 56.25
    /// (our ramp start) triples up, 200 doubles up, 650 halves down.
    #[test]
    fn gekko_formula_reproduces_cgminer() {
        let g = PllSpec::bm1397();
        let w = solve_mhz(&g, 300).unwrap();
        assert_eq!((w.fb, w.refdiv, w.postdiv1, w.postdiv2), (120, 2, 1, 5));
        assert_eq!(w.register_bytes(&g), [0x40, 0x78, 0x02, 0x15]);
        assert_eq!(w.frequency_hz(g.xtal_hz), 300_000_000);
        // 56.25 MHz: f1 = 56 (rounded), fc1 = 3: 56·2·3·5 = 1680 →
        // ceil(1680/25) = 68 → 25·68/30 = 56.67 MHz.
        let w = solve(&g, 56_250_000).unwrap();
        assert_eq!((w.fb, w.refdiv, w.postdiv1, w.postdiv2), (68, 2, 3, 5));
        assert_eq!(w.frequency_hz(g.xtal_hz), 56_666_667);
        // 200 MHz: fc1 = 2: 200·2·2·5 = 4000 → fa 160 → 200 MHz.
        let w = solve_mhz(&g, 200).unwrap();
        assert_eq!((w.fb, w.refdiv, w.postdiv1, w.postdiv2), (160, 2, 2, 5));
        assert_eq!(w.register_bytes(&g), [0x40, 0xA0, 0x02, 0x25]);
        // 650 MHz: fb = 1: 650·5 = 3250 → fa 130 → 650 MHz.
        let w = solve_mhz(&g, 650).unwrap();
        assert_eq!((w.fb, w.refdiv, w.postdiv1, w.postdiv2), (130, 1, 1, 5));
        // The clamp: 700 is treated as 650, 20 as 50.
        assert_eq!(solve_mhz(&g, 700), solve_mhz(&g, 650));
        assert_eq!(solve_mhz(&g, 20), solve_mhz(&g, 50));
        // 210 (NerdNOS after an overheat): 210·2·2·5 = 4200 → fa 168 →
        // 210 MHz; 425 (Bitaxe Max stock): 4250 → fa 170 → 425.
        assert_eq!(
            solve_mhz(&g, 210).unwrap().frequency_hz(g.xtal_hz),
            210_000_000
        );
        assert_eq!(
            solve_mhz(&g, 425).unwrap().frequency_hz(g.xtal_hz),
            425_000_000
        );
        // Never below the target, never more than 25/divider above it.
        for mhz in 50..=650u32 {
            let w = solve_mhz(&g, mhz).unwrap();
            let f = w.frequency_hz(g.xtal_hz);
            assert!(f >= u64::from(mhz) * 1_000_000, "{mhz}");
            assert!(f < (u64::from(mhz) + 5) * 1_000_000, "{mhz}: {f}");
        }
    }

    /// ESP-Miner's own BM1397 path (`pll_get_parameters(…, 60, 200)`,
    /// generic solver, direct post-divider nibbles) at 300 MHz:
    /// `40 48 02 31`, the alternative on record.
    #[test]
    fn esp_miner_bm1397_window_for_the_record() {
        let spec = PllSpec {
            fb: 60..=200,
            solver: Solver::EspMiner,
            ..PllSpec::bm1397()
        };
        let w = solve_mhz(&spec, 300).unwrap();
        assert_eq!((w.fb, w.refdiv, w.postdiv1, w.postdiv2), (72, 2, 3, 1));
        assert_eq!(w.register_bytes(&spec), [0x40, 0x48, 0x02, 0x31]);
    }

    proptest::proptest! {
        /// Integer solver == C solver on every quarter-MHz from 50 to
        /// 1200 MHz, both windows. Ties and all.
        #[test]
        fn integer_solver_agrees_with_the_c_oracle(
            quarter in 200u32..4800, // 50.00 .. 1199.75 MHz
            bm1366 in proptest::bool::ANY,
        ) {
            let spec = if bm1366 { PllSpec::bm1366() } else { PllSpec::bm1370() };
            let hz = quarter * 250_000;
            let ours = solve(&spec, hz);
            let theirs = c_solver(&spec, quarter as f32 / 4.0);
            proptest::prop_assert_eq!(ours, theirs, "{} MHz", quarter as f32 / 4.0);
        }

        /// What the solver returns is always inside the spec.
        /// The gekko word always decodes to what was written and lands
        /// within the formula's bounds.
        #[test]
        fn gekko_words_round_trip(hz in 1_000_000u32..1_000_000_000) {
            let g = PllSpec::bm1397();
            let w = solve(&g, hz).unwrap();
            proptest::prop_assert!(g.fb.contains(&w.fb));
            proptest::prop_assert!((1..=2).contains(&w.refdiv));
            proptest::prop_assert!((1..=3).contains(&w.postdiv1));
            proptest::prop_assert_eq!(w.postdiv2, 5);
            let back = PllWord::from_register_bytes(w.register_bytes(&g), &g).unwrap();
            proptest::prop_assert_eq!(back, w);
            let f = w.frequency_hz(g.xtal_hz);
            proptest::prop_assert!((50_000_000..=655_000_000).contains(&f));
        }

        #[test]
        fn solutions_respect_the_spec(hz in 50_000_000u32..1_200_000_000) {
            let spec = PllSpec::bm1370();
            if let Some(w) = solve(&spec, hz) {
                proptest::prop_assert!(spec.fb.contains(&w.fb));
                proptest::prop_assert!(spec.refdiv.contains(&w.refdiv));
                proptest::prop_assert!(spec.postdiv.contains(&w.postdiv1));
                proptest::prop_assert!(spec.postdiv.contains(&w.postdiv2));
                proptest::prop_assert!(w.postdiv1 > w.postdiv2);
            }
        }
    }
}

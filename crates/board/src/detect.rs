//! Which board this is, on a first start (no `boardModel` configured).
//!
//! Two sources, both read-only: what the stock firmware left in NVS
//! (AxeOS stores `devicemodel` / `asicmodel` / `boardversion` from the
//! factory; NerdOS builds one image per board and stores none), and which
//! chips answer on the two I²C pin pairs the boards use (Bitaxe 47/48,
//! Nerd 44/43). Neither pair drives a power or reset line on the other
//! boards, and a probe only addresses a chip, it writes nothing.
//!
//! A board is taken without asking only when the evidence is unique and
//! the board has run WarpAxe; otherwise the candidates go to the web UI
//! and the chips stay unpowered until someone picks one.

use crate::profile::{
    Profile, BITAXE_GAMMA, NERDAXE_GAMMA, NERDOCTAXE_GAMMA, NERDQAXE_PLUSPLUS, Q1370, Q1373,
};

/// I²C addresses the probe asks for.
pub mod addr {
    /// TPS546 core regulator (Bitaxe, NerdAxe).
    pub const TPS546: u8 = 0x24;
    /// EMC2101 fan controller and chip-diode sensor (Bitaxe, NerdAxe).
    pub const EMC2101: u8 = 0x4C;
    /// EMC2302 dual fan controller (four- and eight-chip Nerd boards).
    pub const EMC2302: u8 = 0x2E;
    /// First TMP1075 temperature sensor (Nerd boards).
    pub const TMP1075: u8 = 0x48;
    /// FXL6408 GPIO expander (NerdQX Q1370/Q1373).
    pub const FXL6408: u8 = 0x43;
    /// SSD1306 OLED (Bitaxe).
    pub const SSD1306: u8 = 0x3C;
    /// Every address probed, in order.
    pub const ALL: [u8; 6] = [TPS546, EMC2101, EMC2302, TMP1075, FXL6408, SSD1306];
}

/// Boards that have run WarpAxe; only these are taken without asking.
pub const TESTED: &[&str] = &["bitaxe-gamma", "nerdqaxe-plusplus"];

/// The outcome of a first-start detection.
#[derive(Debug, Clone, PartialEq)]
pub enum Detection {
    /// Unique and tested: start with it.
    Board(&'static Profile, &'static str),
    /// These boards fit (best guess first; none = nothing recognised); ask
    /// before powering anything.
    Choose(&'static [&'static Profile], &'static str),
}

/// From the stock firmware's NVS (AxeOS keys, as stored). `None` when the
/// values do not name a board WarpAxe knows.
pub fn from_stock(
    devicemodel: Option<&str>,
    asicmodel: Option<&str>,
    boardversion: Option<&str>,
) -> Option<&'static Profile> {
    let model = devicemodel?.trim();
    let asic = asicmodel.unwrap_or("").trim();
    let version: u32 = boardversion
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    // Gamma 600-series boards carry one BM1370 on a TPS546; the Gamma
    // Turbo/Duo/Hex variants report other device models.
    if model.eq_ignore_ascii_case("gamma")
        && (asic.is_empty() || asic.eq_ignore_ascii_case("BM1370"))
        && (600..700).contains(&version)
    {
        return Some(&BITAXE_GAMMA);
    }
    None
}

/// From the chips that answered on the Bitaxe pair (47/48) and on the
/// Nerd pair (44/43).
pub fn from_bus(bitaxe_bus: &[u8], nerd_bus: &[u8]) -> Detection {
    let has = |bus: &[u8], a: u8| bus.contains(&a);
    if has(bitaxe_bus, addr::TPS546) && has(bitaxe_bus, addr::EMC2101) {
        return Detection::Board(&BITAXE_GAMMA, "TPS546 and EMC2101 on the Bitaxe bus");
    }
    if has(nerd_bus, addr::TPS546) && has(nerd_bus, addr::EMC2101) {
        return Detection::Choose(&[&NERDAXE_GAMMA], "TPS546 and EMC2101 on the Nerd bus");
    }
    if has(nerd_bus, addr::EMC2302) && has(nerd_bus, addr::TMP1075) {
        if has(nerd_bus, addr::FXL6408) {
            return Detection::Choose(
                &[&Q1370, &Q1373],
                "EMC2302, TMP1075 and the FXL6408 expander",
            );
        }
        // NerdOS treats the NerdOctaxe as a NerdQAxe++ with more chips; the
        // regulator does not tell the variant. One click decides.
        return Detection::Choose(
            &[&NERDQAXE_PLUSPLUS, &NERDOCTAXE_GAMMA],
            "EMC2302 and TMP1075 on the Nerd bus",
        );
    }
    Detection::Choose(&[], "no known chips answered")
}

/// Stock NVS first, then the bus.
pub fn detect(
    stock: (Option<&str>, Option<&str>, Option<&str>),
    bitaxe_bus: &[u8],
    nerd_bus: &[u8],
) -> Detection {
    // Both sources only ever name a tested board outright (the Bitaxe
    // Gamma); everything else comes back as a choice.
    if let Some(p) = from_stock(stock.0, stock.1, stock.2) {
        return Detection::Board(p, "board model stored by the stock firmware (AxeOS)");
    }
    from_bus(bitaxe_bus, nerd_bus)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: (Option<&str>, Option<&str>, Option<&str>) = (None, None, None);

    #[test]
    fn a_gamma_from_axeos_nvs_is_taken() {
        let d = detect((Some("gamma"), Some("BM1370"), Some("602")), &[], &[]);
        assert!(matches!(d, Detection::Board(p, _) if p.name == "bitaxe-gamma"));
    }

    #[test]
    fn other_axeos_models_fall_through_to_the_bus() {
        assert_eq!(from_stock(Some("supra"), Some("BM1368"), Some("401")), None);
        assert_eq!(from_stock(Some("gamma"), Some("BM1370"), Some("800")), None);
    }

    #[test]
    fn a_gamma_from_its_chips_is_taken() {
        let d = detect(NONE, &[addr::TPS546, addr::EMC2101, addr::SSD1306], &[]);
        assert!(matches!(d, Detection::Board(p, _) if p.name == "bitaxe-gamma"));
    }

    #[test]
    fn nerd_boards_are_offered_not_taken() {
        match detect(NONE, &[], &[addr::EMC2302, addr::TMP1075]) {
            Detection::Choose(c, _) => {
                assert_eq!(
                    c.iter().map(|p| p.name).collect::<Vec<_>>(),
                    ["nerdqaxe-plusplus", "nerdoctaxe-gamma"]
                );
            }
            d => panic!("{d:?}"),
        }
        match detect(NONE, &[], &[addr::EMC2302, addr::TMP1075, addr::FXL6408]) {
            Detection::Choose(c, _) => assert_eq!(c.len(), 2),
            d => panic!("{d:?}"),
        }
        match detect(NONE, &[], &[addr::TPS546, addr::EMC2101]) {
            Detection::Choose(c, _) => assert_eq!(c[0].name, "nerdaxe-gamma"),
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn only_tested_boards_are_ever_taken() {
        assert!(TESTED.contains(&BITAXE_GAMMA.name));
    }

    #[test]
    fn nothing_on_either_bus_offers_nothing() {
        assert_eq!(
            detect(NONE, &[], &[]),
            Detection::Choose(&[], "no known chips answered")
        );
    }
}

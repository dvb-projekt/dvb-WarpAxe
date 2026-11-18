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
    Profile, BITAXE_GAMMA, BITAXE_GAMMA_DUO, BITAXE_GAMMA_HEX, BITAXE_GAMMA_TURBO, BITAXE_NAJA_DUO,
    BITFORGE_NANO, NERDAXE_GAIA, NERDAXE_GAMMA, NERDNOS, NERDOCTAXE_GAMMA, NERDQAXE_PLUSPLUS,
    NERDQAXE_PLUSPLUS_REV7, Q1370, Q1373, T_DISPLAY_S3_DEVKIT,
};

/// I²C addresses the probe asks for.
pub mod addr {
    /// TPS546 core regulator (Bitaxe, NerdAxe).
    pub const TPS546: u8 = 0x24;
    /// EMC2101 fan controller and chip-diode sensor (Bitaxe, NerdAxe).
    pub const EMC2101: u8 = 0x4C;
    /// EMC2302 dual fan controller (four- and eight-chip Nerd boards).
    pub const EMC2302: u8 = 0x2E;
    /// EMC2103 (Gamma Turbo, Naja Duo, Gamma Hex): the same address.
    pub const EMC2103: u8 = 0x2E;
    /// First TMP1075 temperature sensor (Nerd boards).
    pub const TMP1075: u8 = 0x48;
    /// FXL6408 GPIO expander (NerdQX Q1370/Q1373).
    pub const FXL6408: u8 = 0x43;
    /// SSD1306 OLED (Bitaxe).
    pub const SSD1306: u8 = 0x3C;
    /// INA260 input power monitor (BitForge Nano).
    pub const INA260: u8 = 0x40;
    /// PCA9544 I²C mux in front of the Nano's two EMC2101.
    pub const PCA9544: u8 = 0x70;
    /// TPS53647 / TPS53667 multi-phase regulator (the Nerd multi-chip
    /// boards); its device code tells the two apart.
    pub const TPS5364X: u8 = 0x71;
    /// Every address probed, in order.
    pub const ALL: [u8; 9] = [
        TPS546, EMC2101, EMC2302, TMP1075, FXL6408, SSD1306, INA260, PCA9544, TPS5364X,
    ];
}

/// Boards that have run WarpAxe on our bench (the web UI marks the others
/// "untested"). A board joins once it has mined for days and passed the
/// self-test; whether a first start takes it without asking is decided by
/// the detection below, not by this list.
pub const TESTED: &[&str] = &[
    "bitaxe-gamma",
    "nerdqaxe-plusplus",
    "nerdoctaxe-gamma",
    "bitforge-nano",
    "nerdaxe-gaia",
    "nerdnos",
];

/// The outcome of a first-start detection.
#[derive(Debug, Clone, PartialEq)]
pub enum Detection {
    /// Unique and tested: start with it.
    Board(&'static Profile, &'static str),
    /// These boards fit (best guess first; none = nothing recognised); ask
    /// before powering anything.
    Choose(&'static [&'static Profile], &'static str),
    /// A board WarpAxe recognises but cannot run yet (named); nothing is
    /// powered.
    Unsupported(&'static str, &'static str),
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
        && version != 650
    {
        return Some(&BITAXE_GAMMA);
    }
    None
}

/// The Bitaxe families AxeOS runs and WarpAxe does not: BM1397 (Max),
/// BM1366 (Ultra, Hex) and BM1368 (Supra, Supra Hex). Their board
/// versions, device models and chip names as ESP-Miner stores them
/// (`device_config.h`, master 2026-11-18). On these boards the bus looks
/// like a Gamma's (Supra, Ultra: TPS546 and EMC2101) or a Gamma Turbo's
/// (Supra Hex, Hex: their EMC2302 answers at the EMC2103's address), so
/// the stock keys are the only place that tells them apart before a chip
/// is powered.
const UNSUPPORTED_STOCK: &[(&str, &str, &[&str])] = &[
    ("max", "Bitaxe Max (BM1397)", &["102", "2.2"]),
    (
        "ultra",
        "Bitaxe Ultra (BM1366)",
        &["0.11", "201", "202", "203", "204", "205", "207"],
    ),
    ("hex", "Bitaxe Hex (6× BM1366)", &["302", "303"]),
    (
        "supra",
        "Bitaxe Supra (BM1368)",
        &["400", "401", "402", "403"],
    ),
    ("suprahex", "Bitaxe Supra Hex (6× BM1368)", &["701", "702"]),
    // TinyChipHub's Zyber boards (ESP-Miner-Zyber, factory NVS files
    // config-zyber*.cvs): one TPS546 drives four voltage domains in
    // series, and the bus (TPS546, EMC2302, TMP1075) looks like a Supra
    // Hex's. Matched by device model only: their board versions (1000,
    // 1100, 1110 and 1200, which the 8G+ and the 12G share) lie in the
    // range ESP-Miner numbers its own boards in (Naja Duo 1201, Gamma Hex
    // 1300).
    ("zyber8s", "TinyChipHub Zyber8S (8× BM1368)", &[]),
    ("zyber8g", "TinyChipHub Zyber8G (8× BM1370)", &[]),
    ("zyber8gplus", "TinyChipHub Zyber8G+ (8× BM1370)", &[]),
    ("zyber12g", "TinyChipHub Zyber12G (12× BM1370)", &[]),
];

/// A Bitaxe the stock firmware named (device model, board version or
/// chip) that WarpAxe cannot run: its name for the web UI. The chip
/// alone still names it, so a board version ESP-Miner added after this
/// list (a Supra Hex 703, reported 2026-11-18) is caught by its BM1368.
pub fn from_stock_unsupported(
    devicemodel: Option<&str>,
    asicmodel: Option<&str>,
    boardversion: Option<&str>,
) -> Option<&'static str> {
    let model = devicemodel.unwrap_or("").trim();
    let version = boardversion.unwrap_or("").trim();
    if let Some(&(_, name, _)) = UNSUPPORTED_STOCK
        .iter()
        .find(|(m, _, v)| model.eq_ignore_ascii_case(m) || v.contains(&version))
    {
        return Some(name);
    }
    // A Zyber model TinyChipHub adds later.
    if model
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("zyber"))
    {
        return Some("TinyChipHub Zyber");
    }
    let asic = asicmodel.unwrap_or("").trim();
    [
        ("BM1397", "Bitaxe (BM1397)"),
        ("BM1366", "Bitaxe (BM1366)"),
        ("BM1368", "Bitaxe (BM1368)"),
    ]
    .iter()
    .find(|(chip, _)| asic.eq_ignore_ascii_case(chip))
    .map(|&(_, name)| name)
}

/// Board versions the stock firmware stores for boards WarpAxe carries
/// a profile for but has never run: offered, not taken.
pub fn from_stock_choice(boardversion: Option<&str>) -> Option<&'static Profile> {
    match boardversion?.trim() {
        "650" => Some(&BITAXE_GAMMA_DUO),
        "801" => Some(&BITAXE_GAMMA_TURBO),
        "1201" => Some(&BITAXE_NAJA_DUO),
        "1300" => Some(&BITAXE_GAMMA_HEX),
        "800" => Some(&BITFORGE_NANO),
        _ => None,
    }
}

/// WarpAxe's `flipscreen` for AxeOS 2.15's `rotation` (0 = the board as
/// it comes, 180 = turned). On the OLED Bitaxes WarpAxe draws the other
/// way up from AxeOS, so its switch is on at rotation 0. On the Color
/// boards (Naja Duo 1201, Gamma Hex 1300) AxeOS draws the ST7789 the way
/// the profile's `display_flip` already does, so the switch is off at 0.
/// Seen 2026-11-12: a Naja Duo imported rotation 0 as flipped and showed
/// its screen upside down.
pub fn flip_for_axeos_rotation(boardversion: Option<&str>, rotation: f64) -> bool {
    let turned = rotation == 180.0;
    match from_stock_choice(boardversion).map(|p| p.display) {
        Some(crate::profile::Display::St7789I80) => turned,
        _ => !turned,
    }
}

/// From the chips that answered on the Bitaxe pair (47/48) and on the
/// Nerd pair (44/43), and the input voltage the TPS546 reads (READ_VIN,
/// read-only), which tells a 5 V carrier from a 12 V one.
pub fn from_bus(bitaxe_bus: &[u8], nerd_bus: &[u8], vin_v: Option<f32>) -> Detection {
    let has = |bus: &[u8], a: u8| bus.contains(&a);
    // The Nano's EMC2101 hide behind the mux; the INA260 and the mux
    // itself answer on the Bitaxe pins. No other board has them: taken
    // (runs on our bench since 2026-09-29).
    if has(bitaxe_bus, addr::TPS546)
        && has(bitaxe_bus, addr::PCA9544)
        && has(bitaxe_bus, addr::INA260)
    {
        return Detection::Board(
            &BITFORGE_NANO,
            "TPS546, INA260 and a PCA9544 mux on the Bitaxe bus",
        );
    }
    if has(bitaxe_bus, addr::TPS546) && has(bitaxe_bus, addr::EMC2101) {
        return Detection::Board(&BITAXE_GAMMA, "TPS546 and EMC2101 on the Bitaxe bus");
    }
    // The Gamma Turbo: a TPS546 with an EMC2103 on the original pins.
    if has(bitaxe_bus, addr::TPS546) && has(bitaxe_bus, addr::EMC2103) {
        return Detection::Choose(
            &[&BITAXE_GAMMA_TURBO],
            "TPS546 and EMC2103 on the Bitaxe bus",
        );
    }
    if has(nerd_bus, addr::TPS546) && has(nerd_bus, addr::EMC2101) {
        // NerdAxe Gamma (BM1370, 5 V) and NerdAxe Gaia (BM1373, 12 V) look
        // the same on the bus; the regulator's input voltage tells them
        // apart. The Gaia runs on our bench (since 2026-09-29): taken on
        // 12 V. The Gamma has never run: offered on 5 V. Without a
        // reading, one click decides.
        return match vin_v {
            Some(v) if crate::choice::vin_fits(12, v) => Detection::Board(
                &NERDAXE_GAIA,
                "TPS546 and EMC2101 on the Nerd bus, 12 V at the regulator",
            ),
            Some(v) if crate::choice::vin_fits(5, v) => Detection::Choose(
                &[&NERDAXE_GAMMA],
                "TPS546 and EMC2101 on the Nerd bus, 5 V at the regulator",
            ),
            _ => Detection::Choose(
                &[&NERDAXE_GAMMA, &NERDAXE_GAIA],
                "TPS546 and EMC2101 on the Nerd bus",
            ),
        };
    }
    if has(nerd_bus, addr::EMC2302) && has(nerd_bus, addr::TMP1075) {
        if has(nerd_bus, addr::TPS546) {
            // NerdOS finds a TPS546 instead of the TPS53647 on the Rev 7
            // NerdQAxe++. Unique, but the profile has never run: one click.
            return Detection::Choose(
                &[&NERDQAXE_PLUSPLUS_REV7],
                "TPS546 next to EMC2302 and TMP1075 on the Nerd bus",
            );
        }
        if has(nerd_bus, addr::FXL6408) {
            return Detection::Choose(
                &[&Q1370, &Q1373],
                "EMC2302, TMP1075 and the FXL6408 expander",
            );
        }
        // (the Bitaxe Color boards share this address with their EMC2103,
        // but carry no TMP1075: see below)
        // NerdOS treats the NerdOctaxe as a NerdQAxe++ with more chips; the
        // regulator does not tell the variant. One click decides.
        return Detection::Choose(
            &[&NERDQAXE_PLUSPLUS, &NERDOCTAXE_GAMMA],
            "EMC2302 and TMP1075 on the Nerd bus",
        );
    }
    // The Bitaxe Color carriers put their I²C on the Nerd pins: TPS546
    // and EMC2103, no TMP1075.
    if has(nerd_bus, addr::TPS546) && has(nerd_bus, addr::EMC2103) && !has(nerd_bus, addr::TMP1075)
    {
        return Detection::Choose(
            &[&BITAXE_NAJA_DUO, &BITAXE_GAMMA_HEX],
            "TPS546 and EMC2103 on the Color pins",
        );
    }
    Detection::Choose(&[], "no known chips answered")
}

/// Nothing answered on either bus and the stock firmware stored no
/// keys: a LilyGO T-Display-S3 on its own, or on the NerdNOS carrier.
/// Read-only signs: a NerdMiner's `config.json` in flash (both stock
/// firmwares are NerdMiner forks), the NerdNOS's thermistor divider on
/// GPIO2, which reads a steady mid-rail voltage where a bare board's pin
/// floats ([`crate::ntc::Config::plausible`]), and the carrier's 1.4 V
/// core rail on GPIO3 (rev 1.3 schematic: U1 pad 4 = VDD, the
/// TPS566247's output, 1.398 V by its divider). Thermistor and rail
/// together exist on no other board: that is the NerdNOS, taken outright
/// (2026-10-08; before, it was offered first and chosen by hand). The
/// thermistor alone still only offers it: nothing may drive a chip's
/// reset line on a guess. A bare board with NerdMiner's settings becomes
/// the devkit as before; without any sign the choice is open.
pub fn bare_board(
    nerdminer_settings: bool,
    thermistor: bool,
    core_rail_mv: Option<u16>,
) -> Detection {
    if thermistor && core_rail_mv.is_some_and(nerdnos_rail) {
        return Detection::Board(
            &NERDNOS,
            "a thermistor divider on GPIO2 and the 1.4 V core rail on GPIO3: the NerdNOS carrier",
        );
    }
    match (thermistor, nerdminer_settings) {
        (true, true) => Detection::Choose(
            &[&NERDNOS, &T_DISPLAY_S3_DEVKIT],
            "a thermistor divider on GPIO2 and a NerdMiner's settings in flash: the NerdNOS, or a T-Display-S3",
        ),
        (true, false) => Detection::Choose(
            &[&NERDNOS, &T_DISPLAY_S3_DEVKIT],
            "a thermistor divider on GPIO2: the NerdNOS, or a T-Display-S3",
        ),
        (false, true) => Detection::Board(
            &T_DISPLAY_S3_DEVKIT,
            "nothing answered, a NerdMiner's settings in flash: the T-Display-S3 devkit",
        ),
        (false, false) => Detection::Choose(&[], "no known chips answered"),
    }
}

/// The NerdNOS core rail as GPIO3 reads it: 1.4 V nominal (1393–1400 mV
/// seen on the .46), with room for the divider's tolerance and a rail
/// under load. A floating pin or a 3.3 V signal is not it.
pub fn nerdnos_rail(mv: u16) -> bool {
    (1250..=1550).contains(&mv)
}

/// Stock NVS first, then the bus.
pub fn detect(
    stock: (Option<&str>, Option<&str>, Option<&str>),
    bitaxe_bus: &[u8],
    nerd_bus: &[u8],
    vin_v: Option<f32>,
) -> Detection {
    // A Bitaxe AxeOS runs and WarpAxe does not: named, nothing powered.
    // Before the bus, which would take a Supra or an Ultra for a Gamma.
    if let Some(m) = from_stock_unsupported(stock.0, stock.1, stock.2) {
        return Detection::Unsupported(m, "board model stored by the stock firmware (AxeOS)");
    }
    // The stock keys only ever name a tested board outright (the Bitaxe
    // Gamma); the bus takes the Nano, the Gaia and the Gamma on its own.
    if let Some(p) = from_stock(stock.0, stock.1, stock.2) {
        return Detection::Board(p, "board model stored by the stock firmware (AxeOS)");
    }
    // Board versions of boards that have never run WarpAxe: offered,
    // not taken — unless the bus names the same board on its own (the
    // Nano's 800 next to its mux and input monitor).
    if let Some(p) = from_stock_choice(stock.2) {
        if let Detection::Board(b, why) = from_bus(bitaxe_bus, nerd_bus, vin_v) {
            if b.name == p.name {
                return Detection::Board(b, why);
            }
        }
        return Detection::Choose(
            core::slice::from_ref(match p.name {
                "bitaxe-gamma-duo" => &&BITAXE_GAMMA_DUO,
                "bitaxe-gamma-turbo" => &&BITAXE_GAMMA_TURBO,
                "bitaxe-naja-duo" => &&BITAXE_NAJA_DUO,
                "bitaxe-gamma-hex" => &&BITAXE_GAMMA_HEX,
                _ => &&BITFORGE_NANO,
            }),
            "board version stored by the stock firmware",
        );
    }
    from_bus(bitaxe_bus, nerd_bus, vin_v)
}

#[cfg(test)]
mod tests {
    #[test]
    fn axeos_rotation_maps_per_display() {
        // OLED Bitaxe: rotation 0 is WarpAxe's flipped.
        assert!(flip_for_axeos_rotation(Some("801"), 0.0));
        assert!(!flip_for_axeos_rotation(Some("801"), 180.0));
        assert!(flip_for_axeos_rotation(None, 0.0));
        // Color boards: rotation 0 is the profile's own orientation.
        assert!(!flip_for_axeos_rotation(Some("1201"), 0.0));
        assert!(flip_for_axeos_rotation(Some("1201"), 180.0));
        assert!(!flip_for_axeos_rotation(Some("1300"), 0.0));
    }

    #[test]
    fn the_nerdnos_is_taken_with_thermistor_and_rail_only() {
        use super::*;
        match bare_board(false, true, Some(1398)) {
            Detection::Board(p, _) => assert_eq!(p.name, "nerdnos"),
            other => panic!("{other:?}"),
        }
        // Rail without the thermistor, thermistor without the rail, or a
        // rail reading that is not 1.4 V: offered, not taken.
        assert!(matches!(
            bare_board(false, false, Some(1398)),
            Detection::Choose(..)
        ));
        assert!(matches!(
            bare_board(false, true, None),
            Detection::Choose(..)
        ));
        assert!(matches!(
            bare_board(true, true, Some(3290)),
            Detection::Choose(..)
        ));
        assert!(matches!(
            bare_board(false, true, Some(0)),
            Detection::Choose(..)
        ));
        assert!(
            matches!(bare_board(true, false, None), Detection::Board(p, _) if p.name == "t-display-s3")
        );
    }

    use super::*;

    const NONE: (Option<&str>, Option<&str>, Option<&str>) = (None, None, None);

    #[test]
    fn a_gamma_from_axeos_nvs_is_taken() {
        let d = detect((Some("gamma"), Some("BM1370"), Some("602")), &[], &[], None);
        assert!(matches!(d, Detection::Board(p, _) if p.name == "bitaxe-gamma"));
    }

    #[test]
    fn bitaxes_on_other_chips_are_named_not_powered() {
        // The bus a Supra Hex shows (TPS546, EMC2302 at the EMC2103's
        // address, TMP1075, OLED) would offer a Gamma Turbo; a Supra's
        // (TPS546, EMC2101) would be taken as a Gamma.
        let hex = [addr::TPS546, addr::EMC2302, addr::TMP1075, addr::SSD1306];
        let one = [addr::TPS546, addr::EMC2101, addr::SSD1306];
        let named = |s: (Option<&str>, Option<&str>, Option<&str>), bus: &[u8]| match detect(
            s,
            bus,
            &[],
            Some(12.1),
        ) {
            Detection::Unsupported(m, _) => Some(m),
            _ => None,
        };
        assert_eq!(
            named((Some("SupraHex"), Some("BM1368"), Some("701")), &hex),
            Some("Bitaxe Supra Hex (6× BM1368)")
        );
        // A board version ESP-Miner does not list yet: the chip names it.
        assert_eq!(
            named((None, Some("BM1368"), Some("703")), &hex),
            Some("Bitaxe (BM1368)")
        );
        assert_eq!(
            named((Some("supra"), None, None), &one),
            Some("Bitaxe Supra (BM1368)")
        );
        assert_eq!(
            named((None, None, Some("204")), &one),
            Some("Bitaxe Ultra (BM1366)")
        );
        assert_eq!(
            named((None, None, Some("0.11")), &one),
            Some("Bitaxe Ultra (BM1366)")
        );
        assert_eq!(
            named((None, None, Some("2.2")), &one),
            Some("Bitaxe Max (BM1397)")
        );
        // TinyChipHub Zyber: named by the model its factory NVS stores,
        // also on the BM1370 boards; its board versions alone name nothing.
        assert_eq!(
            named((Some("zyber8g"), Some("BM1370"), Some("1100")), &hex),
            Some("TinyChipHub Zyber8G (8× BM1370)")
        );
        assert_eq!(
            named((Some("zyber8gplus"), Some("BM1370"), Some("1200")), &hex),
            Some("TinyChipHub Zyber8G+ (8× BM1370)")
        );
        assert_eq!(
            named((Some("zyber12g"), Some("BM1370"), Some("1200")), &hex),
            Some("TinyChipHub Zyber12G (12× BM1370)")
        );
        assert_eq!(
            named((Some("zyber8s"), Some("BM1368"), Some("1000")), &hex),
            Some("TinyChipHub Zyber8S (8× BM1368)")
        );
        assert_eq!(
            named((Some("Zyber16X"), Some("BM1370"), None), &hex),
            Some("TinyChipHub Zyber")
        );
        assert_eq!(
            from_stock_unsupported(None, Some("BM1370"), Some("1100")),
            None
        );
        assert_eq!(from_stock_unsupported(Some("zyb"), None, None), None);
        assert_eq!(
            from_stock_unsupported(Some("ZYBER8G"), None, None),
            Some("TinyChipHub Zyber8G (8× BM1370)")
        );
        // A multibyte character across byte 5: no panic, no match.
        assert_eq!(from_stock_unsupported(Some("zybe€"), None, None), None);
        assert_eq!(
            named((Some("Hex"), Some("BM1366"), Some("303")), &hex),
            Some("Bitaxe Hex (6× BM1366)")
        );
        assert_eq!(
            named((Some("max"), Some("BM1397"), Some("102")), &one),
            Some("Bitaxe Max (BM1397)")
        );
        assert_eq!(
            named((None, Some(" bm1366 "), None), &one),
            Some("Bitaxe (BM1366)")
        );
        // The boards WarpAxe runs keep their way in.
        assert!(
            matches!(detect((Some("gamma"), Some("BM1370"), Some("601")), &one, &[], Some(5.1)), Detection::Board(p, _) if p.name == "bitaxe-gamma")
        );
        for v in ["650", "801", "1201", "1300", "800"] {
            assert_eq!(
                from_stock_unsupported(Some("x"), Some("BM1370"), Some(v)),
                None,
                "{v}"
            );
        }
        assert_eq!(
            from_stock_unsupported(None, Some("BM1373"), Some("1201")),
            None
        );
        assert_eq!(from_stock_unsupported(None, None, None), None);
        assert_eq!(from_stock_unsupported(Some(""), Some(""), Some("")), None);
    }

    #[test]
    fn other_axeos_models_fall_through_to_the_bus() {
        assert_eq!(from_stock(Some("supra"), Some("BM1368"), Some("401")), None);
        assert_eq!(from_stock(Some("gamma"), Some("BM1370"), Some("800")), None);
    }

    #[test]
    fn a_gamma_from_its_chips_is_taken() {
        let d = detect(
            NONE,
            &[addr::TPS546, addr::EMC2101, addr::SSD1306],
            &[],
            None,
        );
        assert!(matches!(d, Detection::Board(p, _) if p.name == "bitaxe-gamma"));
    }

    #[test]
    fn nerd_boards_are_offered_not_taken() {
        match detect(NONE, &[], &[addr::EMC2302, addr::TMP1075], None) {
            Detection::Choose(c, _) => {
                assert_eq!(
                    c.iter().map(|p| p.name).collect::<Vec<_>>(),
                    ["nerdqaxe-plusplus", "nerdoctaxe-gamma"]
                );
            }
            d => panic!("{d:?}"),
        }
        match detect(
            NONE,
            &[],
            &[addr::EMC2302, addr::TMP1075, addr::FXL6408],
            None,
        ) {
            Detection::Choose(c, _) => assert_eq!(c.len(), 2),
            d => panic!("{d:?}"),
        }
        // The regulator's address answering changes nothing: NerdOS
        // reads it only after the LDO is up, so the probe may not see it.
        match detect(
            NONE,
            &[],
            &[addr::EMC2302, addr::TMP1075, addr::TPS5364X],
            None,
        ) {
            Detection::Choose(c, _) => assert_eq!(c.len(), 2),
            d => panic!("{d:?}"),
        }
        match detect(NONE, &[], &[addr::TPS546, addr::EMC2101], None) {
            Detection::Choose(c, _) => assert_eq!(c[0].name, "nerdaxe-gamma"),
            d => panic!("{d:?}"),
        }
    }

    /// The Gaia and the NerdAxe Gamma share the bus; the regulator's
    /// input voltage decides. 12 V: the Gaia, taken (tested). 5 V: the
    /// Gamma, offered (never run). No reading: both offered.
    #[test]
    fn the_gaia_is_taken_on_12_v_the_nerdaxe_gamma_offered_on_5_v() {
        let bus = [addr::TPS546, addr::EMC2101];
        assert!(matches!(
            detect(NONE, &[], &bus, Some(12.1)),
            Detection::Board(p, _) if p.name == "nerdaxe-gaia"
        ));
        assert!(matches!(
            detect(NONE, &[], &bus, Some(5.05)),
            Detection::Choose([p], _) if p.name == "nerdaxe-gamma"
        ));
        assert!(matches!(
            detect(NONE, &[], &bus, None),
            Detection::Choose([a, b], _) if a.name == "nerdaxe-gamma" && b.name == "nerdaxe-gaia"
        ));
        // A reading that fits neither class: offered, as without one.
        assert!(matches!(
            detect(NONE, &[], &bus, Some(8.0)),
            Detection::Choose([_, _], _)
        ));
        assert!(TESTED.contains(&NERDAXE_GAIA.name));
        assert!(!TESTED.contains(&NERDAXE_GAMMA.name));
    }

    #[test]
    fn only_tested_boards_are_ever_taken() {
        assert!(TESTED.contains(&BITAXE_GAMMA.name));
    }

    #[test]
    fn a_rev7_nerdqaxe_is_offered_not_taken() {
        let d = detect(
            NONE,
            &[],
            &[addr::EMC2302, addr::TMP1075, addr::TPS546],
            None,
        );
        assert!(matches!(d, Detection::Choose([p], _) if p.name == NERDQAXE_PLUSPLUS_REV7.name));
        assert!(!TESTED.contains(&NERDQAXE_PLUSPLUS_REV7.name), "never run");
    }

    /// The mux and the input monitor exist on no other board: taken from
    /// the bus (tested since 2026-09-29). The stock firmware's board
    /// version 800 alone (no bus probe to confirm it) is still only
    /// offered; with the bus agreeing it is taken.
    #[test]
    fn a_nano_is_taken_from_its_bus_and_offered_from_its_board_version_alone() {
        let d = detect(
            (Some("BITFORGE_NANO"), Some("BM1370"), Some("800")),
            &[],
            &[],
            None,
        );
        assert!(matches!(d, Detection::Choose([p], _) if p.name == BITFORGE_NANO.name));
        let nano = [addr::TPS546, addr::INA260, addr::PCA9544];
        let d = detect(
            (Some("BITFORGE_NANO"), Some("BM1370"), Some("800")),
            &nano,
            &[],
            Some(12.2),
        );
        assert!(matches!(d, Detection::Board(p, _) if p.name == BITFORGE_NANO.name));
        let d = detect(NONE, &nano, &[], None);
        assert!(matches!(d, Detection::Board(p, _) if p.name == BITFORGE_NANO.name));
        assert!(TESTED.contains(&BITFORGE_NANO.name));
        // A Gamma still needs its EMC2101 on the root bus.
        let d = detect(NONE, &[addr::TPS546, addr::EMC2101], &[], None);
        assert!(matches!(d, Detection::Board(p, _) if p.name == BITAXE_GAMMA.name));
        // Mux without the monitor, or the other way round: nothing is
        // taken, and a Gamma is not named either.
        let d = detect(NONE, &[addr::TPS546, addr::PCA9544], &[], None);
        assert_eq!(d, Detection::Choose(&[], "no known chips answered"));
    }

    #[test]
    fn the_axeos_boards_are_offered_by_version_or_bus_never_taken() {
        for (v, name) in [
            ("650", "bitaxe-gamma-duo"),
            ("801", "bitaxe-gamma-turbo"),
            ("1201", "bitaxe-naja-duo"),
            ("1300", "bitaxe-gamma-hex"),
        ] {
            let d = detect((Some("GammaDuo"), Some("BM1370"), Some(v)), &[], &[], None);
            assert!(
                matches!(d, Detection::Choose([p], _) if p.name == name),
                "{v}"
            );
            assert!(!TESTED.contains(&name));
        }
        // 650 lies inside the Gamma's 600 range but is the Duo.
        let d = detect((Some("gamma"), Some("BM1370"), Some("650")), &[], &[], None);
        assert!(matches!(d, Detection::Choose([p], _) if p.name == "bitaxe-gamma-duo"));
        let d = detect(NONE, &[addr::TPS546, addr::EMC2103], &[], None);
        assert!(matches!(d, Detection::Choose([p], _) if p.name == "bitaxe-gamma-turbo"));
        let d = detect(NONE, &[], &[addr::TPS546, addr::EMC2103], None);
        assert!(
            matches!(d, Detection::Choose([a, b], _) if a.name == "bitaxe-naja-duo" && b.name == "bitaxe-gamma-hex")
        );
        // A Nerd board with a TMP1075 next to the same address stays a Nerd board.
        let d = detect(NONE, &[], &[addr::EMC2302, addr::TMP1075], None);
        assert!(matches!(d, Detection::Choose([a, ..], _) if a.name == "nerdqaxe-plusplus"));
    }

    #[test]
    fn nothing_on_either_bus_offers_nothing() {
        assert_eq!(
            detect(NONE, &[], &[], None),
            Detection::Choose(&[], "no known chips answered")
        );
    }

    /// A thermistor on GPIO2 puts the NerdNOS first in the choice, never
    /// takes it; NerdMiner's settings alone still make the devkit.
    #[test]
    fn a_bare_t_display_is_the_devkit_a_thermistor_offers_the_nerdnos() {
        assert!(
            matches!(bare_board(true, false, None), Detection::Board(p, _) if p.name == "t-display-s3")
        );
        for settings in [true, false] {
            match bare_board(settings, true, None) {
                Detection::Choose(c, _) => assert_eq!(
                    c.iter().map(|p| p.name).collect::<Vec<_>>(),
                    ["nerdnos", "t-display-s3"]
                ),
                d => panic!("{d:?}"),
            }
        }
        assert_eq!(
            bare_board(false, false, None),
            Detection::Choose(&[], "no known chips answered")
        );
        assert!(TESTED.contains(&NERDNOS.name));
    }
}

//! The configurable parts of the device display, as names.
//!
//! The layout of every screen is fixed in the firmware; what the owner
//! chooses is *which* LCD pages the page key steps through (and in what
//! order, and which one is home), *what* the cockpit's places show, and
//! *which* OLED slides rotate. Each choice is a short name that goes
//! through `PATCH /api/system` and into NVS. This crate holds the names,
//! the factory defaults (today's screens, so an update changes nothing
//! on an existing device) and the checks: anything outside the tables
//! is refused before it is stored.

#![forbid(unsafe_code)]

/// The LCD pages, in their factory order. `cockpit` is the home page.
pub const PAGES: &[&str] = &["cockpit", "bitcoin", "device", "pool", "log"];

/// What the cockpit's large number can be.
pub const BIG: &[&str] = &["hashrate", "efficiency", "power"];

/// What each of the three places in the cockpit's right column can
/// show. `sharesph` (accepted shares per hour) is the chipless board's
/// yield figure, meaningful there only.
pub const SLOTS: &[&str] = &[
    "chip",
    "vr",
    "power",
    "efficiency",
    "fan",
    "fanpct",
    "shares",
    "bestdiff",
    "height",
    "price",
    "pooldiff",
    "uptime",
    "vin",
    "sharesph",
];

/// The cockpit's footer line: what stands left, middle and right.
pub const FOOTERS: &[&str] = &[
    "pool-shares-ip",
    "pool-ip",
    "shares-uptime",
    "pool-uptime",
    "ip-uptime",
    "worker-ip",
];

/// The OLED's slides in their factory order: five with the hashrate
/// large on the left and one value on the right, three full-width info
/// slides, the wordmark.
pub const OLED_SLIDES: &[&str] = &[
    "temp",
    "power",
    "efficiency",
    "shares",
    "best",
    "pool",
    "block",
    "wifi",
    "logo",
];

/// Factory defaults = the screens of v0.1.30.
pub const DEFAULT_BIG: &str = "hashrate";
/// The right column on a board with chips: chip temperature, power,
/// efficiency.
pub const DEFAULT_SLOTS: [&str; 3] = ["chip", "power", "efficiency"];
/// The right column on the chipless board, which has neither power nor
/// a rate that makes J/TH mean anything: chip temperature, shares per
/// hour, uptime.
pub const DEFAULT_SLOTS_DEVKIT: [&str; 3] = ["chip", "sharesph", "uptime"];
/// Pool, shares and IP in the footer line.
pub const DEFAULT_FOOTER: &str = "pool-shares-ip";
/// Seconds an OLED slide stays.
pub const DEFAULT_OLED_INTERVAL_S: u8 = 4;
/// The shortest and longest slide time a PATCH may set.
pub const OLED_INTERVAL_RANGE_S: (u8, u8) = (1, 60);

/// The default cockpit places for a board (`devkit`: the chipless one).
pub fn default_slots(devkit: bool) -> [&'static str; 3] {
    if devkit {
        DEFAULT_SLOTS_DEVKIT
    } else {
        DEFAULT_SLOTS
    }
}

/// The page list as the display steps through it: the configured
/// names that exist, in their order; the full factory list when the
/// configured one is empty or holds nothing known (a config written by
/// hand, or by a later firmware with pages this one lacks).
pub fn pages(configured: &[String]) -> Vec<&'static str> {
    let known: Vec<&'static str> = configured
        .iter()
        .filter_map(|p| PAGES.iter().copied().find(|k| k == p))
        .collect();
    if known.is_empty() {
        PAGES.to_vec()
    } else {
        known
    }
}

/// The index of the home page in `pages`: the configured start page
/// where it is on the list, else the first page.
pub fn start_index(pages: &[&str], start: &str) -> usize {
    pages.iter().position(|p| *p == start).unwrap_or(0)
}

/// The three cockpit places: the configured ones when all three are
/// known, else the board's default.
pub fn slots(configured: &[String], devkit: bool) -> [&'static str; 3] {
    let known: Vec<&'static str> = configured
        .iter()
        .filter_map(|s| SLOTS.iter().copied().find(|k| k == s))
        .collect();
    match known.as_slice() {
        [a, b, c] if configured.len() == 3 => [a, b, c],
        _ => default_slots(devkit),
    }
}

/// The large number: the configured name if known, else the hashrate.
pub fn big(configured: &str) -> &'static str {
    BIG.iter()
        .copied()
        .find(|k| *k == configured)
        .unwrap_or(DEFAULT_BIG)
}

/// The footer: the configured name if known, else the factory one.
pub fn footer(configured: &str) -> &'static str {
    FOOTERS
        .iter()
        .copied()
        .find(|k| *k == configured)
        .unwrap_or(DEFAULT_FOOTER)
}

/// The OLED rotation: the configured slides that exist, in order; all
/// of them when none is known.
pub fn oled_slides(configured: &[String]) -> Vec<&'static str> {
    let known: Vec<&'static str> = configured
        .iter()
        .filter_map(|s| OLED_SLIDES.iter().copied().find(|k| k == s))
        .collect();
    if known.is_empty() {
        OLED_SLIDES.to_vec()
    } else {
        known
    }
}

/// Seconds per OLED slide: the configured value inside the range, else
/// the default.
pub fn oled_interval_s(configured: u8) -> u8 {
    let (lo, hi) = OLED_INTERVAL_RANGE_S;
    if (lo..=hi).contains(&configured) {
        configured
    } else {
        DEFAULT_OLED_INTERVAL_S
    }
}

/// Why a PATCH value is refused; the text goes into the 400 reply.
fn unknown(field: &str, value: &str, table: &[&str]) -> String {
    format!("{field}: {value:?} is not one of {}", table.join(", "))
}

/// A list of names: every one known, none twice, at least one, at most
/// the table's length.
fn check_list(field: &str, values: &[String], table: &[&str]) -> Result<(), String> {
    if values.is_empty() {
        return Err(format!("{field}: at least one"));
    }
    if values.len() > table.len() {
        return Err(format!("{field}: at most {}", table.len()));
    }
    for (i, v) in values.iter().enumerate() {
        if !table.contains(&v.as_str()) {
            return Err(unknown(field, v, table));
        }
        if values[..i].contains(v) {
            return Err(format!("{field}: {v:?} twice"));
        }
    }
    Ok(())
}

/// `displayPages`: the pages the key steps through, in order.
pub fn check_pages(values: &[String]) -> Result<(), String> {
    check_list("displayPages", values, PAGES)
}

/// `displayStartPage`: a page name that is on the (new) page list.
pub fn check_start_page(value: &str, pages: &[String]) -> Result<(), String> {
    if !PAGES.contains(&value) {
        return Err(unknown("displayStartPage", value, PAGES));
    }
    if !pages.iter().any(|p| p == value) {
        return Err(format!(
            "displayStartPage: {value:?} is not among the displayPages"
        ));
    }
    Ok(())
}

/// `cockpitBig`.
pub fn check_big(value: &str) -> Result<(), String> {
    if BIG.contains(&value) {
        Ok(())
    } else {
        Err(unknown("cockpitBig", value, BIG))
    }
}

/// `cockpitSlots`: exactly three known names (the same one more than
/// once is allowed), or an empty list for the board's default.
pub fn check_slots(values: &[String]) -> Result<(), String> {
    if values.is_empty() {
        return Ok(());
    }
    if values.len() != 3 {
        return Err("cockpitSlots: three places (or an empty list for the default)".into());
    }
    for v in values {
        if !SLOTS.contains(&v.as_str()) {
            return Err(unknown("cockpitSlots", v, SLOTS));
        }
    }
    Ok(())
}

/// `cockpitFooter`.
pub fn check_footer(value: &str) -> Result<(), String> {
    if FOOTERS.contains(&value) {
        Ok(())
    } else {
        Err(unknown("cockpitFooter", value, FOOTERS))
    }
}

/// `oledSlides`: the slides that rotate, in order.
pub fn check_oled_slides(values: &[String]) -> Result<(), String> {
    check_list("oledSlides", values, OLED_SLIDES)
}

/// `oledInterval`: seconds per slide.
pub fn check_oled_interval(value: i64) -> Result<(), String> {
    let (lo, hi) = OLED_INTERVAL_RANGE_S;
    if (i64::from(lo)..=i64::from(hi)).contains(&value) {
        Ok(())
    } else {
        Err(format!("oledInterval: {value} outside {lo}–{hi} s"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn defaults_are_the_screens_of_v0_1_30() {
        assert_eq!(pages(&[]), PAGES);
        assert_eq!(start_index(PAGES, "cockpit"), 0);
        assert_eq!(slots(&[], false), ["chip", "power", "efficiency"]);
        assert_eq!(slots(&[], true), ["chip", "sharesph", "uptime"]);
        assert_eq!(big(""), "hashrate");
        assert_eq!(footer(""), "pool-shares-ip");
        assert_eq!(oled_slides(&[]), OLED_SLIDES);
        assert_eq!(oled_interval_s(0), 4);
    }

    #[test]
    fn configured_pages_keep_their_order_and_drop_unknown_ones() {
        assert_eq!(
            pages(&s(&["pool", "cockpit", "later"])),
            ["pool", "cockpit"]
        );
        assert_eq!(pages(&s(&["later"])), PAGES);
        assert_eq!(start_index(&["pool", "cockpit"], "cockpit"), 1);
        assert_eq!(start_index(&["pool", "cockpit"], "log"), 0);
    }

    #[test]
    fn slots_need_all_three_known() {
        assert_eq!(
            slots(&s(&["vr", "fan", "uptime"]), false),
            ["vr", "fan", "uptime"]
        );
        assert_eq!(slots(&s(&["vr", "fan"]), false), DEFAULT_SLOTS);
        assert_eq!(slots(&s(&["vr", "fan", "nope"]), false), DEFAULT_SLOTS);
        assert_eq!(oled_interval_s(61), 4);
        assert_eq!(oled_interval_s(7), 7);
    }

    #[test]
    fn checks_refuse_what_is_not_in_the_tables() {
        assert!(check_pages(&s(&["cockpit", "log"])).is_ok());
        assert!(check_pages(&[]).is_err());
        assert!(check_pages(&s(&["cockpit", "cockpit"])).is_err());
        assert!(check_pages(&s(&["cockpit", "settings"])).is_err());
        assert!(check_start_page("log", &s(&["cockpit", "log"])).is_ok());
        assert!(check_start_page("pool", &s(&["cockpit", "log"])).is_err());
        assert!(check_start_page("home", &s(&["cockpit"])).is_err());
        assert!(check_big("power").is_ok());
        assert!(check_big("temp").is_err());
        assert!(check_slots(&[]).is_ok());
        assert!(check_slots(&s(&["chip", "chip", "chip"])).is_ok());
        assert!(check_slots(&s(&["chip", "power"])).is_err());
        assert!(check_slots(&s(&["chip", "power", "clock"])).is_err());
        assert!(check_footer("worker-ip").is_ok());
        assert!(check_footer("pool").is_err());
        assert!(check_oled_slides(&s(&["logo"])).is_ok());
        assert!(check_oled_slides(&s(&["logo", "logo"])).is_err());
        assert!(check_oled_slides(&s(&["price"])).is_err());
        assert!(check_oled_interval(1).is_ok());
        assert!(check_oled_interval(60).is_ok());
        assert!(check_oled_interval(0).is_err());
        assert!(check_oled_interval(61).is_err());
        let why = check_big("temp").unwrap_err();
        assert!(
            why.contains("cockpitBig") && why.contains("hashrate"),
            "{why}"
        );
    }
}

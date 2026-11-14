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

/// The LCD pages there are. `clock` (v0.1.46) is not among the factory
/// pages: an owner adds it.
pub const PAGES: &[&str] = &["cockpit", "bitcoin", "device", "pool", "log", "clock"];
/// The factory pages and their order. `cockpit` is the home page.
pub const DEFAULT_PAGES: &[&str] = &["cockpit", "bitcoin", "device", "pool", "log"];

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

/// The OLED's slides there are: five with the hashrate large on the left
/// and one value on the right, three full-width info slides, the
/// wordmark, and the clock (v0.1.46, not among the factory slides).
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
    "clock",
];
/// The factory slides and their order.
pub const DEFAULT_OLED_SLIDES: &[&str] = &[
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
/// names that exist, in their order; the factory list when the
/// configured one is empty or holds nothing known (a config written by
/// hand, or by a later firmware with pages this one lacks).
pub fn pages(configured: &[String]) -> Vec<&'static str> {
    let known: Vec<&'static str> = configured
        .iter()
        .filter_map(|p| PAGES.iter().copied().find(|k| k == p))
        .collect();
    if known.is_empty() {
        DEFAULT_PAGES.to_vec()
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

/// The OLED rotation: the configured slides that exist, in order; the
/// factory slides when none is known.
pub fn oled_slides(configured: &[String]) -> Vec<&'static str> {
    let known: Vec<&'static str> = configured
        .iter()
        .filter_map(|s| OLED_SLIDES.iter().copied().find(|k| k == s))
        .collect();
    if known.is_empty() {
        DEFAULT_OLED_SLIDES.to_vec()
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

/// The display fields of a `PATCH /api/system` body, checked together:
/// every list a list of names, every name in its table, the start page
/// on the page list the PATCH leaves behind (`current_pages` is the
/// stored list, used when the PATCH does not send one). `Some(why)` for
/// the 400 reply, `None` when the body passes (fields it does not carry
/// are not checked).
pub fn check_patch(v: &serde_json::Value, current_pages: &[String]) -> Option<String> {
    let list = |k: &str| -> Option<Result<Vec<String>, String>> {
        let x = v.get(k)?;
        Some(
            serde_json::from_value::<Vec<String>>(x.clone())
                .map_err(|_| format!("{k}: a list of names")),
        )
    };
    let text = |k: &str| -> Option<Result<String, String>> {
        let x = v.get(k)?;
        Some(x.as_str().map(str::to_string).ok_or(format!("{k}: a name")))
    };
    let checks: [Option<Result<(), String>>; 6] = [
        list("displayPages").map(|r| r.and_then(|l| check_pages(&l))),
        text("displayStartPage").map(|r| {
            r.and_then(|s| {
                let pages = match list("displayPages") {
                    Some(Ok(l)) => l,
                    Some(Err(e)) => return Err(e),
                    None => current_pages.to_vec(),
                };
                let pages = if pages.is_empty() {
                    DEFAULT_PAGES.iter().map(|p| p.to_string()).collect()
                } else {
                    pages
                };
                check_start_page(&s, &pages)
            })
        }),
        text("cockpitBig").map(|r| r.and_then(|s| check_big(&s))),
        list("cockpitSlots").map(|r| r.and_then(|l| check_slots(&l))),
        text("cockpitFooter").map(|r| r.and_then(|s| check_footer(&s))),
        list("oledSlides").map(|r| r.and_then(|l| check_oled_slides(&l))),
    ];
    for c in checks.into_iter().flatten() {
        if let Err(why) = c {
            return Some(why);
        }
    }
    if let Some(x) = v.get("oledInterval") {
        if let Err(why) = check_oled_interval(x.as_i64().unwrap_or(-1)) {
            return Some(why);
        }
    }
    for k in ["nightStart", "nightEnd"] {
        if let Some(x) = v.get(k) {
            if let Err(why) = check_minute_of_day(k, x.as_i64().unwrap_or(-1)) {
                return Some(why);
            }
        }
    }
    if let Some(x) = v.get("clockFormat") {
        let ok = x.as_str().is_some_and(|f| CLOCK_FORMATS.contains(&f));
        if !ok {
            return Some(unknown("clockFormat", &x.to_string(), CLOCK_FORMATS));
        }
    }
    if let Some(x) = v.get("nightBrightness") {
        if let Err(why) = check_night_brightness(x.as_i64().unwrap_or(-1)) {
            return Some(why);
        }
    }
    None
}

// ------------------------------------------------------------ night mode

/// Night mode's window by default: from 22:00 …
pub const DEFAULT_NIGHT_START_MIN: u16 = 22 * 60;
/// … to 07:00 (minutes of the day).
pub const DEFAULT_NIGHT_END_MIN: u16 = 7 * 60;
/// Backlight (LCD) or contrast (OLED) at night, percent of full; 0 =
/// the display goes dark.
pub const DEFAULT_NIGHT_BRIGHTNESS: u8 = 10;
/// Seconds a key press brings the full display back at night.
pub const NIGHT_WAKE_S: u64 = 30;

/// Whether `now` (minutes of the local day) lies in the night window
/// from `start` up to, not including, `end`. A window may run past
/// midnight (22:00–07:00); `start == end` is no window at all.
pub fn in_night(now: u16, start: u16, end: u16) -> bool {
    match start.cmp(&end) {
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => (start..end).contains(&now),
        std::cmp::Ordering::Greater => now >= start || now < end,
    }
}

/// The brightness at night, percent, kept to 0–100.
pub fn night_brightness(configured: u8) -> u8 {
    configured.min(100)
}

/// `nightStart` / `nightEnd`: a minute of the day, 0–1439.
pub fn check_minute_of_day(field: &str, value: i64) -> Result<(), String> {
    if (0..24 * 60).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{field}: {value} is not a minute of the day (0–1439)"
        ))
    }
}

/// `nightBrightness`: percent, 0–100.
pub fn check_night_brightness(value: i64) -> Result<(), String> {
    if (0..=100).contains(&value) {
        Ok(())
    } else {
        Err(format!("nightBrightness: {value} outside 0–100 %"))
    }
}

// ----------------------------------------------------------- time format

/// The clock's formats: by the time zone, or fixed.
pub const CLOCK_FORMATS: &[&str] = &["auto", "24h", "12h"];
/// Factory setting.
pub const DEFAULT_CLOCK_FORMAT: &str = "auto";

/// POSIX time zone rules that start like these read the clock in 12
/// hours: the United States and Canada with their summer time (so
/// Mexico's and Panama's "CST6" / "EST5" without it stay on 24 hours,
/// and China's "CST-8" is not Chicago), Arizona, Hawaii, Australia, New
/// Zealand, India, Pakistan and the Philippines. A heuristic: "12h" and
/// "24h" set it by hand.
const TWELVE_HOUR_ZONES: &[&str] = &[
    "EST5EDT",
    "CST6CDT",
    "MST7",
    "PST8PDT",
    "AKST9AKDT",
    "HST10",
    "AST4ADT",
    "NST3:30NDT",
    "AEST-10",
    "ACST-9",
    "AWST-8",
    "NZST-12",
    "IST-5:30",
    "PKT-5",
    "PST-8",
];

/// Whether the clock shows 12 hours with AM/PM: as set, or with `auto`
/// (or anything unknown) by the time zone.
pub fn twelve_hour(format: &str, tz: &str) -> bool {
    match format {
        "12h" => true,
        "24h" => false,
        _ => TWELVE_HOUR_ZONES.iter().any(|z| tz.starts_with(z)),
    }
}

/// An hour of the day on the 12-hour clock, and whether it is PM
/// (0 → 12 AM, 12 → 12 PM).
pub fn hour12(h: u8) -> (u8, bool) {
    let pm = h >= 12;
    let h = h % 12;
    (if h == 0 { 12 } else { h }, pm)
}

// ----------------------------------------------------------------- dates

/// The ISO 8601 week of a date, 1–53, from its year, day of the year
/// (0 = 1 January, as C's `tm_yday`) and weekday (0 = Sunday, as
/// `tm_wday`).
pub fn iso_week(year: i32, yday: u16, wday: u8) -> u8 {
    // Monday = 1 … Sunday = 7.
    let wd = if wday == 0 { 7 } else { i32::from(wday) };
    let w = (i32::from(yday) + 1 - wd + 10) / 7;
    if w < 1 {
        weeks_in(year - 1)
    } else if w > i32::from(weeks_in(year)) {
        1
    } else {
        w as u8
    }
}

/// 52 or 53: a year has 53 ISO weeks when 1 January is a Thursday, or
/// a Wednesday in a leap year.
fn weeks_in(year: i32) -> u8 {
    let jan1 = jan1_weekday(year);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    if jan1 == 4 || (leap && jan1 == 3) {
        53
    } else {
        52
    }
}

/// Weekday of 1 January (0 = Sunday), by Gauss.
fn jan1_weekday(year: i32) -> i32 {
    let y = year - 1;
    (1 + 5 * (y % 4) + 4 * (y % 100) + 6 * (y % 400)).rem_euclid(7)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_patch_is_checked_as_a_whole() {
        let cur = s(&["cockpit", "pool"]);
        assert_eq!(check_patch(&json!({"frequency": 600}), &cur), None);
        assert_eq!(
            check_patch(
                &json!({"displayPages": ["pool", "log"], "displayStartPage": "log"}),
                &cur
            ),
            None
        );
        assert!(check_patch(
            &json!({"displayPages": ["pool", "log"], "displayStartPage": "cockpit"}),
            &cur
        )
        .unwrap()
        .contains("displayStartPage"));
        assert_eq!(
            check_patch(&json!({"displayStartPage": "pool"}), &cur),
            None
        );
        assert!(check_patch(&json!({"displayStartPage": "log"}), &cur).is_some());
        assert_eq!(check_patch(&json!({"displayStartPage": "log"}), &[]), None);
        assert!(check_patch(&json!({"displayPages": "cockpit"}), &cur)
            .unwrap()
            .contains("a list of names"));
        assert!(check_patch(&json!({"displayPages": []}), &cur)
            .unwrap()
            .contains("at least one"));
        assert!(check_patch(&json!({"cockpitBig": 3}), &cur)
            .unwrap()
            .contains("a name"));
        assert!(
            check_patch(&json!({"cockpitSlots": ["chip", "clock", "power"]}), &cur)
                .unwrap()
                .contains("clock")
        );
        assert_eq!(check_patch(&json!({"cockpitSlots": []}), &cur), None);
        assert!(check_patch(&json!({"cockpitFooter": "pool"}), &cur).is_some());
        assert!(check_patch(&json!({"oledSlides": ["logo", "logo"]}), &cur)
            .unwrap()
            .contains("twice"));
        assert!(check_patch(&json!({"oledInterval": 0}), &cur)
            .unwrap()
            .contains("oledInterval"));
        assert!(check_patch(&json!({"oledInterval": "4"}), &cur).is_some());
        assert_eq!(
            check_patch(&json!({"oledInterval": 60, "oledSlides": ["temp"]}), &cur),
            None
        );
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn defaults_are_the_screens_of_v0_1_30() {
        assert_eq!(pages(&[]), ["cockpit", "bitcoin", "device", "pool", "log"]);
        assert_eq!(start_index(DEFAULT_PAGES, "cockpit"), 0);
        assert_eq!(slots(&[], false), ["chip", "power", "efficiency"]);
        assert_eq!(slots(&[], true), ["chip", "sharesph", "uptime"]);
        assert_eq!(big(""), "hashrate");
        assert_eq!(footer(""), "pool-shares-ip");
        assert_eq!(oled_slides(&[]), DEFAULT_OLED_SLIDES);
        assert!(!oled_slides(&[]).contains(&"clock"));
        assert_eq!(oled_interval_s(0), 4);
    }

    #[test]
    fn the_clock_is_a_choice_not_a_default() {
        assert!(PAGES.contains(&"clock") && !DEFAULT_PAGES.contains(&"clock"));
        assert!(OLED_SLIDES.contains(&"clock") && !DEFAULT_OLED_SLIDES.contains(&"clock"));
        assert_eq!(&PAGES[..DEFAULT_PAGES.len()], DEFAULT_PAGES);
        assert_eq!(
            &OLED_SLIDES[..DEFAULT_OLED_SLIDES.len()],
            DEFAULT_OLED_SLIDES
        );
        assert_eq!(pages(&s(&["clock", "cockpit"])), ["clock", "cockpit"]);
        assert!(check_pages(&s(&["clock"])).is_ok());
        assert!(check_start_page("clock", &s(&["cockpit", "clock"])).is_ok());
        assert_eq!(oled_slides(&s(&["clock", "temp"])), ["clock", "temp"]);
        assert!(check_oled_slides(&s(&["clock"])).is_ok());
        // An empty stored list falls back to the factory pages, so the
        // start page must be one of those.
        assert!(check_patch(&json!({"displayStartPage": "clock"}), &[]).is_some());
    }

    #[test]
    fn the_night_window_may_run_past_midnight() {
        let (s22, e7) = (DEFAULT_NIGHT_START_MIN, DEFAULT_NIGHT_END_MIN);
        assert!(in_night(22 * 60, s22, e7));
        assert!(in_night(23 * 60 + 59, s22, e7));
        assert!(in_night(0, s22, e7));
        assert!(in_night(6 * 60 + 59, s22, e7));
        assert!(!in_night(7 * 60, s22, e7));
        assert!(!in_night(12 * 60, s22, e7));
        assert!(!in_night(21 * 60 + 59, s22, e7));
        // Within one day.
        assert!(in_night(13 * 60, 12 * 60, 14 * 60));
        assert!(!in_night(14 * 60, 12 * 60, 14 * 60));
        // No window.
        assert!(!in_night(600, 600, 600));
        assert_eq!(night_brightness(250), 100);
        assert_eq!(night_brightness(0), 0);
    }

    #[test]
    fn night_fields_are_checked_in_a_patch() {
        let cur = s(&["cockpit"]);
        assert_eq!(
            check_patch(
                &json!({"nightStart": 0, "nightEnd": 1439, "nightBrightness": 0}),
                &cur
            ),
            None
        );
        assert!(check_patch(&json!({"nightStart": 1440}), &cur)
            .unwrap()
            .contains("nightStart"));
        assert!(check_patch(&json!({"nightEnd": -1}), &cur).is_some());
        assert!(check_patch(&json!({"nightEnd": "07:00"}), &cur).is_some());
        assert!(check_patch(&json!({"nightBrightness": 101}), &cur)
            .unwrap()
            .contains("nightBrightness"));
        assert_eq!(check_patch(&json!({"nightBrightness": 100}), &cur), None);
    }

    #[test]
    fn twelve_hours_follow_the_time_zone_unless_set() {
        assert!(twelve_hour("auto", "EST5EDT,M3.2.0,M11.1.0"));
        assert!(twelve_hour("auto", "PST8PDT,M3.2.0,M11.1.0"));
        assert!(twelve_hour("auto", "MST7"));
        assert!(twelve_hour("auto", "AEST-10AEDT,M10.1.0,M4.1.0/3"));
        assert!(twelve_hour("auto", "NZST-12NZDT,M9.5.0,M4.1.0/3"));
        assert!(!twelve_hour("auto", "CET-1CEST,M3.5.0,M10.5.0/3"));
        assert!(!twelve_hour("auto", "GMT0BST,M3.5.0/1,M10.5.0"));
        assert!(!twelve_hour("auto", "CST-8"), "China, not Chicago");
        assert!(!twelve_hour("auto", "CST6"), "Mexico City, no summer time");
        assert!(!twelve_hour("auto", "EST5"), "Panama");
        assert!(twelve_hour("auto", "AKST9AKDT,M3.2.0,M11.1.0"));
        assert!(twelve_hour("auto", "IST-5:30"));
        assert!(twelve_hour("auto", "PST-8"), "Manila");
        assert!(!twelve_hour("auto", "JST-9"));
        assert!(!twelve_hour("auto", "UTC0"));
        assert!(!twelve_hour("24h", "EST5EDT,M3.2.0,M11.1.0"));
        assert!(twelve_hour("12h", "CET-1CEST,M3.5.0,M10.5.0/3"));
        assert!(!twelve_hour("", "CET-1CEST"));
        assert_eq!(hour12(0), (12, false));
        assert_eq!(hour12(8), (8, false));
        assert_eq!(hour12(12), (12, true));
        assert_eq!(hour12(23), (11, true));
        let cur = s(&["cockpit"]);
        assert_eq!(check_patch(&json!({"clockFormat": "12h"}), &cur), None);
        assert!(check_patch(&json!({"clockFormat": "am"}), &cur)
            .unwrap()
            .contains("clockFormat"));
        assert!(check_patch(&json!({"clockFormat": 12}), &cur).is_some());
    }

    #[test]
    fn iso_weeks_at_the_turn_of_the_year() {
        // Sat 14 Nov 2026: day 317, week 46.
        assert_eq!(iso_week(2026, 317, 6), 46);
        // Thu 1 Jan 2026: week 1.
        assert_eq!(iso_week(2026, 0, 4), 1);
        // Fri 1 Jan 2027: still week 53 of 2026 (2026 starts on a Thursday).
        assert_eq!(iso_week(2027, 0, 5), 53);
        // Mon 4 Jan 2027: week 1.
        assert_eq!(iso_week(2027, 3, 1), 1);
        // Mon 29 Dec 2025: already week 1 of 2026.
        assert_eq!(iso_week(2025, 362, 1), 1);
        // Sun 1 Jan 2023: week 52 of 2022.
        assert_eq!(iso_week(2023, 0, 0), 52);
        // Thu 31 Dec 2020: week 53.
        assert_eq!(iso_week(2020, 365, 4), 53);
        assert_eq!(jan1_weekday(2026), 4);
        assert_eq!(jan1_weekday(2024), 1);
    }

    #[test]
    fn configured_pages_keep_their_order_and_drop_unknown_ones() {
        assert_eq!(
            pages(&s(&["pool", "cockpit", "later"])),
            ["pool", "cockpit"]
        );
        assert_eq!(pages(&s(&["later"])), DEFAULT_PAGES);
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

//! The restart marker: a note the firmware stores in NVS right before an
//! intentional restart (after an update, from the web UI, on the way
//! into download mode) and reads back at the next boot.
//!
//! Why: a Bitaxe Gamma Turbo of a user came back from the restart after
//! an update with reset reason `panic` and no core dump (2026-10-23;
//! docs/12). The marker leaves a trace that does not depend on the core
//! dump being written: the next boot sees that a restart was begun, for
//! what, by which image, and how far it got — the chips were still being
//! switched off, or the system reset itself had started — and says so in
//! the event log when the reset turned out to be a crash. When the reset
//! was the plain software reset it should be, the marker also tells the
//! new image that it was installed by the online update, which the
//! bootloader's own state does not on a bootloader without rollback
//! support (that user's "update installed" event was missing for that
//! reason). No I/O here: the firmware stores [`Marker::to_stored`] and
//! parses what it finds.

/// The NVS key the firmware stores the marker under (at most 15 bytes).
pub const KEY: &str = "restart";
/// The stored string is at most this long.
pub const MAX_STORED: usize = 96;
/// `why` and `from` are cut to this many characters each.
const MAX_FIELD: usize = 24;

/// How far the restart got when the marker was last written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The restart was begun: the chips are being switched off
    /// (`board::prepare_reset`, up to about three seconds).
    Chips,
    /// The chips are off and the system reset was called
    /// (`esp_restart`: the shutdown handlers, then the reset).
    Reset,
}

impl Step {
    fn code(self) -> char {
        match self {
            Step::Chips => 'c',
            Step::Reset => 'r',
        }
    }

    fn from_code(c: &str) -> Option<Step> {
        match c {
            "c" => Some(Step::Chips),
            "r" => Some(Step::Reset),
            _ => None,
        }
    }

    /// The step in English, for the log.
    pub fn text(self) -> &'static str {
        match self {
            Step::Chips => "switching the chips off",
            Step::Reset => "in the system reset",
        }
    }
}

/// What the firmware stores before a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    /// What the restart was for, as the writer names it: "update",
    /// "upload", "restart", "download mode".
    pub why: String,
    /// How far it got.
    pub step: Step,
    /// The image that began the restart (`warpaxe 0.1.36`).
    pub from: String,
    /// Its uptime then, seconds.
    pub uptime_s: u64,
}

/// A field cut to [`MAX_FIELD`] bytes at a character boundary, without
/// the separator or control characters.
fn field(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().filter(|c| *c != '|' && !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_FIELD {
            break;
        }
        out.push(c);
    }
    out
}

impl Marker {
    /// A marker at [`Step::Chips`].
    pub fn new(why: &str, from: &str, uptime_s: u64) -> Marker {
        Marker {
            why: field(why),
            step: Step::Chips,
            from: field(from),
            uptime_s,
        }
    }

    /// The same marker at a later step.
    pub fn at(mut self, step: Step) -> Marker {
        self.step = step;
        self
    }

    /// The string stored in NVS: `why|step|from|uptime`.
    pub fn to_stored(&self) -> String {
        let s = format!(
            "{}|{}|{}|{}",
            field(&self.why),
            self.step.code(),
            field(&self.from),
            self.uptime_s
        );
        debug_assert!(s.len() <= MAX_STORED);
        s
    }

    /// A stored string back; `None` for anything else (an older or a
    /// damaged entry), which the firmware then only removes.
    pub fn parse(s: &str) -> Option<Marker> {
        let mut it = s.trim_end_matches('\0').splitn(4, '|');
        let why = it.next()?;
        let step = Step::from_code(it.next()?)?;
        let from = it.next()?;
        let uptime_s = it.next()?.parse().ok()?;
        if why.is_empty() {
            return None;
        }
        Some(Marker {
            why: field(why),
            step,
            from: field(from),
            uptime_s,
        })
    }

    /// The restart installed an image (the online update, or an upload
    /// through the web UI, which the writer calls "ota"), so the next
    /// boot runs what was written.
    pub fn installs(&self) -> bool {
        matches!(self.why.as_str(), "update" | "ota")
    }

    /// The restart was the way into ROM download mode: what follows is
    /// a USB flash, which ends in a reset by the flashing tool.
    pub fn into_download_mode(&self) -> bool {
        self.why == "download mode"
    }
}

/// How the chip came back, as far as the marker cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reset {
    /// A software reset, a power-on, a USB or JTAG reset, a brownout:
    /// not a crash of the restart itself.
    Clean,
    /// A panic, an interrupt watchdog or a task watchdog: the firmware
    /// crashed.
    Crash,
    /// The RTC (or another system) watchdog. The firmware's own
    /// restart never ends in one, but a flashing tool does: esptool and
    /// the web flasher leave ROM download mode by arming the RTC
    /// watchdog, which reads as `wdt` at the next boot.
    Watchdog,
}

/// What the next boot makes of the marker it found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The reset was the software reset the restart asked for.
    Completed(Marker),
    /// The reset was a crash (a panic or a watchdog): the restart did
    /// not get past the marker's step.
    Crashed(Marker),
}

/// The marker against the reset reason. A crash is a crash, a watchdog
/// reset is one too. Anything else — the software reset, but also a
/// power cycle or a USB reset while the marker sat there — counts as
/// completed: the restart itself did not crash (a hang ended by a power
/// cycle is not told apart, docs/12).
///
/// After the way into download mode the RTC watchdog is not a crash of
/// that restart: the restart ends in the ROM, and the watchdog is how the
/// web flasher, espflash and `esptool -a watchdog-reset` leave download
/// mode (a USB reset or a power cycle are clean anyway). A panic or a
/// task or interrupt watchdog is one all the same: their hint in
/// `RTC_CNTL_STORE6_REG` is written by the panic handler alone and
/// cleared by nothing but an app boot, so with a download-mode marker it
/// can only be the marker's own run that crashed, after the marker and
/// before the ROM (the .41 and .45 on 2026-10-24: the console in the
/// torn-down CDC port, docs/12). It reaches the app with the first
/// software reset after the ROM session, however long that took.
pub fn outcome(m: Marker, reset: Reset) -> Outcome {
    match reset {
        Reset::Crash => Outcome::Crashed(m),
        Reset::Watchdog if m.into_download_mode() => Outcome::Completed(m),
        Reset::Watchdog => Outcome::Crashed(m),
        Reset::Clean => Outcome::Completed(m),
    }
}

/// Whether the last reset was a crash, for the crash banner, the 30 s
/// on USB-Serial/JTAG and the core dump's verdict. A panic or a task or
/// interrupt watchdog always is. The RTC watchdog is how the web
/// flasher, espflash and `esptool -a watchdog-reset` leave download
/// mode, so it is no crash on the first boot of an image (`first_boot`:
/// the app's ELF SHA differs from the one stored by the last run, or
/// none is stored, as after a first install over a stock firmware)
/// when no restart marker is open, and never after the way into
/// download mode (that marker's own rule, [`outcome`]). With any other
/// marker open — an update's, say — the watchdog is a crash of that
/// restart, first boot or not. Blind spot: a real RTC-watchdog hang in
/// the first boot of a new image, before the SHA is stored, is forgiven
/// at the second boot too (docs/12).
pub fn crashed(reset: Reset, marker: Option<&Marker>, first_boot: bool) -> bool {
    match reset {
        Reset::Crash => true,
        Reset::Clean => false,
        Reset::Watchdog => match marker {
            Some(m) if m.into_download_mode() => false,
            Some(_) => true,
            None => !first_boot,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_round_trips_through_the_stored_string() {
        let m = Marker::new("update", "warpaxe 0.1.36", 7823);
        assert_eq!(m.to_stored(), "update|c|warpaxe 0.1.36|7823");
        assert_eq!(Marker::parse(&m.to_stored()), Some(m.clone()));
        let r = m.clone().at(Step::Reset);
        assert_eq!(r.to_stored(), "update|r|warpaxe 0.1.36|7823");
        assert_eq!(Marker::parse(&r.to_stored()), Some(r));
        // As NVS hands it back, with the terminator.
        assert_eq!(
            Marker::parse("restart|c|warpaxe 0.1.36|12\0"),
            Some(Marker::new("restart", "warpaxe 0.1.36", 12))
        );
    }

    #[test]
    fn a_damaged_or_foreign_entry_parses_to_nothing() {
        for s in [
            "",
            "update",
            "update|c",
            "update|x|v|1",
            "|c|v|1",
            "update|c|v|many",
            "\0",
        ] {
            assert_eq!(Marker::parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn fields_are_cut_and_cannot_break_the_format() {
        let m = Marker::new("why|with|bars\n", &"v".repeat(60), 1);
        assert_eq!(m.why, "whywithbars");
        assert_eq!(m.from.len(), MAX_FIELD);
        let s = m.to_stored();
        assert!(s.len() <= MAX_STORED, "{}", s.len());
        assert_eq!(Marker::parse(&s), Some(m));
        // The longest marker fits the limit, in bytes, whatever the
        // characters.
        let long = Marker::new(&"w".repeat(100), &"f".repeat(100), u64::MAX);
        assert!(long.to_stored().len() <= MAX_STORED);
        let wide = Marker::new(&"ü".repeat(100), &"€".repeat(100), u64::MAX);
        assert!(
            wide.to_stored().len() <= MAX_STORED,
            "{}",
            wide.to_stored().len()
        );
        assert_eq!(wide.from.chars().count(), MAX_FIELD / 3);
        assert_eq!(Marker::parse(&wide.to_stored()), Some(wide));
    }

    #[test]
    fn a_watchdog_on_the_first_boot_of_an_image_is_the_flashing_tools_reset() {
        let update = Marker::new("update", "warpaxe 0.1.36", 10);
        let download = Marker::new("download mode", "warpaxe 0.1.36", 10);
        // wdt on the first boot without a marker: the web flasher's reset.
        assert!(!crashed(Reset::Watchdog, None, true));
        // wdt on a later boot of the same image: a real watchdog.
        assert!(crashed(Reset::Watchdog, None, false));
        // wdt with an update's marker open: a crash of that restart.
        assert!(crashed(Reset::Watchdog, Some(&update), true));
        assert!(crashed(Reset::Watchdog, Some(&update), false));
        // After the way into download mode the tool's reset, whatever
        // the image.
        assert!(!crashed(Reset::Watchdog, Some(&download), false));
        assert!(!crashed(Reset::Watchdog, Some(&download), true));
        // A panic, an interrupt or a task watchdog: always a crash.
        assert!(crashed(Reset::Crash, None, true));
        assert!(crashed(Reset::Crash, Some(&download), true));
        assert!(crashed(Reset::Crash, Some(&update), true));
        // The clean reasons never are.
        assert!(!crashed(Reset::Clean, None, false));
        assert!(!crashed(Reset::Clean, Some(&update), false));
    }

    #[test]
    fn the_outcome_follows_the_reset_reason() {
        let m = Marker::new("update", "warpaxe 0.1.34", 100).at(Step::Reset);
        assert_eq!(
            outcome(m.clone(), Reset::Crash),
            Outcome::Crashed(m.clone())
        );
        assert_eq!(
            outcome(m.clone(), Reset::Watchdog),
            Outcome::Crashed(m.clone())
        );
        assert_eq!(outcome(m.clone(), Reset::Clean), Outcome::Completed(m));
        // After download mode the RTC watchdog is the flashing tool's way
        // back and a USB reset or power cycle are clean; a panic can only
        // be the marker's own run, on its way into the ROM.
        let d = Marker::new("download mode", "warpaxe 0.1.36", 5).at(Step::Reset);
        for r in [Reset::Watchdog, Reset::Clean] {
            assert_eq!(outcome(d.clone(), r), Outcome::Completed(d.clone()));
        }
        assert_eq!(outcome(d.clone(), Reset::Crash), Outcome::Crashed(d));
        assert!(Marker::new("update", "", 0).installs());
        assert!(Marker::new("ota", "", 0).installs());
        assert!(!Marker::new("restart", "", 0).installs());
        assert!(!Marker::new("download mode", "", 0).installs());
        assert_eq!(Step::Chips.text(), "switching the chips off");
        assert_eq!(Step::Reset.text(), "in the system reset");
    }
}

//! What the board tells about itself when nobody is listening: the last
//! crash, the reason for the last reset, and a window on USB-Serial/JTAG
//! after a panic so the board can always be reflashed.

use std::sync::atomic::AtomicU32;
use std::time::{Duration, Instant};

use esp_idf_svc::sys::{self as sys, tusb};

pub static HTTP_HITS: AtomicU32 = AtomicU32::new(0);

static BOOT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn mark_boot() {
    let _ = BOOT.set(Instant::now());
}

pub fn uptime_s() -> u64 {
    BOOT.get().map_or(0, |t| t.elapsed().as_secs())
}

static BOOT_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// A number that tells this boot from the last one (random, for the
/// web UI's once-per-boot notices).
pub fn boot_id() -> u32 {
    *BOOT_ID.get_or_init(|| unsafe { sys::esp_random() })
}

/// Route ESP_LOG through the in-RAM ring the display shows.
pub fn start_log_capture() {
    unsafe { tusb::wa_log_capture_start() };
}

/// Why the chip last reset, as the IDF names it.
pub fn reset_reason() -> &'static str {
    match unsafe { sys::esp_reset_reason() } {
        sys::esp_reset_reason_t_ESP_RST_POWERON => "power-on",
        sys::esp_reset_reason_t_ESP_RST_SW => "software",
        sys::esp_reset_reason_t_ESP_RST_PANIC => "panic",
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => "int-wdt",
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "task-wdt",
        sys::esp_reset_reason_t_ESP_RST_WDT => "wdt",
        sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "deep-sleep",
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "brownout",
        sys::esp_reset_reason_t_ESP_RST_USB => "usb",
        sys::esp_reset_reason_t_ESP_RST_JTAG => "jtag",
        _ => "unknown",
    }
}

/// The RTC controller's own reset cause (`soc_reset_reason_t`: 0x01
/// power-on, 0x03 software, 0x0c CPU software, 0x09/0x10 RTC watchdog,
/// 0x15/0x16 the USB-Serial/JTAG controller's UART or JTAG side), next
/// to the IDF's reading above. The IDF turns
/// a software reset into `panic` when the panic handler's hint sits in
/// RTC memory, and the hint survives a ROM download-mode session until
/// an app boots: which reset a flashing tool ends the session with
/// decides whether an old hint is read then (docs/12).
pub fn reset_cause() -> u32 {
    unsafe { sys::esp_rom_get_reset_reason(0) }
}

/// The chip came up from a power-on (the supply or the EN pin), so the
/// board's rails were really down. A software, watchdog, USB or JTAG
/// reset kept the 12 V input and everything fed from it.
pub fn reset_was_power_on() -> bool {
    unsafe { sys::esp_reset_reason() == sys::esp_reset_reason_t_ESP_RST_POWERON }
}

/// The last reset as the restart marker reads it (`warpaxe_diag::restart`):
/// a crash, a watchdog without a crash, or clean.
///
/// The IDF names `panic`, `int-wdt` and `task-wdt` in two ways. From the
/// panic handler's hint in RTC memory, in which case the RTC cause is a
/// software reset (`esp_restart_noos` after the handler ran): this app
/// crashed, whatever else happened. Or from the RTC cause alone, MWDT0
/// or MWDT1 having reset the chip with no handler in between: a hang the
/// watchdog's last stage ended, or a tool's way out of ROM download mode
/// (esptool 4.9's hard reset from a download mode the app had forced
/// arrived as MWDT0 on the Nano .44, 2026-10-24, as `usb` on the .41 a
/// day earlier, as the RTC watchdog from the web flasher). The second
/// kind is a watchdog like the RTC one: a crash unless a marker or the
/// first boot of an image says it was the tool's (`restart::crashed`).
pub fn reset_class() -> warpaxe_diag::restart::Reset {
    use warpaxe_diag::restart::Reset;
    let reason = unsafe { sys::esp_reset_reason() };
    let crash_reason = matches!(
        reason,
        sys::esp_reset_reason_t_ESP_RST_PANIC
            | sys::esp_reset_reason_t_ESP_RST_INT_WDT
            | sys::esp_reset_reason_t_ESP_RST_TASK_WDT
    );
    let software_cause = matches!(
        reset_cause(),
        sys::soc_reset_reason_t_RESET_REASON_CORE_SW | sys::soc_reset_reason_t_RESET_REASON_CPU0_SW
    );
    if crash_reason && software_cause {
        Reset::Crash
    } else if crash_reason || reason == sys::esp_reset_reason_t_ESP_RST_WDT {
        Reset::Watchdog
    } else {
        Reset::Clean
    }
}

/// Set at boot when an RTC-watchdog reset was the flashing tool's way
/// out of download mode, not a crash (`warpaxe_diag::restart::crashed`).
static WATCHDOG_FORGIVEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn forgive_watchdog() {
    WATCHDOG_FORGIVEN.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The last run crashed (or was reset by a watchdog), unless that
/// watchdog reset was a flashing tool's (`forgive_watchdog`, decided at
/// boot in main before anything reads this).
pub fn last_run_crashed() -> bool {
    use warpaxe_diag::restart::Reset;
    match reset_class() {
        Reset::Crash => true,
        Reset::Watchdog => !WATCHDOG_FORGIVEN.load(std::sync::atomic::Ordering::Relaxed),
        Reset::Clean => false,
    }
}

static LAST_CRASH: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// The stored core dump was written by this crash (this image, not
/// reported at an earlier boot). False until `check_core_dump` ran.
static CORE_DUMP_CURRENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The stored core dump's identity as one tag, `dump:<sha9>@<pc>`: the
/// first nine hex digits of the SHA-256 of the app that wrote it and the
/// crashed task's PC. `None` without a dump.
/// Reads the dump's summary from flash: on the main task only (a flash
/// read from a thread whose stack sits in PSRAM hangs it, docs/09).
pub fn core_dump_tag() -> Option<String> {
    // SAFETY: plain IDF calls with an out-struct; the summary is read
    // from the coredump partition.
    unsafe {
        if sys::esp_core_dump_image_check() != sys::ESP_OK {
            return None;
        }
        let mut sum: sys::esp_core_dump_summary_t = core::mem::zeroed();
        if sys::esp_core_dump_get_summary(&mut sum) != sys::ESP_OK {
            return None;
        }
        let end = sum
            .app_elf_sha256
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(sum.app_elf_sha256.len());
        let sha = String::from_utf8_lossy(&sum.app_elf_sha256[..end.min(9)]).into_owned();
        Some(format!("dump:{sha}@{:08x}", sum.exc_pc))
    }
}

/// The first nine hex digits of the running app's ELF SHA-256 (what a
/// core dump written by this image carries).
pub fn app_elf_sha() -> String {
    let mut buf = [0u8; 20];
    unsafe { sys::esp_app_get_elf_sha256(buf.as_mut_ptr() as *mut core::ffi::c_char, buf.len()) };
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end.min(9)]).into_owned()
}

/// Decide whether the stored core dump belongs to the crash the board
/// just came back from, and log it. A dump stays in flash until it is
/// erased or overwritten, and a crash that cannot write one (an
/// interrupt watchdog during a flash operation, for one) leaves the old
/// one in place: the Nano of 2026-10-21 came back from an `int-wdt`
/// with a dump written by an older test image. So a dump counts as this
/// crash's only when it was written by this image (ELF SHA) and no
/// earlier boot event already names it (`earlier_boots`: the boot
/// events' details, which carry the tag). Returns the tag and the
/// verdict; `None` without a dump.
pub fn check_core_dump(earlier_boots: &[String]) -> Option<(String, bool)> {
    let tag = core_dump_tag()?;
    let same_image = tag[5..].starts_with(&app_elf_sha());
    let seen = earlier_boots.iter().any(|d| d.contains(&tag));
    let current = last_run_crashed() && same_image && !seen;
    CORE_DUMP_CURRENT.store(current, std::sync::atomic::Ordering::Relaxed);
    if !last_run_crashed() {
        log::info!(
            "a core dump is stored ({tag}), from an earlier crash; this restart was no crash"
        );
    } else if !same_image {
        log::warn!(
            "the stored core dump ({tag}) was written by another image (this one is {}): not this crash, none was written for it",
            app_elf_sha()
        );
    } else if seen {
        log::warn!("the stored core dump ({tag}) was reported at an earlier boot: not this crash, none was written for it");
    }
    Some((tag, current))
}

/// The stored core dump belongs to the crash the board just came back from.
pub fn core_dump_current() -> bool {
    CORE_DUMP_CURRENT.load(std::sync::atomic::Ordering::Relaxed)
}

/// If the previous run crashed and the stored core dump is its own
/// (`check_core_dump`): log the dump's summary (task, cause, PC,
/// backtrace) into the ring and keep it for the API and the display.
/// The dump itself stays in flash for `/api/v1/coredump`, so after a clean
/// restart it is not reported again. Returns true if there was one.
pub fn log_core_dump() -> bool {
    if !last_run_crashed() || !core_dump_current() {
        return false;
    }
    let before = unsafe { tusb::wa_log_count() };
    let had = unsafe { tusb::wa_coredump_log() != 0 };
    if had {
        let after = unsafe { tusb::wa_log_count() };
        let mut lines = Vec::new();
        for i in before..after {
            let mut buf = [0u8; 56];
            unsafe { tusb::wa_log_line(i, buf.as_mut_ptr() as *mut core::ffi::c_char, buf.len()) };
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            lines.push(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
        *LAST_CRASH.lock().unwrap() = Some(lines.join(" | "));
    }
    had
}

/// Flash address and size of the stored core dump (ELF), if there is one.
pub fn core_dump() -> Option<(u32, u32)> {
    let (mut addr, mut size) = (0u32, 0u32);
    (unsafe { tusb::wa_coredump_image(&mut addr, &mut size) } != 0).then_some((addr, size))
}

/// Read part of the stored core dump from flash.
pub fn read_core_dump(addr: u32, buf: &mut [u8]) -> bool {
    unsafe { tusb::wa_coredump_read(addr, buf.as_mut_ptr().cast(), buf.len() as u32) != 0 }
}

pub fn erase_core_dump() {
    unsafe { tusb::wa_coredump_erase() };
}

/// The core dump partition's size, bytes: what a dump may be at most.
/// Read from the partition table in flash: on the main task at boot,
/// kept for the API.
static CORE_DUMP_ROOM: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

pub fn core_dump_room() -> u32 {
    *CORE_DUMP_ROOM.get_or_init(|| unsafe { tusb::wa_coredump_room() })
}

/// A core dump partition smaller than this leaves no room to spare:
/// this firmware's dumps run 60–64 KB (four of them measured on
/// 2026-10-23: 60,644 to 64,292 bytes, 18 to 24 tasks), and the writer
/// stores nothing at all when the dump would not fit (docs/12).
pub const CORE_DUMP_ROOM_WANTED: u32 = 128 * 1024;

/// What the crashing code said, if it could: a Rust panic's message
/// (`install_panic_hook`) or the task whose stack overflowed (the glue's
/// `vApplicationStackOverflowHook`), kept in RTC memory through the
/// reset. Set by `take_last_words` at boot, after a crash only.
static LAST_WORDS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Read and clear the last words of the previous run. On the main task
/// at boot, before anything else can crash. Kept only when the reset
/// was a crash: words left by an earlier crash would have been taken
/// at that boot already, so this guards against a reset in between
/// that kept RTC memory (a software reset) after words were written
/// but never taken.
pub fn take_last_words() {
    if unsafe { tusb::wa_last_words_fits() } == 0 {
        log::error!("last words: this image's RTC data reaches into the last-words block; none are kept");
        return;
    }
    let mut buf = [0u8; 200];
    let had = unsafe { tusb::wa_last_words_take(buf.as_mut_ptr() as *mut core::ffi::c_char, buf.len()) } != 0;
    if !had {
        return;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let words = String::from_utf8_lossy(&buf[..end]).into_owned();
    if last_run_crashed() {
        log::error!("last words of the crashed run: {words}");
        *LAST_WORDS.lock().unwrap() = Some(words);
    } else {
        log::warn!("last words found without a crash reset ({words}), ignored");
    }
}

/// The previous run's last words, if it crashed and could say any.
pub fn last_words() -> Option<String> {
    LAST_WORDS.lock().unwrap().clone()
}

/// A writer into a fixed buffer, for the panic hook: no allocation,
/// truncates at the end of the buffer.
struct Words {
    buf: [u8; 192],
    len: usize,
}

impl core::fmt::Write for Words {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = self.buf.len() - 1 - self.len;
        let n = s.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// The last two components of a source path, enough to find the line.
fn short_path(p: &str) -> &str {
    let mut cut = p.len();
    for _ in 0..2 {
        match p[..cut].rfind('/') {
            Some(i) => cut = i,
            None => return p,
        }
    }
    &p[cut + 1..]
}

/// Format and store a panic's words. Its own frame (the buffer and the
/// formatter) is gone before the default hook runs with its own, which
/// matters on the smallest stacks (the system event task has 2,304
/// bytes). No allocation: the task's name comes from FreeRTOS, which
/// every task has, where `std::thread::current` would allocate a handle
/// for a task std did not spawn (the HTTP server's, the event task's).
#[inline(never)]
fn store_panic_words(info: &std::panic::PanicHookInfo<'_>) {
    use core::fmt::Write;
    let mut w = Words {
        buf: [0u8; 192],
        len: 0,
    };
    // SAFETY: a null handle names the calling task; the name lives in
    // its TCB for as long as the task.
    let name = unsafe { core::ffi::CStr::from_ptr(sys::pcTaskGetName(core::ptr::null_mut())) };
    let _ = write!(w, "{} panicked", name.to_str().unwrap_or("?"));
    if let Some(l) = info.location() {
        let _ = write!(w, " at {}:{}", short_path(l.file()), l.line());
    }
    if let Some(m) = info.payload().downcast_ref::<&str>() {
        let _ = write!(w, ": {m}");
    } else if let Some(m) = info.payload().downcast_ref::<String>() {
        let _ = write!(w, ": {m}");
    }
    // SAFETY: the buffer is NUL-terminated (one byte kept) and lives for
    // the call; the glue copies it.
    unsafe { tusb::wa_last_words_set(w.buf.as_ptr() as *const core::ffi::c_char) };
}

/// Keep a Rust panic's message for the next boot (`take_last_words`)
/// before the default hook prints it and the firmware aborts
/// (`panic_abort`: the ESP-IDF panic handler, the core dump when it
/// fits, the reset with reason `panic`). The console is unreadable
/// after a boot's first 30 s (the USB PHY is on USB OTG, docs/12), and
/// the core dump is not always written, so this is what a user's
/// diagnostics file can still carry. Installed first thing at boot.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        store_panic_words(info);
        prev(info);
    }));
}

/// The previous run's crash summary, if there was one.
pub fn last_crash() -> Option<String> {
    LAST_CRASH.lock().unwrap().clone()
}

/// After a crash, USB OTG must not take the PHY immediately: give the
/// host this long to reflash over USB-Serial/JTAG. A working firmware
/// costs the user a few seconds after a crash; a crash loop never
/// brickes the board.
pub fn jtag_grace() -> Duration {
    if last_run_crashed() {
        Duration::from_secs(30)
    } else {
        Duration::ZERO
    }
}

/// Reboot into ROM download mode with the PHY handed back to
/// USB-Serial/JTAG (the host must clear `RTC_CNTL_OPTION1_REG` before the
/// reset that should start the app again).
pub fn reboot_to_bootloader() -> ! {
    unsafe { tusb::wa_reboot_to_bootloader() }
}

pub fn restart() -> ! {
    unsafe { sys::esp_restart() }
}

pub fn free_heap() -> u32 {
    unsafe { sys::esp_get_free_heap_size() }
}

/// The logger: ESP-IDF's, plus every line into the capture ring that
/// `/api/v1/log` and the web UI read (the Rust logger does not go through
/// esp_log's vprintf, so the ring would miss it otherwise).
struct RingLogger(esp_idf_svc::log::EspLogger);

impl log::Log for RingLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        self.0.enabled(m)
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        self.0.log(r);
        let lvl = match r.level() {
            log::Level::Error => 'E',
            log::Level::Warn => 'W',
            log::Level::Info => 'I',
            log::Level::Debug => 'D',
            log::Level::Trace => 'V',
        };
        let ms = unsafe { esp_idf_svc::sys::esp_log_timestamp() };
        let line = format!("{lvl} ({ms}) {}: {}", r.target(), r.args());
        if let Ok(c) = std::ffi::CString::new(line) {
            unsafe { tusb::wa_log_push(c.as_ptr()) };
        }
    }
    fn flush(&self) {}
}

static LOGGER: RingLogger = RingLogger(esp_idf_svc::log::EspLogger::new());

/// Install the logger (in place of `EspLogger::initialize_default`).
pub fn init_logger() {
    let _ = log::set_logger(&LOGGER);
    LOGGER.0.initialize();
}

/// Free internal (non-PSRAM) heap: task stacks, DMA and mbedTLS need it.
pub fn free_heap_internal() -> u32 {
    unsafe { sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL) as u32 }
}

/// The largest piece of internal heap in one go: what a task stack or a
/// DMA buffer can still get. The free total says nothing about it once
/// the heap is fragmented (45 KB free and no 16 KB piece, 2026-10-20).
pub fn largest_free_internal() -> u32 {
    unsafe { sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_INTERNAL) as u32 }
}

/// Internal heap in one line for the log, with its largest piece.
pub fn heap_line() -> String {
    format!(
        "{} KB internal free, largest piece {} KB",
        free_heap_internal() / 1024,
        largest_free_internal() / 1024
    )
}

/// How much of the calling task's stack was never touched so far, bytes
/// (FreeRTOS' high-water mark): logged after a flash write to see the
/// writer's real need.
pub fn stack_unused() -> u32 {
    unsafe { sys::uxTaskGetStackHighWaterMark(core::ptr::null_mut()) as u32 }
}

/// Raises the calling task's FreeRTOS priority until dropped, then puts
/// the old one back (on every path, the error path included).
pub struct PriorityBoost(u32);

impl PriorityBoost {
    pub fn raise(prio: u32) -> PriorityBoost {
        unsafe {
            let old = sys::uxTaskPriorityGet(core::ptr::null_mut());
            if prio > old {
                sys::vTaskPrioritySet(core::ptr::null_mut(), prio);
            }
            PriorityBoost(old)
        }
    }
}

impl Drop for PriorityBoost {
    fn drop(&mut self) {
        unsafe { sys::vTaskPrioritySet(core::ptr::null_mut(), self.0) }
    }
}

/// Spawn a thread whose stack lives in PSRAM, for threads that neither
/// write flash nor run with the cache off (display, fetches, sampling):
/// the internal RAM stays free for the OTA writer and TLS.
pub fn spawn_psram<F>(
    name: &str,
    stack: usize,
    f: F,
) -> std::io::Result<std::thread::JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    // The FreeRTOS task gets the name too (Builder's name stays in Rust),
    // so a panic or core dump says "stratum" rather than "pthread".
    // FreeRTOS copies it at creation; the CString outlives the spawn.
    let task_name = std::ffi::CString::new(name).unwrap_or_default();
    unsafe {
        let mut cfg = sys::esp_pthread_get_default_config();
        cfg.stack_size = stack;
        cfg.stack_alloc_caps = sys::MALLOC_CAP_8BIT | sys::MALLOC_CAP_SPIRAM;
        cfg.thread_name = task_name.as_ptr();
        sys::esp_pthread_set_cfg(&cfg);
    }
    let r = std::thread::Builder::new()
        .name(name.into())
        .stack_size(stack)
        .spawn(f);
    unsafe {
        let d = sys::esp_pthread_get_default_config();
        sys::esp_pthread_set_cfg(&d);
    }
    r
}

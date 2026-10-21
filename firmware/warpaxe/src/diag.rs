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

/// The chip came up from a power-on (the supply or the EN pin), so the
/// board's rails were really down. A software, watchdog, USB or JTAG
/// reset kept the 12 V input and everything fed from it.
pub fn reset_was_power_on() -> bool {
    unsafe { sys::esp_reset_reason() == sys::esp_reset_reason_t_ESP_RST_POWERON }
}

/// The last run crashed (or was reset by a watchdog).
pub fn last_run_crashed() -> bool {
    matches!(
        unsafe { sys::esp_reset_reason() },
        sys::esp_reset_reason_t_ESP_RST_PANIC
            | sys::esp_reset_reason_t_ESP_RST_INT_WDT
            | sys::esp_reset_reason_t_ESP_RST_TASK_WDT
            | sys::esp_reset_reason_t_ESP_RST_WDT
    )
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
    unsafe {
        let mut cfg = sys::esp_pthread_get_default_config();
        cfg.stack_size = stack;
        cfg.stack_alloc_caps = sys::MALLOC_CAP_8BIT | sys::MALLOC_CAP_SPIRAM;
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

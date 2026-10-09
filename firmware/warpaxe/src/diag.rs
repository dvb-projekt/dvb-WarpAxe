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

/// If the previous run crashed: log its core-dump summary (task, cause,
/// PC, backtrace) into the ring and keep it for the API and the display.
/// The dump itself stays in flash for `/api/v1/coredump`, so after a clean
/// restart it is not reported again. Returns true if there was one.
pub fn log_core_dump() -> bool {
    if !last_run_crashed() {
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

// The USB side of the proof as a handful of plain C calls. TinyUSB's own
// headers cannot go through bindgen (packed structs with bit-field
// unions), so this header is what Rust sees; warpaxe_glue.c is the only
// file that includes tinyusb.h.
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

/// Called from TinyUSB's task with a received Ethernet frame. The buffer
/// is valid only for the duration of the call.
typedef esp_err_t (*wa_usb_rx_cb_t)(void *buffer, uint16_t len, void *ctx);

/// Install TinyUSB with the Kconfig descriptors (CDC-NCM + CDC-ACM), move
/// the console to the CDC-ACM port, and start the NCM link. `host_mac` is
/// the MAC the host's adapter will report.
esp_err_t wa_usb_start(const uint8_t host_mac[6], wa_usb_rx_cb_t on_rx, void *ctx);

/// Send one Ethernet frame to the host, blocking up to 100 ms.
esp_err_t wa_usb_send(void *buffer, uint16_t len);

/// The T-Display-S3 without chips (warpaxe_cpuhash.c): double SHA-256
/// of a block header from its midstate for the nonces `start..start+n`.
/// Returns the index of the first nonce whose hash has its two most
/// significant bytes zero, with the hash in `hash_out` (natural byte
/// order), or `n` when none has.
uint32_t wa_sha256d_scan(const uint32_t mid[8], const uint8_t tail[12], uint32_t start,
                         uint32_t n, uint8_t hash_out[32]);

/// The first hash's state after its second block for `nonce`, as the
/// eight words the S3's SHA peripheral holds in H (memory order).
void wa_sha256_block1_h(const uint32_t mid[8], const uint8_t tail[12], uint32_t nonce,
                        uint32_t h_out[8]);

/// The double hash's last word for `nonce` as the peripheral's H[7]
/// reads it (memory order): what the workers test for a candidate.
uint32_t wa_sha256d_h7(const uint32_t mid[8], const uint8_t tail[12], uint32_t nonce);

/// The SHA peripheral measured (warpaxe_shabench.c): cycles are CPU
/// cycles at `cpu_mhz`; 0xffffffff means "not found" or "not run".
typedef struct {
    uint32_t cpu_mhz;
    /// T: register access cost, best of eight, interrupts off.
    uint32_t t_nothing;            // two CCOUNT reads back to back
    uint32_t t_store16_text;       // sixteen stores to TEXT
    uint32_t t_store16_text_memw;  // the same with a MEMW after each
    uint32_t t_store8_h;           // eight stores to H
    uint32_t t_load8_h;            // eight loads from H
    uint32_t t_load_busy;          // one load of BUSY
    uint32_t t_store_then_load;    // a store to TEXT, then a load of BUSY
    uint32_t t_store16_ram;        // sixteen stores to DRAM, for scale
    /// A: busy window after a trigger.
    uint32_t a_continue_poll;      // CONTINUE to the first idle poll
    uint32_t a_start_poll;         // START to the first idle poll
    uint32_t a_continue_delay;     // smallest counted delay, then idle in one read
    uint32_t a_start_delay;
    uint32_t a_drained_poll;       // CONTINUE after a MEMW (no store backlog) to idle
    uint32_t a_drained_delay;      // the same as the smallest counted delay
    /// B: TEXT sampling: delay after CONTINUE from which TEXT[k] may
    /// change without touching the digest, for k = 0, 3, 8, 15.
    uint32_t b_text_safe[4];
    uint32_t b_text_kept;          // 1: a block leaves TEXT as written
    /// D: the DMA path.
    uint32_t d_dma_ok;             // 1: a GDMA channel was had
    uint32_t d_dma1_poll;          // DMA_START over one block to idle
    uint32_t d_dma2_poll;          // over two blocks
    uint32_t d_dma_right;          // 1: the two-block hash equals the midstate path
    uint32_t d_dma_text;           // 1: a DMA hash leaves its block in TEXT
    /// V: loop variants over 2048 nonces: cycles per nonce and nonces
    /// whose H[7] differed from the software hash. 0 = shipped loop,
    /// 1 = the next block's TEXT written while the engine runs,
    /// 2 = 1 with the digest read in one go, 3 = 1 with a counted wait
    /// before the first BUSY read, 4 = 2 and 3 together.
    uint32_t v_counted_delay;      // the counted wait of 3 and 4, cycles
    uint32_t v_cycles[5];
    uint32_t v_bad[5];
} wa_sha_bench_t;

/// Run the bench on the calling core, under the crypto lock. `head` is
/// the header's first 64 bytes as little-endian words. Returns 0 when
/// no memory was had.
int wa_sha_bench(const uint32_t mid[8], const uint8_t tail[12], const uint32_t head[16],
                 wa_sha_bench_t *out);

/// Route stdout (and so ESP_LOG) to the CDC-ACM port. Non-blocking writes.
esp_err_t wa_usb_console_to_cdc(void);

/// Enumerated and configured by a host.
bool wa_usb_mounted(void);

/// The host has activated the NCM data interface (TinyUSB's network init
/// callback fired). Sending before that is not allowed.
bool wa_usb_net_ready(void);

/// The CPU clock in MHz as the ROM counts it.
int wa_cpu_freq_mhz(void);

/// Switch the CPU clock (80, 160 or 240 MHz; sdkconfig starts at 160).
/// Returns the clock afterwards, or -1 for a value the chip has not got.
int wa_cpu_freq_set(int mhz);

/// Restart into the ROM download mode (USB-Serial/JTAG comes back).
void wa_reboot_to_bootloader(void) __attribute__((noreturn));

/// Keep the last few ESP_LOG lines in memory so the display can show them
/// — the only console this board has once USB OTG owns the PHY.
void wa_log_capture_start(void);
int wa_log_count(void);
/// Copy line `i` (0 = oldest kept) into `buf`, NUL-terminated.
void wa_log_line(int i, char *buf, size_t n);
void wa_log_push(const char *line);

/// TinyUSB's debug printf, filtered to NCM/error lines, into the same ring.
int wa_tu_printf(const char *fmt, ...);

/// Log the stored core dump's summary (task, cause, PC, backtrace)
/// through ESP_LOGE so it lands in the ring. The dump stays in flash.
/// Returns 1 if there was one.
int wa_coredump_log(void);

/// Flash address and size of the stored core dump (ELF); 0 if none.
int wa_coredump_image(uint32_t *addr, uint32_t *size);

/// Read n bytes of flash at addr; 1 on success.
int wa_coredump_read(uint32_t addr, void *buf, uint32_t n);

/// Erase the stored core dump.
void wa_coredump_erase(void);

#ifdef __cplusplus
}
#endif

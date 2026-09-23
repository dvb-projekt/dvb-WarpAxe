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

/// Enumerated and configured by a host.
bool wa_usb_mounted(void);

/// Restart into the ROM download mode (USB-Serial/JTAG comes back).
void wa_reboot_to_bootloader(void) __attribute__((noreturn));

/// Keep the last few ESP_LOG lines in memory so the display can show them
/// — the only console this board has once USB OTG owns the PHY.
void wa_log_capture_start(void);
int wa_log_count(void);
/// Copy line `i` (0 = oldest kept) into `buf`, NUL-terminated.
void wa_log_line(int i, char *buf, size_t n);

/// TinyUSB's debug printf, filtered to NCM/error lines, into the same ring.
int wa_tu_printf(const char *fmt, ...);

#ifdef __cplusplus
}
#endif

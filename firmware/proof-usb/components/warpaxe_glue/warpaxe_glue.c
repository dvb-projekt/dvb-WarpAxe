#include "warpaxe_glue.h"

#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "esp_system.h"
#include "soc/rtc_cntl_reg.h"
#include "tinyusb.h"
#include "tinyusb_net.h"
#include "tusb_cdc_acm.h"
#include "tusb_console.h"

static const char *TAG = "wa_usb";

static void free_tx(void *buffer, void *ctx)
{
    (void)buffer;
    (void)ctx;
    // Synchronous sends: nothing of ours to release.
}

// A 'B' on the console port reboots into the ROM download mode: the
// flash-back path without a finger on BOOT and without an IP address.
static void cdc_rx(int itf, cdcacm_event_t *event)
{
    uint8_t buf[64];
    size_t n = 0;
    if (tinyusb_cdcacm_read(itf, buf, sizeof buf, &n) == ESP_OK) {
        for (size_t i = 0; i < n; i++) {
            if (buf[i] == 'B') {
                wa_reboot_to_bootloader();
            }
        }
    }
}

esp_err_t wa_usb_start(const uint8_t host_mac[6], wa_usb_rx_cb_t on_rx, void *ctx)
{
    // NULL descriptors: esp_tinyusb builds them from Kconfig
    // (TINYUSB_NET_MODE_NCM + TINYUSB_CDC_ENABLED).
    const tinyusb_config_t usb_cfg = {0};
    esp_err_t err = tinyusb_driver_install(&usb_cfg);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "tinyusb_driver_install: %s", esp_err_to_name(err));
        return err;
    }

    const tinyusb_config_cdcacm_t acm_cfg = {
        .usb_dev = TINYUSB_USBDEV_0,
        .cdc_port = TINYUSB_CDC_ACM_0,
        .rx_unread_buf_sz = 64,
        .callback_rx = cdc_rx,
    };
    err = tusb_cdc_acm_init(&acm_cfg);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "tusb_cdc_acm_init: %s", esp_err_to_name(err));
        return err;
    }
    // Console stays where it is (USB-Serial/JTAG, silent once OTG owns the
    // PHY); the display shows the log tail. Redirecting stdout into the CDC
    // port is a suspect for stalls while no host reads it.

    tinyusb_net_config_t net_cfg = {
        .on_recv_callback = on_rx,
        .free_tx_buffer = free_tx,
        .user_context = ctx,
    };
    for (int i = 0; i < 6; i++) {
        net_cfg.mac_addr[i] = host_mac[i];
    }
    err = tinyusb_net_init(TINYUSB_USBDEV_0, &net_cfg);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "tinyusb_net_init: %s", esp_err_to_name(err));
        return err;
    }
    ESP_LOGI(TAG, "USB up: NCM + CDC-ACM console");
    return ESP_OK;
}

esp_err_t wa_usb_send(void *buffer, uint16_t len)
{
    return tinyusb_net_send_sync(buffer, len, NULL, pdMS_TO_TICKS(100));
}

bool wa_usb_mounted(void)
{
    return tud_mounted();
}

void wa_reboot_to_bootloader(void)
{
    // The ROM honours this on the next reset and enters download mode on
    // USB-Serial/JTAG — the same thing holding BOOT during reset does.
    REG_WRITE(RTC_CNTL_OPTION1_REG, RTC_CNTL_FORCE_DOWNLOAD_BOOT);
    esp_restart();
    while (1) {
    }
}

#define WA_LOG_LINES 8
#define WA_LOG_COLS 56
static char s_lines[WA_LOG_LINES][WA_LOG_COLS];
static int s_head = 0;
static int s_count = 0;
static vprintf_like_t s_prev = NULL;
static portMUX_TYPE s_mux = portMUX_INITIALIZER_UNLOCKED;

static void wa_push_line(const char *buf, int n)
{
    {
        char line[WA_LOG_COLS];
        int o = 0;
        for (int i = 0; i < n && buf[i] && o < WA_LOG_COLS - 1; i++) {
            char c = buf[i];
            if (c == '\033') { // ANSI colour sequence
                while (i < n && buf[i] && buf[i] != 'm') i++;
                continue;
            }
            if (c == '\n' || c == '\r') continue;
            line[o++] = c;
        }
        line[o] = 0;
        if (o > 0) {
            portENTER_CRITICAL_SAFE(&s_mux);
            memcpy(s_lines[s_head], line, sizeof line);
            s_head = (s_head + 1) % WA_LOG_LINES;
            if (s_count < WA_LOG_LINES) s_count++;
            portEXIT_CRITICAL_SAFE(&s_mux);
        }
    }
}

static int wa_vprintf(const char *fmt, va_list ap)
{
    char buf[192];
    va_list ap2;
    va_copy(ap2, ap);
    int n = vsnprintf(buf, sizeof buf, fmt, ap2);
    va_end(ap2);
    wa_push_line(buf, n);
    return s_prev ? s_prev(fmt, ap) : vprintf(fmt, ap);
}

int wa_tu_printf(const char *fmt, ...)
{
    char buf[160];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    // Only what helps: NCM driver lines and anything TinyUSB marks as error.
    if (n > 0 && (strstr(buf, "EE") || strstr(buf, "ncm") || strstr(buf, "NCM") || strstr(buf, "recv") || strstr(buf, "xfer_out") || strstr(buf, "ndp") || strstr(buf, "NTB"))) {
        wa_push_line(buf, n);
    }
    return n;
}

void wa_log_capture_start(void)
{
    s_prev = esp_log_set_vprintf(wa_vprintf);
}

int wa_log_count(void)
{
    return s_count;
}

void wa_log_line(int i, char *buf, size_t n)
{
    if (n == 0) return;
    portENTER_CRITICAL_SAFE(&s_mux);
    int idx = ((s_head - s_count + i) % WA_LOG_LINES + WA_LOG_LINES) % WA_LOG_LINES;
    strncpy(buf, s_lines[idx], n - 1);
    portEXIT_CRITICAL_SAFE(&s_mux);
    buf[n - 1] = 0;
}

#include "warpaxe_glue.h"

#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include "esp_log.h"
#include "esp_core_dump.h"
#include "esp_flash.h"
#include <stdlib.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_system.h"
#include "soc/rtc_cntl_reg.h"
#include "esp_mac.h"
#include "sdkconfig.h"
#include "soc/rtc.h"
#include "esp_private/esp_clk.h"
#include "tinyusb.h"
#include "tinyusb_net.h"
#include "tusb_cdc_acm.h"
#include "tusb_console.h"

static const char *TAG = "wa_usb";

static void free_tx(void *buffer, void *ctx)
{
    (void)ctx;
    // Asynchronous sends: `buffer` is the copy wa_usb_send() made.
    free(buffer);
}

// A 'B' on the console port reboots into the ROM download mode: the
// flash-back path without a finger on BOOT and without an IP address.
static void bootloader_task(void *arg)
{
    (void)arg;
    vTaskDelay(pdMS_TO_TICKS(100));
    wa_reboot_to_bootloader();
}

static void cdc_rx(int itf, cdcacm_event_t *event)
{
    (void)event;
    uint8_t buf[64];
    size_t n = 0;
    if (tinyusb_cdcacm_read(itf, buf, sizeof buf, &n) == ESP_OK) {
        for (size_t i = 0; i < n; i++) {
            if (buf[i] == 'B') {
                // This callback runs on the TinyUSB task; the reboot path
                // uninstalls TinyUSB, so it must run elsewhere.
                xTaskCreate(bootloader_task, "wa_boot", 4096, NULL, 5, NULL);
                return;
            }
        }
    }
}

static volatile bool s_net_ready = false;

static void net_init_cb(void *ctx)
{
    (void)ctx;
    s_net_ready = true;
    ESP_LOGI(TAG, "NCM data interface up");
}

bool wa_usb_net_ready(void)
{
    return s_net_ready;
}

esp_err_t wa_usb_start(const uint8_t host_mac[6], wa_usb_rx_cb_t on_rx, void *ctx)
{
    // Device and configuration descriptors: esp_tinyusb builds them from
    // Kconfig (TINYUSB_NET_MODE_NCM + TINYUSB_CDC_ENABLED). The string
    // descriptors are its default list with one change: the serial is the
    // base MAC (as the ROM's USB-Serial/JTAG reports it), not Kconfig's
    // "123456" that every WarpAxe would share — two of them on one host
    // get distinct port names then. The order and count must match
    // esp_tinyusb's descriptor_str_default (usb_descriptors.c).
    static const char s_lang[] = {0x09, 0x04};
    static char s_serial[13];
    static const char *s_strings[7];
    uint8_t mac[6] = {0};
    esp_read_mac(mac, ESP_MAC_WIFI_STA);
    snprintf(s_serial, sizeof s_serial, "%02X%02X%02X%02X%02X%02X",
             mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    s_strings[0] = s_lang;
    s_strings[1] = CONFIG_TINYUSB_DESC_MANUFACTURER_STRING;
    s_strings[2] = CONFIG_TINYUSB_DESC_PRODUCT_STRING;
    s_strings[3] = s_serial;
    s_strings[4] = CONFIG_TINYUSB_DESC_CDC_STRING;
    s_strings[5] = "USB net";
    s_strings[6] = "";  // MAC string, filled by the net driver
    tinyusb_config_t usb_cfg = {0};
    usb_cfg.string_descriptor = s_strings;
    usb_cfg.string_descriptor_count = 7;
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
        .on_init_callback = net_init_cb,
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
    if (!tud_mounted()) {
        return ESP_ERR_INVALID_STATE; // no host: drop, never touch the driver
    }
    // Asynchronous: the caller (lwIP's tcpip thread) must never wait for
    // the TinyUSB task. The copy lives until TinyUSB has taken it into an
    // NTB (free_tx) or dropped it because no NTB was free.
    void *copy = malloc(len);
    if (!copy) {
        return ESP_ERR_NO_MEM;
    }
    memcpy(copy, buffer, len);
    esp_err_t err = tinyusb_net_send_async(copy, len, copy);
    if (err != ESP_OK) {
        free(copy);
    }
    return err;
}

bool wa_usb_mounted(void)
{
    return tud_mounted();
}

int wa_cpu_freq_mhz(void)
{
    return esp_clk_cpu_freq() / 1000000;
}

int wa_cpu_freq_set(int mhz)
{
    rtc_cpu_freq_config_t cfg;
    if (!rtc_clk_cpu_freq_mhz_to_config((uint32_t)mhz, &cfg)) {
        return -1;
    }
    // 80, 160 and 240 MHz all come from the 480 MHz PLL on the ESP32-S3 and
    // APB stays at 80 MHz, so only the CPU divider changes: UART, LCD and
    // timers keep their clocks. The ROM's ticks-per-us follow inside.
    rtc_clk_cpu_freq_set_config(&cfg);
    return wa_cpu_freq_mhz();
}

void wa_reboot_to_bootloader(void)
{
    // Hand the one USB PHY back to USB-Serial/JTAG: the driver's own
    // teardown does that properly (usb_del_phy). The PHY selection lives in
    // the RTC domain and survives a software reset, so without this the
    // ROM's download mode would sit on a PHY nobody is listening to.
    tinyusb_driver_uninstall();
    REG_SET_BIT(RTC_CNTL_USB_CONF_REG, RTC_CNTL_SW_HW_USB_PHY_SEL);
    REG_CLR_BIT(RTC_CNTL_USB_CONF_REG, RTC_CNTL_SW_USB_PHY_SEL);
    vTaskDelay(pdMS_TO_TICKS(50));
    // The ROM honours this on the next reset and enters download mode on
    // USB-Serial/JTAG — the same thing holding BOOT during reset does.
    REG_WRITE(RTC_CNTL_OPTION1_REG, RTC_CNTL_FORCE_DOWNLOAD_BOOT);
    esp_restart();
    while (1) {
    }
}

#define WA_LOG_LINES 128
#define WA_LOG_COLS 120
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

/* Some components (the Wi-Fi blob) print one line in several calls:
 * collect the pieces until a newline, then store the line once. */
static char s_pending[WA_LOG_COLS];
static int s_pending_len = 0;

static int wa_vprintf(const char *fmt, va_list ap)
{
    char buf[192];
    va_list ap2;
    va_copy(ap2, ap);
    int n = vsnprintf(buf, sizeof buf, fmt, ap2);
    va_end(ap2);
    if (n > 0 && !xPortInIsrContext()) {
        if (n > (int) sizeof buf - 1) n = sizeof buf - 1;
        portENTER_CRITICAL_SAFE(&s_mux);
        int room = WA_LOG_COLS - 1 - s_pending_len;
        int take = n < room ? n : room;
        memcpy(s_pending + s_pending_len, buf, take);
        s_pending_len += take;
        s_pending[s_pending_len] = 0;
        bool done = buf[n - 1] == '\n' || s_pending_len >= WA_LOG_COLS - 1;
        char line[WA_LOG_COLS];
        int len = 0;
        if (done) {
            memcpy(line, s_pending, s_pending_len + 1);
            len = s_pending_len;
            s_pending_len = 0;
        }
        portEXIT_CRITICAL_SAFE(&s_mux);
        if (done) wa_push_line(line, len);
    }
    return s_prev ? s_prev(fmt, ap) : vprintf(fmt, ap);
}

/* A whole line from the Rust logger, which bypasses esp_log's vprintf. */
void wa_log_push(const char *line)
{
    wa_push_line(line, strlen(line));
}

int wa_tu_printf(const char *fmt, ...)
{
    // Never format from interrupt context: the ISR stack is small and
    // TinyUSB's DCD logs from its interrupt handler.
    if (xPortInIsrContext()) {
        return 0;
    }
    char buf[160];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    // Only what helps: NCM driver lines and anything TinyUSB marks as error.
    if (n > 0 && (strstr(buf, "EE") || strstr(buf, "ASSERT") || strstr(buf, "assert") || strstr(buf, "dcd") || strstr(buf, "fifo") || strstr(buf, "FIFO") || strstr(buf, "ncm") || strstr(buf, "NCM") || strstr(buf, "recv") || strstr(buf, "xfer_out") || strstr(buf, "ndp") || strstr(buf, "NTB"))) {
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

int wa_coredump_log(void)
{
    if (esp_core_dump_image_check() != ESP_OK) {
        ESP_LOGI(TAG, "no core dump from a previous run");
        return 0;
    }
    esp_core_dump_summary_t *sum = calloc(1, sizeof *sum);
    if (!sum) return 0;
    if (esp_core_dump_get_summary(sum) != ESP_OK) {
        ESP_LOGE(TAG, "core dump present but summary failed");
        free(sum);
        return 0;
    }
    // Short enough for a ring line: the PC must not be cut.
    ESP_LOGE(TAG, "CRASH %s cause %lu pc %08lx", sum->exc_task, (unsigned long) sum->ex_info.exc_cause, (unsigned long) sum->exc_pc);
    ESP_LOGE(TAG, "CRASH vaddr=%08lx depth=%lu%s", (unsigned long) sum->ex_info.exc_vaddr, (unsigned long) sum->exc_bt_info.depth, sum->exc_bt_info.corrupted ? " CORRUPT" : "");
    for (uint32_t i = 0; i < sum->exc_bt_info.depth && i < 16; i += 3) {
        ESP_LOGE(TAG, "BT%lu %08lx %08lx %08lx", (unsigned long) i,
                 (unsigned long) sum->exc_bt_info.bt[i],
                 (unsigned long) (i + 1 < sum->exc_bt_info.depth ? sum->exc_bt_info.bt[i + 1] : 0),
                 (unsigned long) (i + 2 < sum->exc_bt_info.depth ? sum->exc_bt_info.bt[i + 2] : 0));
    }
    free(sum);
    // The dump stays in flash for GET /api/v1/coredump until it is erased
    // there or a new crash overwrites it.
    return 1;
}

int wa_coredump_image(uint32_t *addr, uint32_t *size)
{
    size_t a = 0, n = 0;
    if (esp_core_dump_image_check() != ESP_OK || esp_core_dump_image_get(&a, &n) != ESP_OK) {
        return 0;
    }
    *addr = (uint32_t) a;
    *size = (uint32_t) n;
    return 1;
}

int wa_coredump_read(uint32_t addr, void *buf, uint32_t n)
{
    return esp_flash_read(NULL, buf, addr, n) == ESP_OK;
}

void wa_coredump_erase(void)
{
    esp_core_dump_image_erase();
}

esp_err_t wa_usb_console_to_cdc(void)
{
    return esp_tusb_init_console(TINYUSB_CDC_ACM_0);
}

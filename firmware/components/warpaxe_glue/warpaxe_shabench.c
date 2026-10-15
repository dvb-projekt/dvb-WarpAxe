// The SHA peripheral measured from the inside, for the chipless
// T-Display-S3 (src/cpuminer.rs, hw_batch) and, standalone, for any
// ESP32-S3 or ESP32-P4 (tools/sha-bench): where the cycles of a nonce go.
//
// Everything runs under the mbedTLS crypto lock on the calling core (in
// the firmware: the peripheral worker's core at a priority above the
// workers). The micro-probes (T, A, B, D) run with interrupts off for a
// few microseconds each; the loop variants (V) run with interrupts on,
// 2048 nonces each, and every nonce's last digest word is checked against
// the software hash afterwards, so a variant that corrupts a hash shows
// up as `v_bad`, never as a hashrate.
//
// Register access as in warpaxe_sha_regs.h (Xtensa or RISC-V); the MEMW
// (FENCE) variant is measured separately (t_store16_text_memw). The DMA
// probe takes the bus the chip's SHA hangs on: the AHB GDMA on the S3,
// the AXI DMA on the P4, where the descriptor and the data also have to
// be written back from the L1 cache before the DMA can see them (that
// write-back is part of what the probe measures: the DMA path pays it).
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include "sdkconfig.h"
#include "esp_attr.h"
#include "esp_cpu.h"
#include "esp_heap_caps.h"
#include "esp_private/esp_clk.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "soc/soc_caps.h"
#include "soc/hwcrypto_reg.h"
#include "soc/gdma_channel.h"
#include "hal/dma_types.h"
#include "esp_private/gdma.h"
// (The S3 does not define SOC_GDMA_BUS_AXI at all: an undefined name is 0
// to the preprocessor, the same as the AHB bus, hence the defined().)
#if defined(SOC_GDMA_BUS_AXI) && SOC_GDMA_TRIG_PERIPH_SHA0_BUS == SOC_GDMA_BUS_AXI
#include "soc/axi_dma_struct.h"
#include "hal/axi_dma_ll.h"
typedef dma_descriptor_align8_t bench_desc_t;
#define dma_new_channel gdma_new_axi_channel
#define dma_ll_tx_set_desc_addr(ch, addr) axi_dma_ll_tx_set_desc_addr(&AXI_DMA, (ch), (addr))
#define dma_ll_tx_start(ch) axi_dma_ll_tx_start(&AXI_DMA, (ch))
#else
#include "soc/gdma_struct.h"
#include "hal/gdma_ll.h"
typedef dma_descriptor_t bench_desc_t;
#define dma_new_channel gdma_new_ahb_channel
#define dma_ll_tx_set_desc_addr(ch, addr) gdma_ll_tx_set_desc_addr(&GDMA, (ch), (addr))
#define dma_ll_tx_start(ch) gdma_ll_tx_start(&GDMA, (ch))
#endif
#if SOC_CACHE_INTERNAL_MEM_VIA_L1CACHE
#include "esp_cache.h"
#endif
#include "warpaxe_sha.h"
#include "warpaxe_sha_regs.h"

// mbedTLS port (sha/sha_dma.h): the crypto lock plus the block's clock.
void esp_sha_acquire_hardware(void);
void esp_sha_release_hardware(void);

#define SHA2_256 WA_SHA2_256
#define SHA_H(k) WA_SHA_H(k)
#define SHA_TEXT(k) WA_SHA_TEXT(k)
#define HW_PAD WA_HW_PAD
#define HW_LEN_640 WA_HW_LEN_640
#define HW_PAD2 WA_HW_PAD2
#define HW_LEN_256 WA_HW_LEN_256
#define reg_w wa_reg_w
#define reg_r wa_reg_r
#define memw wa_memw
#define ccount wa_ccount
#define wait_idle wa_sha_wait_idle
#define write_h wa_sha_write_h
#define write_block1 wa_sha_write_block1
#define zero_text_9_14 wa_sha_zero_text_9_14

#define N_VAR 5
#define N_NONCE 2048

// Spin until `cycles` CPU cycles have passed since `t0`.
static inline __attribute__((always_inline)) void spin_until(uint32_t t0, uint32_t cycles)
{
    while (ccount() - t0 < cycles) {
    }
}

// Internal memory the DMA reads: written back from the cache where
// there is one in front of it (the P4), nothing on the S3.
static inline void dma_writeback(void *p, size_t n)
{
#if SOC_CACHE_INTERNAL_MEM_VIA_L1CACHE
    esp_cache_msync(p, n, ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED);
#else
    (void)p;
    (void)n;
#endif
}

// ---- T: what a register access costs ---------------------------------

static IRAM_ATTR uint32_t t_store16_text(void)
{
    uint32_t t0 = ccount();
    for (int k = 0; k < 16; k++) reg_w(SHA_TEXT(k), 0);
    return ccount() - t0;
}

static IRAM_ATTR uint32_t t_store16_text_memw(void)
{
    uint32_t t0 = ccount();
    for (int k = 0; k < 16; k++) {
        reg_w(SHA_TEXT(k), 0);
        memw();
    }
    return ccount() - t0;
}

static IRAM_ATTR uint32_t t_store8_h(const uint32_t hs[8])
{
    uint32_t t0 = ccount();
    write_h(hs);
    return ccount() - t0;
}

static IRAM_ATTR uint32_t t_load8_h(uint32_t out[8])
{
    uint32_t t0 = ccount();
    for (int k = 0; k < 8; k++) out[k] = reg_r(SHA_H(k));
    return ccount() - t0;
}

static IRAM_ATTR uint32_t t_load_busy(void)
{
    uint32_t t0 = ccount();
    (void)reg_r(SHA_BUSY_REG);
    return ccount() - t0;
}

static IRAM_ATTR uint32_t t_store_then_load(void)
{
    uint32_t t0 = ccount();
    reg_w(SHA_TEXT(9), 0);
    (void)reg_r(SHA_BUSY_REG);
    return ccount() - t0;
}

static IRAM_ATTR uint32_t t_store16_ram(uint32_t *ram)
{
    uint32_t t0 = ccount();
    for (int k = 0; k < 16; k++) reg_w((uint32_t)&ram[k], 0);
    return ccount() - t0;
}

static IRAM_ATTR uint32_t t_nothing(void)
{
    uint32_t t0 = ccount();
    return ccount() - t0;
}

// ---- A: how long the engine is busy -----------------------------------

// CONTINUE (or START) then poll BUSY: cycles until the first idle read.
static IRAM_ATTR uint32_t a_poll(const uint32_t hs[8], const uint32_t tw[3], uint32_t trigger)
{
    write_h(hs);
    write_block1(tw, 0);
    uint32_t t0 = ccount();
    reg_w(trigger, 1);
    wait_idle();
    return ccount() - t0;
}

// CONTINUE (or START), a counted delay, one BUSY read: the smallest delay
// that finds the engine idle three times running (0xffffffff: never).
static IRAM_ATTR uint32_t a_delay(const uint32_t hs[8], const uint32_t tw[3], uint32_t trigger)
{
    int hits = 0;
    for (uint32_t d = 0; d < 1200; d += 2) {
        write_h(hs);
        write_block1(tw, 0);
        uint32_t t0 = ccount();
        reg_w(trigger, 1);
        spin_until(t0, d);
        uint32_t busy = reg_r(SHA_BUSY_REG);
        wait_idle();
        if (!busy) {
            if (++hits == 3) return d - 4;
        } else {
            hits = 0;
        }
    }
    return 0xffffffffu;
}

// CONTINUE after the bus has drained (MEMW), then poll: the engine's own
// time plus one trigger and one read, without the backlog of the stores.
static IRAM_ATTR uint32_t a_drained_poll(const uint32_t hs[8], const uint32_t tw[3])
{
    write_h(hs);
    write_block1(tw, 0);
    memw();
    uint32_t t0 = ccount();
    reg_w(SHA_CONTINUE_REG, 1);
    wait_idle();
    return ccount() - t0;
}

// The same as the smallest counted delay after which one read finds idle.
static IRAM_ATTR uint32_t a_drained_delay(const uint32_t hs[8], const uint32_t tw[3])
{
    int hits = 0;
    for (uint32_t d = 0; d < 1200; d += 2) {
        write_h(hs);
        write_block1(tw, 0);
        memw();
        uint32_t t0 = ccount();
        reg_w(SHA_CONTINUE_REG, 1);
        spin_until(t0, d);
        uint32_t busy = reg_r(SHA_BUSY_REG);
        wait_idle();
        if (!busy) {
            if (++hits == 3) return d - 4;
        } else {
            hits = 0;
        }
    }
    return 0xffffffffu;
}

// ---- B: when the engine samples TEXT -----------------------------------

// CONTINUE, a counted delay, then TEXT[k] overwritten: the smallest delay
// after which the digest still comes out right (three times running).
// The expected digest is the first hash's state for nonce 0, in H order.
static IRAM_ATTR uint32_t b_text_safe(const uint32_t hs[8], const uint32_t tw[3], const uint32_t want[8],
                                      int k)
{
    int hits = 0;
    for (uint32_t d = 0; d < 400; d += 2) {
        write_h(hs);
        write_block1(tw, 0);
        uint32_t t0 = ccount();
        reg_w(SHA_CONTINUE_REG, 1);
        spin_until(t0, d);
        reg_w(SHA_TEXT(k), 0xdeadbeefu);
        wait_idle();
        int ok = 1;
        for (int j = 0; j < 8; j++) {
            if (reg_r(SHA_H(j)) != want[j]) ok = 0;
        }
        if (ok) {
            if (++hits == 3) return d - 4;
        } else {
            hits = 0;
        }
    }
    return 0xffffffffu;
}

// Does a block leave TEXT as written? 1 if all sixteen words read back.
static IRAM_ATTR uint32_t b_text_kept(const uint32_t hs[8], const uint32_t tw[3])
{
    write_h(hs);
    write_block1(tw, 0x12345678u);
    reg_w(SHA_CONTINUE_REG, 1);
    wait_idle();
    uint32_t want[16] = {tw[0], tw[1], tw[2], 0x12345678u, HW_PAD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, HW_LEN_640};
    for (int k = 0; k < 16; k++) {
        if (reg_r(SHA_TEXT(k)) != want[k]) return 0;
    }
    return 1;
}

// ---- D: the DMA path ----------------------------------------------------

static gdma_channel_handle_t s_tx;
static int s_tx_id = -1;
static DRAM_ATTR bench_desc_t s_desc __attribute__((aligned(64)));
static DRAM_ATTR uint32_t s_buf[32] __attribute__((aligned(64)));

static void dma_teardown(void)
{
    if (s_tx_id < 0) return;
    gdma_disconnect(s_tx);
    gdma_del_channel(s_tx);
    s_tx = NULL;
    s_tx_id = -1;
}

static int dma_setup(void)
{
    if (s_tx_id >= 0) return 1;
    gdma_channel_alloc_config_t cfg = {.direction = GDMA_CHANNEL_DIRECTION_TX};
    if (dma_new_channel(&cfg, &s_tx) != ESP_OK) return 0;
    if (gdma_connect(s_tx, GDMA_MAKE_TRIGGER(GDMA_TRIG_PERIPH_SHA, 0)) != ESP_OK) {
        gdma_del_channel(s_tx);
        return 0;
    }
    gdma_transfer_config_t tc = {.max_data_burst_size = 16, .access_ext_mem = false};
    gdma_config_transfer(s_tx, &tc);
    gdma_get_channel_id(s_tx, &s_tx_id);
    return 1;
}

// Arm the descriptor on `words` (16 per block) and start the DMA: the
// SHA's DMA_START/DMA_CONTINUE then pulls the blocks through.
static inline __attribute__((always_inline)) void dma_arm(const uint32_t *words, uint32_t blocks)
{
    s_desc.dw0.size = 64 * blocks;
    s_desc.dw0.length = 64 * blocks;
    s_desc.dw0.suc_eof = 1;
    s_desc.dw0.owner = 1;
    s_desc.buffer = (void *)words;
    s_desc.next = NULL;
    dma_writeback(&s_desc, sizeof(s_desc));
    dma_ll_tx_set_desc_addr(s_tx_id, (uint32_t)&s_desc);
    dma_ll_tx_start(s_tx_id);
}

// DMA_START over `blocks` blocks from s_buf: cycles until idle.
static IRAM_ATTR uint32_t d_dma_poll(uint32_t blocks)
{
    uint32_t t0 = ccount();
    dma_arm(s_buf, blocks);
    reg_w(SHA_BLOCK_NUM_REG, blocks);
    reg_w(SHA_DMA_START_REG, 1);
    wait_idle();
    return ccount() - t0;
}

// ---- V: the loop variants ------------------------------------------------

typedef struct {
    const uint32_t *hs;   // midstate in H order
    const uint32_t *tw;   // tail words
    const uint32_t *head; // the header's first block, 16 words
    uint32_t start;       // the first nonce
    uint32_t cont_delay;  // cycles from a CONTINUE's issue before the first BUSY read
    uint32_t start_delay; // the same for START
} var_ctx_t;

// V0: the loop as shipped (src/cpuminer.rs hw_batch), polling.
static IRAM_ATTR void v0_shipped(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    zero_text_9_14();
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        write_block1(c->tw, c->start + i);
        reg_w(SHA_CONTINUE_REG, 1);
        wait_idle();
        for (int k = 0; k < 8; k++) reg_w(SHA_TEXT(k), reg_r(SHA_H(k)));
        reg_w(SHA_TEXT(8), HW_PAD2);
        reg_w(SHA_TEXT(15), HW_LEN_256);
        reg_w(SHA_START_REG, 1);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

// V1: the next block's TEXT words written while the engine runs, polling.
static IRAM_ATTR void v1_pipelined(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    zero_text_9_14();
    write_block1(c->tw, c->start);
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        reg_w(SHA_CONTINUE_REG, 1);
        reg_w(SHA_TEXT(8), HW_PAD2);
        reg_w(SHA_TEXT(15), HW_LEN_256);
        wait_idle();
        for (int k = 0; k < 8; k++) reg_w(SHA_TEXT(k), reg_r(SHA_H(k)));
        reg_w(SHA_START_REG, 1);
        write_block1(c->tw, c->start + i + 1);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

// V2: V1 with the digest read in one go (eight loads, then eight
// stores) instead of load-store pairs.
static IRAM_ATTR void v2_grouped(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    zero_text_9_14();
    write_block1(c->tw, c->start);
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        reg_w(SHA_CONTINUE_REG, 1);
        reg_w(SHA_TEXT(8), HW_PAD2);
        reg_w(SHA_TEXT(15), HW_LEN_256);
        wait_idle();
        uint32_t h[8];
        for (int k = 0; k < 8; k++) h[k] = reg_r(SHA_H(k));
        for (int k = 0; k < 8; k++) reg_w(SHA_TEXT(k), h[k]);
        reg_w(SHA_START_REG, 1);
        write_block1(c->tw, c->start + i + 1);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

// V3: V1 with a counted wait before the first BUSY read: the engine's
// drained time, measured from the trigger's issue.
static IRAM_ATTR void v3_counted(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    zero_text_9_14();
    write_block1(c->tw, c->start);
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        uint32_t t0 = ccount();
        reg_w(SHA_CONTINUE_REG, 1);
        reg_w(SHA_TEXT(8), HW_PAD2);
        reg_w(SHA_TEXT(15), HW_LEN_256);
        spin_until(t0, c->cont_delay);
        wait_idle();
        for (int k = 0; k < 8; k++) reg_w(SHA_TEXT(k), reg_r(SHA_H(k)));
        t0 = ccount();
        reg_w(SHA_START_REG, 1);
        write_block1(c->tw, c->start + i + 1);
        spin_until(t0, c->start_delay);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

// V4: V2 and V3 together.
static IRAM_ATTR void v4_grouped_counted(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    zero_text_9_14();
    write_block1(c->tw, c->start);
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        uint32_t t0 = ccount();
        reg_w(SHA_CONTINUE_REG, 1);
        reg_w(SHA_TEXT(8), HW_PAD2);
        reg_w(SHA_TEXT(15), HW_LEN_256);
        spin_until(t0, c->cont_delay);
        wait_idle();
        uint32_t h[8];
        for (int k = 0; k < 8; k++) h[k] = reg_r(SHA_H(k));
        for (int k = 0; k < 8; k++) reg_w(SHA_TEXT(k), h[k]);
        t0 = ccount();
        reg_w(SHA_START_REG, 1);
        write_block1(c->tw, c->start + i + 1);
        spin_until(t0, c->start_delay);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

typedef void (*variant_fn)(const var_ctx_t *, uint32_t *, uint32_t);

static const variant_fn s_variants[N_VAR] = {v0_shipped, v1_pipelined, v2_grouped, v3_counted, v4_grouped_counted};

// ---- the run -------------------------------------------------------------

static inline uint32_t min_of(uint32_t a, uint32_t b)
{
    return a < b ? a : b;
}

static inline uint32_t le32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

int wa_sha_bench_from(const uint32_t mid[8], const uint8_t tail[12], const uint32_t head[16], uint32_t start,
                      wa_sha_bench_t *out)
{
    memset(out, 0, sizeof(*out));
    out->cpu_mhz = (uint32_t)(esp_clk_cpu_freq() / 1000000);
    uint32_t *res = heap_caps_malloc(N_NONCE * sizeof(uint32_t), MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (!res) return 0;

    uint32_t hs[8];
    for (int k = 0; k < 8; k++) hs[k] = __builtin_bswap32(mid[k]);
    uint32_t tw[3];
    for (int k = 0; k < 3; k++) tw[k] = le32(tail + 4 * k);
    // The first hash's state for nonce 0, in H order, for the B probes.
    uint32_t want1[8];
    wa_sha256_block1_h(mid, tail, 0, want1);

    esp_sha_acquire_hardware();
    reg_w(SHA_MODE_REG, SHA2_256);

    // T: each the best of eight, interrupts off.
    uint32_t ram[16];
    out->t_nothing = 0xffffffffu;
    out->t_store16_text = 0xffffffffu;
    out->t_store16_text_memw = 0xffffffffu;
    out->t_store8_h = 0xffffffffu;
    out->t_load8_h = 0xffffffffu;
    out->t_load_busy = 0xffffffffu;
    out->t_store_then_load = 0xffffffffu;
    out->t_store16_ram = 0xffffffffu;
    for (int r = 0; r < 8; r++) {
        portDISABLE_INTERRUPTS();
        out->t_nothing = min_of(out->t_nothing, t_nothing());
        out->t_store16_text = min_of(out->t_store16_text, t_store16_text());
        out->t_store16_text_memw = min_of(out->t_store16_text_memw, t_store16_text_memw());
        out->t_store8_h = min_of(out->t_store8_h, t_store8_h(hs));
        out->t_load8_h = min_of(out->t_load8_h, t_load8_h(ram));
        out->t_load_busy = min_of(out->t_load_busy, t_load_busy());
        out->t_store_then_load = min_of(out->t_store_then_load, t_store_then_load());
        out->t_store16_ram = min_of(out->t_store16_ram, t_store16_ram(ram));
        portENABLE_INTERRUPTS();
    }

    // A: busy windows.
    zero_text_9_14();
    out->a_continue_poll = 0xffffffffu;
    out->a_start_poll = 0xffffffffu;
    for (int r = 0; r < 8; r++) {
        portDISABLE_INTERRUPTS();
        out->a_continue_poll = min_of(out->a_continue_poll, a_poll(hs, tw, SHA_CONTINUE_REG));
        out->a_start_poll = min_of(out->a_start_poll, a_poll(hs, tw, SHA_START_REG));
        portENABLE_INTERRUPTS();
    }
    portDISABLE_INTERRUPTS();
    out->a_continue_delay = a_delay(hs, tw, SHA_CONTINUE_REG);
    portENABLE_INTERRUPTS();
    portDISABLE_INTERRUPTS();
    out->a_start_delay = a_delay(hs, tw, SHA_START_REG);
    portENABLE_INTERRUPTS();
    out->a_drained_poll = 0xffffffffu;
    for (int r = 0; r < 8; r++) {
        portDISABLE_INTERRUPTS();
        out->a_drained_poll = min_of(out->a_drained_poll, a_drained_poll(hs, tw));
        portENABLE_INTERRUPTS();
    }
    portDISABLE_INTERRUPTS();
    out->a_drained_delay = a_drained_delay(hs, tw);
    portENABLE_INTERRUPTS();

    // B: TEXT sampling.
    static const int words[4] = {0, 3, 8, 15};
    for (int j = 0; j < 4; j++) {
        portDISABLE_INTERRUPTS();
        out->b_text_safe[j] = b_text_safe(hs, tw, want1, words[j]);
        portENABLE_INTERRUPTS();
    }
    portDISABLE_INTERRUPTS();
    out->b_text_kept = b_text_kept(hs, tw);
    portENABLE_INTERRUPTS();

    // D: DMA latency (the channel stays allocated for the variants).
    out->d_dma_ok = dma_setup() ? 1 : 0;
    if (out->d_dma_ok) {
        for (int k = 0; k < 16; k++) s_buf[k] = head[k];
        s_buf[16] = tw[0];
        s_buf[17] = tw[1];
        s_buf[18] = tw[2];
        s_buf[19] = 0;
        s_buf[20] = HW_PAD;
        for (int k = 21; k < 31; k++) s_buf[k] = 0;
        s_buf[31] = HW_LEN_640;
        dma_writeback(s_buf, sizeof(s_buf));
        out->d_dma1_poll = 0xffffffffu;
        out->d_dma2_poll = 0xffffffffu;
        for (int r = 0; r < 8; r++) {
            portDISABLE_INTERRUPTS();
            out->d_dma2_poll = min_of(out->d_dma2_poll, d_dma_poll(2));
            portENABLE_INTERRUPTS();
        }
        // One block alone: the header's first block (any block will do).
        for (int r = 0; r < 8; r++) {
            portDISABLE_INTERRUPTS();
            out->d_dma1_poll = min_of(out->d_dma1_poll, d_dma_poll(1));
            portENABLE_INTERRUPTS();
        }
        // Does the DMA path go through TEXT? 1 when TEXT now holds the
        // block the DMA fed (so TEXT cannot be prepared during a DMA hash).
        out->d_dma_text = 1;
        for (int k = 0; k < 16; k++) {
            if (reg_r(SHA_TEXT(k)) != s_buf[k]) out->d_dma_text = 0;
        }
        // The two-block DMA hash must equal the midstate path.
        dma_arm(s_buf, 2);
        reg_w(SHA_BLOCK_NUM_REG, 2);
        reg_w(SHA_DMA_START_REG, 1);
        wait_idle();
        out->d_dma_right = 1;
        for (int k = 0; k < 8; k++) {
            if (reg_r(SHA_H(k)) != want1[k]) out->d_dma_right = 0;
        }
    }

    // V: the variants, each timed over N_NONCE nonces, then checked. The
    // pipelined ones are only tried when the engine does latch TEXT at
    // the trigger (B found a delay for every word).
    int latches = 1;
    for (int j = 0; j < 4; j++) {
        if (out->b_text_safe[j] == 0xffffffffu) latches = 0;
    }
    uint32_t drained = out->a_drained_delay == 0xffffffffu ? 0 : out->a_drained_delay;
    var_ctx_t ctx = {
        .hs = hs,
        .tw = tw,
        .head = head,
        .start = start,
        .cont_delay = drained,
        .start_delay = drained,
    };
    out->v_counted_delay = drained;
    for (int v = 0; v < N_VAR; v++) {
        if (v > 0 && !latches) {
            out->v_cycles[v] = 0;
            out->v_bad[v] = 0xffffffffu;
            continue;
        }
        reg_w(SHA_MODE_REG, SHA2_256);
        uint32_t t0 = ccount();
        s_variants[v](&ctx, res, N_NONCE);
        uint32_t dt = ccount() - t0;
        out->v_cycles[v] = dt / N_NONCE;
        esp_sha_release_hardware();
        uint32_t bad = 0;
        for (uint32_t i = 0; i < N_NONCE; i++) {
            if (res[i] != wa_sha256d_h7(mid, tail, start + i)) bad++;
        }
        out->v_bad[v] = bad;
        vTaskDelay(1);
        esp_sha_acquire_hardware();
    }

    // 5: the firmware's own loop, pipelined when the engine latches TEXT.
    // It stops at the first candidate (H[7]'s upper half zero), so the
    // cycles are per nonce it got through. Checked: its first nonce's
    // H[7], and the candidate's eight words (or, without one, that the
    // software sees none either).
    {
        reg_w(SHA_MODE_REG, SHA2_256);
        uint32_t first_h7 = 0;
        uint32_t h_out[8] = {0};
        uint32_t t0 = ccount();
        uint32_t idx = wa_sha256d_hw_scan(hs, tw, start, N_NONCE, latches, &first_h7, h_out);
        uint32_t dt = ccount() - t0;
        uint32_t scanned = idx < N_NONCE ? idx + 1 : N_NONCE;
        out->v_cycles[5] = dt / scanned;
        esp_sha_release_hardware();
        uint32_t bad = 0;
        if (first_h7 != wa_sha256d_h7(mid, tail, start)) bad++;
        uint8_t hash[32];
        if (idx < N_NONCE) {
            if (wa_sha256d_scan(mid, tail, start + idx, 1, hash) != 0) {
                bad++;
            } else {
                for (int k = 0; k < 8; k++) {
                    if (h_out[k] != le32(hash + 4 * k)) bad++;
                }
            }
        } else if (wa_sha256d_scan(mid, tail, start, N_NONCE, hash) != N_NONCE) {
            bad++;
        }
        out->v_bad[5] = bad;
    }
    // The channel goes back: mbedTLS's own DMA path shares the SHA.
    dma_teardown();
    free(res);
    return 1;
}

int wa_sha_bench(const uint32_t mid[8], const uint8_t tail[12], const uint32_t head[16], wa_sha_bench_t *out)
{
    return wa_sha_bench_from(mid, tail, head, 0, out);
}

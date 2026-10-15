// The ESP32-S3's SHA peripheral measured from the inside, for the chipless
// T-Display-S3 (src/cpuminer.rs, hw_batch): where the cycles of a nonce go.
//
// Everything runs under the mbedTLS crypto lock at a priority above the
// workers, on the core the peripheral worker uses. The micro-probes (T, A,
// B, D) run with interrupts off for a few microseconds each; the loop
// variants (V) run with interrupts on, 2048 nonces each, and every nonce's
// last digest word is checked against the software hash afterwards, so a
// variant that corrupts a hash shows up as `v_bad`, never as a hashrate.
//
// Register traffic goes through inline `s32i`/`l32i` so the compiler
// neither reorders nor adds MEMW on its own; the MEMW variant is measured
// separately (t_store16_text_memw).
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include "esp_attr.h"
#include "esp_cpu.h"
#include "esp_heap_caps.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "soc/hwcrypto_reg.h"
#include "soc/gdma_struct.h"
#include "soc/gdma_channel.h"
#include "hal/gdma_ll.h"
#include "hal/dma_types.h"
#include "esp_private/gdma.h"
#include "warpaxe_glue.h"

// mbedTLS port (sha/sha_dma.h): the crypto lock plus the block's clock.
void esp_sha_acquire_hardware(void);
void esp_sha_release_hardware(void);

#define SHA2_256 2
#define SHA_H(k) (SHA_H_BASE + 4 * (k))
#define SHA_TEXT(k) (SHA_TEXT_BASE + 4 * (k))

// Padding words as the loop writes them (crates/cpuminer sha256.rs).
#define HW_PAD 0x00000080u
#define HW_LEN_640 0x80020000u
#define HW_PAD2 0x00000080u
#define HW_LEN_256 0x00010000u

#define N_VAR 5
#define N_NONCE 2048

static inline __attribute__((always_inline)) void reg_w(uint32_t addr, uint32_t v)
{
    __asm__ volatile("s32i %0, %1, 0" ::"r"(v), "r"(addr) : "memory");
}

static inline __attribute__((always_inline)) uint32_t reg_r(uint32_t addr)
{
    uint32_t v;
    __asm__ volatile("l32i %0, %1, 0" : "=r"(v) : "r"(addr) : "memory");
    return v;
}

static inline __attribute__((always_inline)) void memw(void)
{
    __asm__ volatile("memw" ::: "memory");
}

static inline __attribute__((always_inline)) uint32_t ccount(void)
{
    return esp_cpu_get_cycle_count();
}

static inline __attribute__((always_inline)) void wait_idle(void)
{
    while (reg_r(SHA_BUSY_REG)) {
    }
}

// Spin until `cycles` CPU cycles have passed since `t0`.
static inline __attribute__((always_inline)) void spin_until(uint32_t t0, uint32_t cycles)
{
    while (ccount() - t0 < cycles) {
    }
}

static inline __attribute__((always_inline)) void write_h(const uint32_t hs[8])
{
    for (int k = 0; k < 8; k++) reg_w(SHA_H(k), hs[k]);
}

// The first hash's second block for `nonce` into TEXT (words 9..14 are
// zero in both blocks and written once per run).
static inline __attribute__((always_inline)) void write_block1(const uint32_t tw[3], uint32_t nonce)
{
    reg_w(SHA_TEXT(0), tw[0]);
    reg_w(SHA_TEXT(1), tw[1]);
    reg_w(SHA_TEXT(2), tw[2]);
    reg_w(SHA_TEXT(3), nonce);
    reg_w(SHA_TEXT(4), HW_PAD);
    reg_w(SHA_TEXT(5), 0);
    reg_w(SHA_TEXT(6), 0);
    reg_w(SHA_TEXT(7), 0);
    reg_w(SHA_TEXT(8), 0);
    reg_w(SHA_TEXT(15), HW_LEN_640);
}

static inline __attribute__((always_inline)) void zero_text_9_14(void)
{
    for (int k = 9; k < 15; k++) reg_w(SHA_TEXT(k), 0);
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
static DRAM_ATTR dma_descriptor_t s_desc __attribute__((aligned(4)));
static DRAM_ATTR uint32_t s_buf[32] __attribute__((aligned(16)));
static DRAM_ATTR uint32_t s_buf2[16] __attribute__((aligned(16)));

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
    if (gdma_new_ahb_channel(&cfg, &s_tx) != ESP_OK) return 0;
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
    gdma_ll_tx_set_desc_addr(&GDMA, s_tx_id, (uint32_t)&s_desc);
    gdma_ll_tx_start(&GDMA, s_tx_id);
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
    uint32_t text_delay;  // cycles after a trigger before TEXT may change
    uint32_t cont_delay;  // cycles a CONTINUE keeps the engine busy
    uint32_t start_delay; // cycles a START keeps the engine busy
} var_ctx_t;

// V0: the loop as shipped (src/cpuminer.rs hw_batch), polling.
static IRAM_ATTR void v0_shipped(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    zero_text_9_14();
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        write_block1(c->tw, i);
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
    write_block1(c->tw, 0);
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        uint32_t t0 = ccount();
        reg_w(SHA_CONTINUE_REG, 1);
        spin_until(t0, c->text_delay);
        reg_w(SHA_TEXT(8), HW_PAD2);
        reg_w(SHA_TEXT(15), HW_LEN_256);
        wait_idle();
        for (int k = 0; k < 8; k++) reg_w(SHA_TEXT(k), reg_r(SHA_H(k)));
        t0 = ccount();
        reg_w(SHA_START_REG, 1);
        spin_until(t0, c->text_delay);
        write_block1(c->tw, i + 1);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

// V2: V1 with counted waits: one BUSY read at the earliest moment the
// engine can be idle, instead of polling from the trigger on.
static IRAM_ATTR void v2_counted(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    zero_text_9_14();
    write_block1(c->tw, 0);
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        uint32_t t0 = ccount();
        reg_w(SHA_CONTINUE_REG, 1);
        spin_until(t0, c->text_delay);
        reg_w(SHA_TEXT(8), HW_PAD2);
        reg_w(SHA_TEXT(15), HW_LEN_256);
        spin_until(t0, c->cont_delay);
        wait_idle();
        for (int k = 0; k < 8; k++) reg_w(SHA_TEXT(k), reg_r(SHA_H(k)));
        t0 = ccount();
        reg_w(SHA_START_REG, 1);
        spin_until(t0, c->text_delay);
        write_block1(c->tw, i + 1);
        spin_until(t0, c->start_delay);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

// V3: both hashes through DMA: the first as two blocks from the header
// (no midstate, no H restore), the second as one block from the digest.
static IRAM_ATTR void v3_dma(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    uint32_t *b1 = s_buf;  // header block 0 + block 1: 32 words
    uint32_t *b2 = s_buf2; // the second hash's block
    for (int k = 0; k < 16; k++) b1[k] = c->head[k];
    b1[16] = c->tw[0];
    b1[17] = c->tw[1];
    b1[18] = c->tw[2];
    b1[20] = HW_PAD;
    for (int k = 21; k < 31; k++) b1[k] = 0;
    b1[31] = HW_LEN_640;
    b2[8] = HW_PAD2;
    for (int k = 9; k < 15; k++) b2[k] = 0;
    b2[15] = HW_LEN_256;
    for (uint32_t i = 0; i < n; i++) {
        b1[19] = i;
        dma_arm(b1, 2);
        reg_w(SHA_BLOCK_NUM_REG, 2);
        reg_w(SHA_DMA_START_REG, 1);
        wait_idle();
        for (int k = 0; k < 8; k++) b2[k] = reg_r(SHA_H(k));
        dma_arm(b2, 1);
        reg_w(SHA_BLOCK_NUM_REG, 1);
        reg_w(SHA_DMA_START_REG, 1);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

// V4: the first hash as V1 (midstate, pipelined TEXT), the second through
// DMA from the digest words in RAM.
static IRAM_ATTR void v4_mixed(const var_ctx_t *c, uint32_t *out, uint32_t n)
{
    uint32_t *b2 = s_buf2;
    zero_text_9_14();
    write_block1(c->tw, 0);
    b2[8] = HW_PAD2;
    for (int k = 9; k < 15; k++) b2[k] = 0;
    b2[15] = HW_LEN_256;
    reg_w(SHA_BLOCK_NUM_REG, 1);
    for (uint32_t i = 0; i < n; i++) {
        write_h(c->hs);
        reg_w(SHA_CONTINUE_REG, 1);
        wait_idle();
        for (int k = 0; k < 8; k++) b2[k] = reg_r(SHA_H(k));
        dma_arm(b2, 1);
        uint32_t t0 = ccount();
        reg_w(SHA_DMA_START_REG, 1);
        spin_until(t0, c->text_delay);
        write_block1(c->tw, i + 1);
        wait_idle();
        out[i] = reg_r(SHA_H(7));
    }
}

typedef void (*variant_fn)(const var_ctx_t *, uint32_t *, uint32_t);

static const variant_fn s_variants[N_VAR] = {v0_shipped, v1_pipelined, v2_counted, v3_dma, v4_mixed};

// ---- the run -------------------------------------------------------------

static inline uint32_t min_of(uint32_t a, uint32_t b)
{
    return a < b ? a : b;
}

int wa_sha_bench(const uint32_t mid[8], const uint8_t tail[12], const uint32_t head[16], wa_sha_bench_t *out)
{
    memset(out, 0, sizeof(*out));
    out->cpu_mhz = (uint32_t)wa_cpu_freq_mhz();
    uint32_t *res = heap_caps_malloc(N_NONCE * sizeof(uint32_t), MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (!res) return 0;

    uint32_t hs[8];
    for (int k = 0; k < 8; k++) hs[k] = __builtin_bswap32(mid[k]);
    uint32_t tw[3];
    for (int k = 0; k < 3; k++) {
        tw[k] = (uint32_t)tail[4 * k] | ((uint32_t)tail[4 * k + 1] << 8) | ((uint32_t)tail[4 * k + 2] << 16) |
                ((uint32_t)tail[4 * k + 3] << 24);
    }
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

    // V: the variants, each timed over N_NONCE nonces, then checked.
    uint32_t safe = 0;
    for (int j = 0; j < 4; j++) {
        if (out->b_text_safe[j] != 0xffffffffu && out->b_text_safe[j] > safe) safe = out->b_text_safe[j];
    }
    var_ctx_t ctx = {
        .hs = hs,
        .tw = tw,
        .head = head,
        .text_delay = safe + 8,
        .cont_delay = out->a_continue_delay == 0xffffffffu ? 0 : out->a_continue_delay,
        .start_delay = out->a_start_delay == 0xffffffffu ? 0 : out->a_start_delay,
    };
    out->v_text_delay = ctx.text_delay;
    for (int v = 0; v < N_VAR; v++) {
        if ((v == 3 || v == 4) && !out->d_dma_ok) {
            out->v_cycles[v] = 0;
            out->v_bad[v] = 0xffffffffu;
            continue;
        }
        // Pipelining is only tried when the engine does latch TEXT.
        if ((v == 1 || v == 2 || v == 4) && safe == 0 && out->b_text_safe[0] == 0xffffffffu) {
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
            if (res[i] != wa_sha256d_h7(mid, tail, i)) bad++;
        }
        out->v_bad[v] = bad;
        vTaskDelay(1);
        esp_sha_acquire_hardware();
    }
    esp_sha_release_hardware();
    // The channel goes back: mbedTLS's own DMA path shares the SHA.
    dma_teardown();
    free(res);
    return 1;
}

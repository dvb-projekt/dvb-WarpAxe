// The ESP32-S3's SHA peripheral as the hash loop and its bench see it
// (soc/hwcrypto_reg.h): a message block goes into TEXT as sixteen words,
// the digest state lives in H as eight. Private to the component: not
// under include/, so bindgen never sees it.
//
// Register traffic goes through inline `s32i`/`l32i` so the compiler
// neither reorders the accesses nor adds a MEMW of its own: a store is
// posted (~6 cycles at 240 MHz), a load waits for the bus (~31), and
// the engine takes ~140 cycles a block. It latches TEXT at the trigger
// and leaves it alone, so the next block's words may go in while it
// runs (the bench, warpaxe_shabench.c, 2026-10-15).
#pragma once
#include <stdint.h>
#include "esp_cpu.h"
#include "soc/hwcrypto_reg.h"

#define WA_SHA2_256 2
#define WA_SHA_H(k) (SHA_H_BASE + 4 * (k))
#define WA_SHA_TEXT(k) (SHA_TEXT_BASE + 4 * (k))

// Padding words as the loop writes them (crates/cpuminer sha256.rs).
#define WA_HW_PAD 0x00000080u
#define WA_HW_LEN_640 0x80020000u
#define WA_HW_PAD2 0x00000080u
#define WA_HW_LEN_256 0x00010000u

static inline __attribute__((always_inline)) void wa_reg_w(uint32_t addr, uint32_t v)
{
    __asm__ volatile("s32i %0, %1, 0" ::"r"(v), "r"(addr) : "memory");
}

static inline __attribute__((always_inline)) uint32_t wa_reg_r(uint32_t addr)
{
    uint32_t v;
    __asm__ volatile("l32i %0, %1, 0" : "=r"(v) : "r"(addr) : "memory");
    return v;
}

static inline __attribute__((always_inline)) void wa_memw(void)
{
    __asm__ volatile("memw" ::: "memory");
}

static inline __attribute__((always_inline)) uint32_t wa_ccount(void)
{
    return esp_cpu_get_cycle_count();
}

static inline __attribute__((always_inline)) void wa_sha_wait_idle(void)
{
    while (wa_reg_r(SHA_BUSY_REG)) {
    }
}

// The midstate into the eight H registers.
static inline __attribute__((always_inline)) void wa_sha_write_h(const uint32_t hs[8])
{
    for (int k = 0; k < 8; k++) wa_reg_w(WA_SHA_H(k), hs[k]);
}

// The first hash's second block for `nonce` into TEXT: words 9..14 are
// zero in both blocks and written once (wa_sha_zero_text_9_14).
static inline __attribute__((always_inline)) void wa_sha_write_block1(const uint32_t tw[3], uint32_t nonce)
{
    wa_reg_w(WA_SHA_TEXT(0), tw[0]);
    wa_reg_w(WA_SHA_TEXT(1), tw[1]);
    wa_reg_w(WA_SHA_TEXT(2), tw[2]);
    wa_reg_w(WA_SHA_TEXT(3), nonce);
    wa_reg_w(WA_SHA_TEXT(4), WA_HW_PAD);
    wa_reg_w(WA_SHA_TEXT(5), 0);
    wa_reg_w(WA_SHA_TEXT(6), 0);
    wa_reg_w(WA_SHA_TEXT(7), 0);
    wa_reg_w(WA_SHA_TEXT(8), 0);
    wa_reg_w(WA_SHA_TEXT(15), WA_HW_LEN_640);
}

static inline __attribute__((always_inline)) void wa_sha_zero_text_9_14(void)
{
    for (int k = 9; k < 15; k++) wa_reg_w(WA_SHA_TEXT(k), 0);
}

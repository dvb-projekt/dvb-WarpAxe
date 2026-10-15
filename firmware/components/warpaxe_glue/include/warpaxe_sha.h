// The chipless devkit's hash code (warpaxe_cpuhash.c, warpaxe_shabench.c):
// double SHA-256 of a block header from its midstate, once in software
// and once on the SHA peripheral, and the bench that measures that
// peripheral. Builds for the ESP32-S3 and the ESP32-P4 (warpaxe_sha_regs.h);
// the standalone bench, tools/sha-bench, compiles these same files, so
// nothing in here may need TinyUSB or the rest of the firmware.
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// The SHA-256 state after a header's first block (64 bytes), as the
/// eight native words the software loop continues from.
void wa_sha256_midstate(const uint8_t block[64], uint32_t mid[8]);

/// Double SHA-256 of a block header from its midstate for the nonces
/// `start..start+n`, in software. Returns the index of the first nonce
/// whose hash has its two most significant bytes zero, with the hash in
/// `hash_out` (natural byte order), or `n` when none has.
uint32_t wa_sha256d_scan(const uint32_t mid[8], const uint8_t tail[12], uint32_t start,
                         uint32_t n, uint8_t hash_out[32]);

/// The peripheral loop: the SHA engine over the nonces `start..start+n`,
/// `hs` the midstate in H order, `tw` the tail words, the caller holding
/// the crypto lock. With `pipelined` the next block's words go into TEXT
/// while the engine runs. Returns the index of the first candidate
/// (upper half of H[7] zero) with its eight H words in `h_out`, or `n`;
/// `first_h7` is the first nonce's H[7].
uint32_t wa_sha256d_hw_scan(const uint32_t hs[8], const uint32_t tw[3], uint32_t start,
                            uint32_t n, int pipelined, uint32_t *first_h7, uint32_t h_out[8]);

/// The first hash's state after its second block for `nonce`, as the
/// eight words the SHA peripheral holds in H (memory order).
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
    uint32_t t_nothing;            // two cycle-counter reads back to back
    uint32_t t_store16_text;       // sixteen stores to TEXT
    uint32_t t_store16_text_memw;  // the same with a MEMW (FENCE) after each
    uint32_t t_store8_h;           // eight stores to H
    uint32_t t_load8_h;            // eight loads from H
    uint32_t t_load_busy;          // one load of BUSY
    uint32_t t_store_then_load;    // a TEXT store, then a BUSY load
    uint32_t t_store16_ram;        // sixteen stores to internal RAM, for scale
    /// A: how long a trigger keeps the engine busy. Polled: until the
    /// first idle read. Delay: the smallest counted wait after which one
    /// read finds it idle. Drained: the same after a MEMW emptied the bus.
    uint32_t a_continue_poll;
    uint32_t a_start_poll;
    uint32_t a_continue_delay;
    uint32_t a_start_delay;
    uint32_t a_drained_poll;
    uint32_t a_drained_delay;
    /// B: the delay after CONTINUE from which TEXT[0, 3, 8, 15] may be
    /// overwritten without touching the digest (0: the engine latches
    /// TEXT at the trigger), and whether a block leaves TEXT as written.
    uint32_t b_text_safe[4];
    uint32_t b_text_kept;
    /// D: the DMA path. ok: a channel was had. Cycles from DMA_START to
    /// idle for one and two blocks; whether a two-block DMA hash equals
    /// the midstate path; whether TEXT holds the DMA'd block afterwards.
    uint32_t d_dma_ok;
    uint32_t d_dma1_poll;
    uint32_t d_dma2_poll;
    uint32_t d_dma_right;
    uint32_t d_dma_text;
    /// V: the loop variants over 2048 nonces, cycles a nonce and nonces
    /// whose H[7] the software hash contradicted. 0: the loop as first
    /// shipped. 1: the next block's TEXT words written while the engine
    /// runs. 2: that with the digest read in one go. 3: that with a
    /// counted wait before the first BUSY read. 4: 2 and 3 together.
    /// 5: the firmware's own loop (wa_sha256d_hw_scan), pipelined when
    /// the engine latches TEXT; it stops at the first candidate, so its
    /// cycles are per nonce it got through.
    uint32_t v_counted_delay;      // the counted wait of 3 and 4, cycles
    uint32_t v_cycles[6];
    uint32_t v_bad[6];
} wa_sha_bench_t;

/// Run the bench on the calling core, under the crypto lock, over the
/// nonces `start..start+2048`. `head` is the header's first 64 bytes as
/// little-endian words (for the DMA probe). Returns 0 when no memory
/// was had.
int wa_sha_bench_from(const uint32_t mid[8], const uint8_t tail[12], const uint32_t head[16],
                      uint32_t start, wa_sha_bench_t *out);

/// wa_sha_bench_from with the nonces from 0.
int wa_sha_bench(const uint32_t mid[8], const uint8_t tail[12], const uint32_t head[16],
                 wa_sha_bench_t *out);

#ifdef __cplusplus
}
#endif

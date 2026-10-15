// tools/sha-bench: the SHA peripheral of an ESP32-S3 or an ESP32-P4
// measured from the inside, standalone (no network, no mining). The
// WarpAxe firmware's bench (firmware/components/warpaxe_glue/
// warpaxe_shabench.c, docs/06) runs over the Bitcoin genesis block's
// header, once at boot and then every 30 s, and prints a table plus one
// machine-readable line (`SHABENCH {...}`) on the console. Every loop
// variant is re-hashed in software: a variant that corrupts a hash shows
// as `bad`, never as a hashrate. The firmware's own loop has to find the
// genesis block's nonce, which sits inside the 2048 nonces scanned.
#include <inttypes.h>
#include <stdio.h>
#include <string.h>
#include "sdkconfig.h"
#include "esp_chip_info.h"
#include "esp_idf_version.h"
#include "esp_private/esp_clk.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "warpaxe_sha.h"

// mbedTLS port (sha/sha_dma.h): the crypto lock plus the block's clock.
void esp_sha_acquire_hardware(void);
void esp_sha_release_hardware(void);

// Bitcoin block 0 (2009-01-03): version 1, no previous block, the
// coinbase's merkle root, time 1231006505, bits 0x1d00ffff, nonce
// 0x7c2bac1d. Its hash as displayed (bytes reversed):
// 000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f.
static const uint8_t GENESIS_HEADER[80] = {
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x3b, 0xa3, 0xed, 0xfd, 0x7a, 0x7b, 0x12, 0xb2, 0x7a, 0xc7, 0x2c, 0x3e,
    0x67, 0x76, 0x8f, 0x61, 0x7f, 0xc8, 0x1b, 0xc3, 0x88, 0x8a, 0x51, 0x32, 0x3a, 0x9f, 0xb8, 0xaa,
    0x4b, 0x1e, 0x5e, 0x4a, 0x29, 0xab, 0x5f, 0x49, 0xff, 0xff, 0x00, 0x1d, 0x1d, 0xac, 0x2b, 0x7c,
};
// The same hash in natural order (the digest's bytes as SHA-256 emits them).
static const uint8_t GENESIS_HASH[32] = {
    0x6f, 0xe2, 0x8c, 0x0a, 0xb6, 0xf1, 0xb3, 0x72, 0xc1, 0xa6, 0xa2, 0x46, 0xae, 0x63, 0xf7, 0x4f,
    0x93, 0x1e, 0x83, 0x65, 0xe1, 0x5a, 0x08, 0x9c, 0x68, 0xd6, 0x19, 0x00, 0x00, 0x00, 0x00, 0x00,
};
#define GENESIS_NONCE 0x7c2bac1du
// The bench scans 2048 nonces; the genesis nonce is the 1001st of them.
#define GENESIS_INDEX 1000u
#define START_NONCE (GENESIS_NONCE - GENESIS_INDEX)
#define N_NONCE 2048u
#define PERIOD_S 30

#define NONE 0xffffffffu

static uint32_t le32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

// A cycle count for the table: the number, or "none" for 0xffffffff.
static const char *cyc(uint32_t v, char buf[16])
{
    if (v == NONE) return "none";
    snprintf(buf, 16, "%" PRIu32, v);
    return buf;
}

static const char *yesno(uint32_t v)
{
    return v ? "yes" : "no";
}

// The chip as the image sees it, for the table's header and the JSON.
typedef struct {
    const char *target;
    const char *arch;
    char rev[8];
    int cores;
    int cpu_mhz;
    int apb_mhz;
    int xtal_mhz;
    const char *idf;
} chip_t;

static void chip_fill(chip_t *c)
{
    esp_chip_info_t info;
    esp_chip_info(&info);
    c->target = CONFIG_IDF_TARGET;
#if defined(__XTENSA__)
    c->arch = "xtensa";
#else
    c->arch = "riscv";
#endif
    snprintf(c->rev, sizeof(c->rev), "v%u.%u", info.revision / 100, info.revision % 100);
    c->cores = info.cores;
    c->cpu_mhz = esp_clk_cpu_freq() / 1000000;
    c->apb_mhz = esp_clk_apb_freq() / 1000000;
    c->xtal_mhz = esp_clk_xtal_freq() / 1000000;
    c->idf = esp_get_idf_version();
}

// The software reference against the known hash: the loop that checks
// every variant must itself be right before anything is measured.
static int software_self_test(const uint32_t mid[8], const uint8_t tail[12])
{
    uint8_t hash[32];
    if (wa_sha256d_scan(mid, tail, GENESIS_NONCE, 1, hash) != 0) return 0;
    if (memcmp(hash, GENESIS_HASH, 32) != 0) return 0;
    if (wa_sha256d_h7(mid, tail, GENESIS_NONCE) != le32(GENESIS_HASH + 28)) return 0;
    return 1;
}

// The firmware's own loop over the bench's nonces: it must stop at the
// genesis nonce with the genesis hash in H.
static int genesis_by_hardware(const uint32_t mid[8], const uint8_t tail[12], int pipelined, uint32_t *idx_out)
{
    uint32_t hs[8], tw[3], h_out[8], first_h7 = 0;
    for (int k = 0; k < 8; k++) hs[k] = __builtin_bswap32(mid[k]);
    for (int k = 0; k < 3; k++) tw[k] = le32(tail + 4 * k);
    esp_sha_acquire_hardware();
    uint32_t idx = wa_sha256d_hw_scan(hs, tw, START_NONCE, N_NONCE, pipelined, &first_h7, h_out);
    esp_sha_release_hardware();
    *idx_out = idx;
    if (idx != GENESIS_INDEX) return 0;
    if (first_h7 != wa_sha256d_h7(mid, tail, START_NONCE)) return 0;
    for (int k = 0; k < 8; k++) {
        if (h_out[k] != le32(GENESIS_HASH + 4 * k)) return 0;
    }
    return 1;
}

static const char *const VARIANT_NAMES[6] = {
    "the loop as first shipped (store, trigger, poll, load)",
    "next block's words written while the engine runs",
    "1 with the digest read in one go (8 loads, 8 stores)",
    "1 with a counted wait before the first BUSY read",
    "2 and 3 together",
    "the WarpAxe firmware loop (wa_sha256d_hw_scan)",
};

static void print_table(const chip_t *c, const wa_sha_bench_t *b, unsigned run, int pipelined, int genesis_ok,
                        uint32_t genesis_idx)
{
    char s1[16], s2[16], s3[16], s4[16];
    printf("\n==== SHA bench run %u: %s %s, %d cores, CPU %d MHz, APB %d MHz, XTAL %d MHz, ESP-IDF %s (%s) ====\n",
           run, c->target, c->rev, c->cores, c->cpu_mhz, c->apb_mhz, c->xtal_mhz, c->idf, c->arch);
    printf("T  register access, CPU cycles, best of 8 with interrupts off\n");
    printf("   two cycle-counter reads back to back   %6s\n", cyc(b->t_nothing, s1));
    printf("   16 stores to TEXT                      %6s   %.1f a store\n", cyc(b->t_store16_text, s1),
           b->t_store16_text / 16.0);
    printf("   the same with MEMW/FENCE after each    %6s   %.1f a store\n", cyc(b->t_store16_text_memw, s1),
           b->t_store16_text_memw / 16.0);
    printf("   8 stores to H                          %6s   %.1f a store\n", cyc(b->t_store8_h, s1),
           b->t_store8_h / 8.0);
    printf("   8 loads from H                         %6s   %.1f a load\n", cyc(b->t_load8_h, s1),
           b->t_load8_h / 8.0);
    printf("   one BUSY load                          %6s\n", cyc(b->t_load_busy, s1));
    printf("   a TEXT store, then a BUSY load         %6s\n", cyc(b->t_store_then_load, s1));
    printf("   16 stores to internal RAM (for scale)  %6s\n", cyc(b->t_store16_ram, s1));
    printf("A  how long a trigger keeps the engine busy, cycles\n");
    printf("   CONTINUE -> idle after 18 stores       %6s polled   %6s as a counted delay\n",
           cyc(b->a_continue_poll, s1), cyc(b->a_continue_delay, s2));
    printf("   START -> idle after 18 stores          %6s polled   %6s as a counted delay\n",
           cyc(b->a_start_poll, s1), cyc(b->a_start_delay, s2));
    printf("   CONTINUE -> idle, bus drained first    %6s polled   %6s as a counted delay   (the engine alone)\n",
           cyc(b->a_drained_poll, s1), cyc(b->a_drained_delay, s2));
    printf("B  when the engine samples TEXT\n");
    printf("   TEXT[0] TEXT[3] TEXT[8] TEXT[15] safe to overwrite after   %s %s %s %s cycles   (0 = latched at the trigger)\n",
           cyc(b->b_text_safe[0], s1), cyc(b->b_text_safe[1], s2), cyc(b->b_text_safe[2], s3),
           cyc(b->b_text_safe[3], s4));
    printf("   a block leaves TEXT as written         %s\n", yesno(b->b_text_kept));
    printf("D  the DMA path (a DMA channel on the SHA trigger): %s\n", b->d_dma_ok ? "ok" : "no channel, skipped");
    if (b->d_dma_ok) {
        printf("   DMA_START one block -> idle            %6s\n", cyc(b->d_dma1_poll, s1));
        printf("   DMA_START two blocks -> idle           %6s\n", cyc(b->d_dma2_poll, s1));
        printf("   two-block DMA hash = midstate path     %s\n", yesno(b->d_dma_right));
        printf("   TEXT holds the DMA'd block afterwards  %s\n", yesno(b->d_dma_text));
    }
    printf("V  loop variants, %u nonces each, every nonce re-hashed in software (bad must be 0)\n", N_NONCE);
    printf("   #  variant                                                cycles/nonce     kH/s   bad\n");
    for (int v = 0; v < 6; v++) {
        if (b->v_cycles[v] == 0) {
            printf("   %d  %-52s   not run (the engine does not latch TEXT)\n", v, VARIANT_NAMES[v]);
            continue;
        }
        double khs = (double)b->cpu_mhz * 1e3 / (double)b->v_cycles[v];
        printf("   %d  %-52s   %6" PRIu32 "       %7.1f   %" PRIu32 "\n", v, VARIANT_NAMES[v], b->v_cycles[v], khs,
               b->v_bad[v]);
    }
    printf("   counted wait of 3 and 4: %" PRIu32 " cycles; variants 1-5 %s\n", b->v_counted_delay,
           pipelined ? "pipelined (the engine latches TEXT at the trigger)" : "run unpipelined");
    if (genesis_ok) {
        printf("   the firmware loop found the genesis block: nonce 0x%08x at index %" PRIu32 ", hash as expected\n",
               GENESIS_NONCE, genesis_idx);
    } else {
        printf("   the firmware loop DID NOT find the genesis block (stopped at index %" PRIu32 ", expected %u)\n",
               genesis_idx, GENESIS_INDEX);
    }
}

static void print_json(const chip_t *c, const wa_sha_bench_t *b, unsigned run, int pipelined, int genesis_ok)
{
    printf("SHABENCH {\"run\":%u,\"chip\":\"%s\",\"rev\":\"%s\",\"arch\":\"%s\",\"cores\":%d,\"cpuMhz\":%d,"
           "\"apbMhz\":%d,\"xtalMhz\":%d,\"idf\":\"%s\",\"nonces\":%u,\"startNonce\":%" PRIu32 ",",
           run, c->target, c->rev, c->arch, c->cores, c->cpu_mhz, c->apb_mhz, c->xtal_mhz, c->idf, N_NONCE,
           (uint32_t)START_NONCE);
    printf("\"tNothing\":%" PRIu32 ",\"tStore16Text\":%" PRIu32 ",\"tStore16TextMemw\":%" PRIu32
           ",\"tStore8H\":%" PRIu32 ",\"tLoad8H\":%" PRIu32 ",\"tLoadBusy\":%" PRIu32 ",\"tStoreThenLoad\":%" PRIu32
           ",\"tStore16Ram\":%" PRIu32 ",",
           b->t_nothing, b->t_store16_text, b->t_store16_text_memw, b->t_store8_h, b->t_load8_h, b->t_load_busy,
           b->t_store_then_load, b->t_store16_ram);
    printf("\"aContinuePoll\":%" PRIu32 ",\"aStartPoll\":%" PRIu32 ",\"aContinueDelay\":%" PRIu32
           ",\"aStartDelay\":%" PRIu32 ",\"aDrainedPoll\":%" PRIu32 ",\"aDrainedDelay\":%" PRIu32 ",",
           b->a_continue_poll, b->a_start_poll, b->a_continue_delay, b->a_start_delay, b->a_drained_poll,
           b->a_drained_delay);
    printf("\"bTextSafe\":[%" PRIu32 ",%" PRIu32 ",%" PRIu32 ",%" PRIu32 "],\"bTextKept\":%" PRIu32 ",",
           b->b_text_safe[0], b->b_text_safe[1], b->b_text_safe[2], b->b_text_safe[3], b->b_text_kept);
    printf("\"dDmaOk\":%" PRIu32 ",\"dDma1Poll\":%" PRIu32 ",\"dDma2Poll\":%" PRIu32 ",\"dDmaRight\":%" PRIu32
           ",\"dDmaText\":%" PRIu32 ",",
           b->d_dma_ok, b->d_dma1_poll, b->d_dma2_poll, b->d_dma_right, b->d_dma_text);
    printf("\"vCountedDelay\":%" PRIu32 ",\"vPipelined\":%d,\"vCycles\":[", b->v_counted_delay, pipelined);
    for (int v = 0; v < 6; v++) printf("%s%" PRIu32, v ? "," : "", b->v_cycles[v]);
    printf("],\"vKhs\":[");
    for (int v = 0; v < 6; v++) {
        double khs = b->v_cycles[v] ? (double)b->cpu_mhz * 1e3 / (double)b->v_cycles[v] : 0.0;
        printf("%s%.1f", v ? "," : "", khs);
    }
    printf("],\"vBad\":[");
    for (int v = 0; v < 6; v++) printf("%s%" PRIu32, v ? "," : "", b->v_bad[v]);
    printf("],\"genesisFound\":%s}\n", genesis_ok ? "true" : "false");
}

void app_main(void)
{
    uint32_t mid[8], head[16];
    wa_sha256_midstate(GENESIS_HEADER, mid);
    for (int k = 0; k < 16; k++) head[k] = le32(GENESIS_HEADER + 4 * k);
    const uint8_t *tail = GENESIS_HEADER + 64;

    chip_t chip;
    chip_fill(&chip);
    printf("\nWarpAxe SHA bench: the SHA peripheral measured over the Bitcoin genesis header\n");
    printf("chip %s %s, %d cores, CPU %d MHz, APB %d MHz, XTAL %d MHz, ESP-IDF %s, %s\n", chip.target, chip.rev,
           chip.cores, chip.cpu_mhz, chip.apb_mhz, chip.xtal_mhz, chip.idf, chip.arch);
    int sw_ok = software_self_test(mid, tail);
    printf("software reference against the genesis hash: %s\n", sw_ok ? "ok" : "WRONG (the checks below mean nothing)");
    printf("a run takes about a second and repeats every %d s; the SHABENCH line is the machine-readable result\n",
           PERIOD_S);

    for (unsigned run = 1;; run++) {
        wa_sha_bench_t b;
        if (!wa_sha_bench_from(mid, tail, head, START_NONCE, &b)) {
            printf("sha bench: no memory for the result buffer\n");
            vTaskDelay(pdMS_TO_TICKS(PERIOD_S * 1000));
            continue;
        }
        int pipelined = 1;
        for (int j = 0; j < 4; j++) {
            if (b.b_text_safe[j] == NONE) pipelined = 0;
        }
        uint32_t genesis_idx = 0;
        int genesis_ok = genesis_by_hardware(mid, tail, pipelined, &genesis_idx);
        print_table(&chip, &b, run, pipelined, genesis_ok, genesis_idx);
        print_json(&chip, &b, run, pipelined, genesis_ok);
        fflush(stdout);
        vTaskDelay(pdMS_TO_TICKS(PERIOD_S * 1000));
    }
}

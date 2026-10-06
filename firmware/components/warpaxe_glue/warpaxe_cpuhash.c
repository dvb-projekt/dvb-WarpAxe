// The software hash of the T-Display-S3 without chips (src/cpuminer.rs):
// double SHA-256 of a block header from its midstate, nonce after nonce, in C
// so GCC's -O2 and IRAM_ATTR do what they do for NerdMiner's nerd_sha256d. The
// Rust loop managed 11 kH/s on the Xtensa (a rolled 64-round loop, ~60
// instructions a round); this one unrolls eight rounds per iteration.
#include <stdint.h>
#include <string.h>
#include "esp_attr.h"
#include "warpaxe_glue.h"

static DRAM_ATTR const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

#define ROTR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))
#define S0(x) (ROTR(x, 7) ^ ROTR(x, 18) ^ ((x) >> 3))
#define S1(x) (ROTR(x, 17) ^ ROTR(x, 19) ^ ((x) >> 10))
#define S2(x) (ROTR(x, 2) ^ ROTR(x, 13) ^ ROTR(x, 22))
#define S3(x) (ROTR(x, 6) ^ ROTR(x, 11) ^ ROTR(x, 25))
#define F0(x, y, z) (((x) & (y)) | ((z) & ((x) | (y))))
#define F1(x, y, z) ((z) ^ ((x) & ((y) ^ (z))))
#define P(a, b, c, d, e, f, g, h, x, k)              \
    do {                                             \
        uint32_t t1 = (h) + S3(e) + F1(e, f, g) + (k) + (x); \
        uint32_t t2 = S2(a) + F0(a, b, c);           \
        (d) += t1;                                   \
        (h) = t1 + t2;                               \
    } while (0)

// One compression of W[0..15] (message words, big-endian already) into s.
static IRAM_ATTR void compress(uint32_t s[8], uint32_t W[64])
{
    for (int t = 16; t < 64; t++) {
        W[t] = S1(W[t - 2]) + W[t - 7] + S0(W[t - 15]) + W[t - 16];
    }
    uint32_t a = s[0], b = s[1], c = s[2], d = s[3], e = s[4], f = s[5], g = s[6], h = s[7];
    for (int t = 0; t < 64; t += 8) {
        P(a, b, c, d, e, f, g, h, W[t], K[t]);
        P(h, a, b, c, d, e, f, g, W[t + 1], K[t + 1]);
        P(g, h, a, b, c, d, e, f, W[t + 2], K[t + 2]);
        P(f, g, h, a, b, c, d, e, W[t + 3], K[t + 3]);
        P(e, f, g, h, a, b, c, d, W[t + 4], K[t + 4]);
        P(d, e, f, g, h, a, b, c, W[t + 5], K[t + 5]);
        P(c, d, e, f, g, h, a, b, W[t + 6], K[t + 6]);
        P(b, c, d, e, f, g, h, a, W[t + 7], K[t + 7]);
    }
    s[0] += a; s[1] += b; s[2] += c; s[3] += d; s[4] += e; s[5] += f; s[6] += g; s[7] += h;
}

static inline uint32_t be32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3];
}

IRAM_ATTR uint32_t wa_sha256d_scan(const uint32_t mid[8], const uint8_t tail[12], uint32_t start,
                                   uint32_t n, uint8_t hash_out[32])
{
    uint32_t t0 = be32(tail), t1 = be32(tail + 4), t2 = be32(tail + 8);
    for (uint32_t i = 0; i < n; i++) {
        uint32_t nonce = start + i;
        uint32_t W[64];
        uint32_t s[8];
        // First hash, second block: tail, nonce (little-endian in the
        // header, so byte-swapped as a word), padding, 640 bits.
        W[0] = t0; W[1] = t1; W[2] = t2;
        W[3] = __builtin_bswap32(nonce);
        W[4] = 0x80000000u;
        for (int k = 5; k < 15; k++) W[k] = 0;
        W[15] = 640;
        memcpy(s, mid, sizeof(s));
        compress(s, W);
        // Second hash: the digest words, padding, 256 bits.
        for (int k = 0; k < 8; k++) W[k] = s[k];
        W[8] = 0x80000000u;
        for (int k = 9; k < 15; k++) W[k] = 0;
        W[15] = 256;
        s[0] = 0x6a09e667; s[1] = 0xbb67ae85; s[2] = 0x3c6ef372; s[3] = 0xa54ff53a;
        s[4] = 0x510e527f; s[5] = 0x9b05688c; s[6] = 0x1f83d9ab; s[7] = 0x5be0cd19;
        compress(s, W);
        // Candidate: the hash's two most significant bytes (the last two
        // in natural order) are zero, i.e. the low half of the last word.
        if ((s[7] & 0xffffu) == 0) {
            for (int k = 0; k < 8; k++) {
                hash_out[4 * k] = (uint8_t)(s[k] >> 24);
                hash_out[4 * k + 1] = (uint8_t)(s[k] >> 16);
                hash_out[4 * k + 2] = (uint8_t)(s[k] >> 8);
                hash_out[4 * k + 3] = (uint8_t)s[k];
            }
            return i;
        }
    }
    return n;
}

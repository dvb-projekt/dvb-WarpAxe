# The T-Display-S3's SHA peripheral: where the cycles of a nonce go

Written 2026-10-15 for the chipless devkit (`192.0.2.111`, profile
`t-display-s3`, WarpAxe v0.1.28 at 240 MHz on USB power). Handover from
dvb-WarpAxe #2: the firmware did ~293 kH/s (peripheral loop 265 on core
1, C software loop 29 on core 0), NMMiner does 384.5 kH/s on the same
board. Task: measure first, then speed up the peripheral path as far as
it is safe, every candidate still re-hashed in software.

Result in one line: a nonce cost ~890 cycles, of which the engine itself
takes ~280 and the register traffic the rest; writing the next block's
words while the engine runs brings it to ~740 (325 kH/s at the probe in
C, 0 wrong hashes in 2048), and that is close to the floor this engine
has. At the devkit: 292 → 340 kH/s over 30 minutes (+16 %). The DMA
path, counted waits and grouped loads do not help.

## The loop before

`hw_batch` (NerdMiner v2's `minerWorkerHw`): per nonce eight stores to
`H` (the midstate), ten stores to `TEXT` (tail, nonce, padding, length),
`CONTINUE`, a busy wait, eight loads from `H` each stored into `TEXT`,
two padding stores, `START`, a busy wait, one load of `H[7]`. About 39
register accesses and two engine runs; 3.7 µs = 886 cycles.

## The bench

`firmware/components/warpaxe_glue/warpaxe_shabench.c`, IRAM, `-O2`, run
by the peripheral worker on core 1 at priority 20 after its start-up
probe (and again on `POST /api/v1/cpuminer/bench`), under the mbedTLS
crypto lock. Register access through inline `s32i`/`l32i` so the compiler
neither reorders nor inserts `MEMW`. Results in `cpuMiner.bench` of
`/api/system/info` and in four `sha bench:` log lines. Every loop variant
runs 2048 nonces and every nonce's `H[7]` is checked against the software
hash afterwards: a variant that corrupts a hash shows as `vBad`, never as
a hashrate.

| Probe | What it does |
|---|---|
| T | CCOUNT around register sequences, best of eight with interrupts off: 16 TEXT stores with and without MEMW, 8 H stores, 8 H loads, one BUSY load, a store then a load, 16 DRAM stores for scale |
| A | How long CONTINUE/START keep the engine busy: polled, as the smallest counted delay after which one BUSY read finds it idle, and the same after a MEMW drained the store backlog |
| B | After CONTINUE, a counted delay, then TEXT[k] overwritten (k = 0, 3, 8, 15): the smallest delay from which the digest stays right. Also whether a block leaves TEXT as written |
| D | An own GDMA TX channel on the SHA trigger (freed after the run): DMA_START over one and two blocks to idle, whether a two-block DMA hash of the header equals the midstate path, and whether the DMA feeds through TEXT |
| V | Five loop variants (below) |

## What the engine does (240 MHz CPU cycles)

| Measurement | Cycles | Meaning |
|---|---|---|
| 16 stores to TEXT | 102–104 | **6.5 a store**: posted, the CPU does not wait (16 DRAM stores: 101) |
| the same with MEMW after each | 277–289 | 18 a store once the bus has to confirm it |
| 8 stores to H | 70 | 8.75 a store |
| 8 loads from H | 252–262 | **31.5 a load**: the CPU waits for the bus every time |
| one BUSY load | 20 | a lone load, bus idle |
| a TEXT store then a BUSY load | 33 | the load waits behind the store |
| CONTINUE → idle, polled, after 18 stores | 283 | includes the backlog of the stores before the trigger |
| the same as a counted delay | 244–248 | |
| CONTINUE → idle after a MEMW (bus drained) | 165 polled / 142 counted | **the engine itself: ~140 a block** (~0.6 µs) |
| START → idle | same as CONTINUE | |
| TEXT[0, 3, 8, 15] safe to overwrite after | 0, 0, 0, 0 | **the engine latches TEXT at the trigger** |
| TEXT read back after a block | unchanged | a block leaves TEXT alone |
| DMA_START, one block | 399 | |
| DMA_START, two blocks | 525 | a second block adds 126 |
| two-block DMA hash of the header | = midstate path | the mode works |
| TEXT after a DMA hash | holds the DMA'd block | **the DMA feeds the engine through TEXT**: nothing can be prepared there meanwhile |

So, unlike the classic ESP32 (HenrysCat/NerdMiner-Modded: engine ~68
cycles, ~247 a nonce), the S3's engine is not cheap against its bus: a
nonce is two engine runs (~280) plus eight unavoidable H loads (~260)
plus the stores. The stores are posted and cost the CPU little, but the
trigger behind them lands only once the bus has drained them (the
difference between 283 and 165 above).

## The variants (2048 nonces each, every H[7] checked)

| # | Variant | Cycles a nonce | kH/s | Wrong |
|---|---|---|---|---|
| 0 | the loop as shipped, in C | 886–890 | 270 | 0 |
| 1 | the second block's padding written right after CONTINUE, the next nonce's first block right after START | **738–741** | **324** | 0 |
| 2 | variant 1 with the digest read in one go (8 loads, then 8 stores) | 783 | 306 | 0 |
| 3 | variant 1 with a counted wait (the drained engine time) before the first BUSY read | 741 | 324 | 0 |
| 4 | 2 and 3 together | 783 | 306 | 0 |
| — | both hashes by DMA (two-block DMA_START over the header, no H restore; the digest block by DMA) | 1135 | 211 | 0 |
| — | first hash as variant 1, second by DMA, first block prepared during the DMA hash | 894 | — | 2047 (the DMA overwrites TEXT) |

Why the others lose: grouped loads take away the overlap a load-store
pair gets (the posted store runs while the next load waits); polling
costs nothing the counted wait saves, since one BUSY read follows anyway;
the DMA path pays a descriptor fetch and the bus transfer for every
block (~120 cycles each) and needs TEXT for itself.

What is left in variant 1: 8 H stores + CONTINUE (~100 on the bus),
engine (~140), 8 H loads with their stores (~260), START, engine (~140),
H[7] load (31), the loop. The eight loads and the two engine runs are
the floor, ~700 cycles; the rest is a few percent.

## What shipped

Commit 951c619: `hw_batch` writes the next block's words while the engine
runs (`HW_PIPELINED`). Each batch's first nonce is re-hashed in software;
a contradiction counts in `cpuMiner.hwSpotBad`, turns the pipelining off
for good and the loop goes back to the order it first shipped in
(`cpuMiner.hwPipelined`). Candidates are re-hashed as before
(`hwMismatch`). The host test
`pipelined_write_sets_leave_the_right_blocks` models the TEXT register
file across nonces: each block only rewrites what the other changed.

Commit 50f6a11: the loop in C (`wa_sha256d_hw_scan`, IRAM, `-O2`), the
register helpers shared with the bench (`warpaxe_sha_regs.h`): the Rust
loop did 308 kH/s at the probe where the C one does 324.

Probe (one batch alone at priority 20, 4096 nonces): peripheral worker
268.2 → 308.3 kH/s (Rust loop, 951c619), software worker 31–34 kH/s
unchanged.

## The 30-minute runs

Before (v0.1.28 72b20dd, 26 h up, 2026-10-15 09:3x CEST): `khs10m` 292.3,
peripheral 262–266, software 28.7–29.7, `hwMismatch` 0, die 58.6 °C.

After: 951c619 from 09:46 CEST (sampled 09:47–10:19), 50f6a11 from
10:21 CEST (sampled 10:22–10:53), one line a minute from
`/api/system/info`.

| | Before (72b20dd, Rust loop as shipped) | 951c619 (pipelined, Rust loop) | 50f6a11 (pipelined, C loop) |
|---|---|---|---|
| `khs10m` (10-minute window) | 292.3 | **329.5–331.7** from minute 12 on (+13 %) | **337.0–339.8** from minute 12 on (+16 %) |
| peripheral worker in service | 262–266 | 300–308 | 294–315, mostly 308–312 |
| software worker in service | 28.7–29.7 | 29.2–30.5 | 28.3–30.7 |
| probe (alone, priority 20) | 269.5 / 33.6 | 308.3 / 33.6 | 313.4 / 30.5 |
| µs a hash in the batches | 3.88 / 33 | 3.20–3.45 / 32–35 | 3.14–3.46 / 30–36 |
| `busyPct` | 100 / 99 | 99–100 / 96–100 | 98–100 / 98–100 |
| `hwMismatch` | 0 | 0 | 0 |
| `hwSpotBad` | — | 0 (pipelining stayed on) | 0 |
| shares in 32 min | — | 11 accepted, 0 rejected | 36 accepted, 0 rejected |
| die temperature | 58.6 °C | 58.6–59.6 °C | 58.6 °C throughout |
| internal heap | 58.1 KB | 55.3–57.1 KB, stable | 52.5–56.6 KB, stable |
| Wi-Fi | −65 dBm | −57…−68 dBm | −63…−64 dBm |

Minute by minute (UTC, 2026-10-15): `khs10m` 301 → 322 → 330 as the
window filled, then 329.5–331.7 for the remaining 20 minutes; one minute
(08:15) with the peripheral worker at 286.6 and 10.3 µs a hash in one
batch (a preemption on core 1), the next minute back at 304.

## What did not work, and why

- **DMA-SHA**: slower than the register path (399 cycles a block against
  ~280 for H restore + CONTINUE) and it needs TEXT, so nothing can be
  prepared while it runs. Dead for this loop.
- **Counted waits instead of polling**: no gain; the engine time varies
  with the store backlog before the trigger, and one BUSY read is needed
  anyway.
- **Loads grouped before stores**: slower by 40 cycles a nonce.
- **The register traffic itself**: the stores are already posted, the
  loads are the bus's latency. There is no second H register file, so
  two nonces cannot overlap on the engine: the digest of one must be
  read before the next block can start.

## What is left

- The software worker on core 0 (29–30 kH/s in service, 34 alone): a
  hand-written Xtensa SHA-256 was estimated at +15–25 % of that worker,
  so +5–8 kH/s of the total; the owner set it aside on 2026-10-07.
- NMMiner's 384.5 kH/s: with ~320 from the peripheral and ~30 from the
  software, WarpAxe lands at ~350. The remaining 10 % would have to come
  from a software loop two to three times faster than ours, or from
  something NMMiner does that is not visible from the outside (closed
  source, not disassembled by agreement).

## Rules kept

- Every candidate re-hashed in software, `hwMismatch` 0 throughout; a
  software spot check every batch, `hwSpotBad` 0 throughout.
- The crypto lock is held per batch and let go for every candidate and
  between batches: mbedTLS (the online update's TLS, the signature
  check) ran its update check on the new image without error.
- The bench frees its GDMA channel after the run. The bench endpoint
  stays on the branch as diagnostics; whether it ships, or behind a
  switch, is decided at the merge.
- No change to any board with chips: the loop only runs under
  `board::devkit()`.

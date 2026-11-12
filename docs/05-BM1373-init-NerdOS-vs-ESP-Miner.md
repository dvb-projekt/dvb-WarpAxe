# BM1373: NerdOS's init against ESP-Miner's, for the NerdAxe Gaia

Written 2026-10-10 for the Gaia (`192.168.178.40`, one BM1373 on a
TPS546, 12 V carrier, 380 MHz / 960 mV under WarpAxe v0.1.24). This is
step 1 of the handoff: the whole bring-up of the three firmwares side by
side, read from source, nothing run on a board. Step 2 (an A/B on the
Gaia) only happens with Oliver's OK.

Why: the Gaia's chip error counter sits at 3–6 % (3.3 % at 350 MHz /
960 mV, 6.2 % over the 24 h run of 07./08.10. at 300–380 MHz / 930–960
mV), and those errors cost pool-side hashrate about 1:1 (94.0 % of
nominal against the Gamma's 100.0 %, 6187 shares). More voltage is out:
the core draws 31–33 A of the regulator's 33 A. On the NerdOctaxe one
core trim (ClockDelayCtrl 0x0C instead of 0x10) was the difference
between dead domains and clean starts (`docs/04`), so the init is the
next place to look.

## The three sources

| | Source | Init read | Board it serves |
|---|---|---|---|
| **WarpAxe v0.1.24** | `crates/core/src/chip/bm1373.rs` (`init_source: "NerdOS"`), the driver tail in `crates/chain/src/driver.rs` (`finish_steps`, `finish_ramp`), the bring-up in `firmware/warpaxe/src/board.rs` (`start_tps546_nerd`) | — | NerdAxe Gaia, 1 chip (runs since 2026-09-29) |
| **NerdOS** | shufps/ESP-Miner-NerdQAxePlus `develop` 762092d, `components/bm1397/bm1373.cpp` + `asic.cpp`, board `main/boards/nerdaxegaia.cpp` | 927407d, 2026-06-20 ("New bm1373 init sequence", #647, co-author Pmaxsd); unchanged since | Q1373 (4 chips, TPS53647/53667) and the Gaia (1 chip) |
| **Gaia stock (BitMaker)** | the firmware the reference board shipped with, `v1.1.0-31-gae509770`: commit ae50977 (2026-09-03) of the **same** repository | `bm1373.cpp` at ae50977 is byte-identical to 762092d | NerdAxe Gaia |
| **ESP-Miner** | bitaxeorg/ESP-Miner `master` a31e084 (2026-10-09), `components/asic/bm1373.c`, `asic_init.c`, `frequency_transition_bmXX.c`, `pll.c` | 5c27713, 2026-08-22 ("Add BM1372/BM1373 ASIC driver support", #1890); since then only the 0x28/0xC4 writes moved from `set_max_baud` into `init` and the mask constant was renamed; a31e084 adds domain counters 5–8 to the readout | **Bitaxe Naja Duo**: 2 chips in 2 series voltage domains on 12 V, stacked TPS546, stock 327 MHz / 1000 mV, difficulty 256 |

So the "third column" from the handoff collapses into the second: the
Gaia's stock init **is** NerdOS's, and WarpAxe's profile is a transcript
of it (the render test in `bm1373.rs` pins the byte sequence). The
comparison is NerdOS/WarpAxe against ESP-Miner.

Before the init, all three: core rail at the bring-up voltage (NerdOS
and WarpAxe: max(1000 mV, configured) on the Gaia, ESP-Miner: the
configured voltage), reset released, host at 115200. The Gaia's reset
timing under WarpAxe is the stock firmware's (LDO never cut, 500 ms,
reset low 350 ms, rail, 500 ms, release, 250 ms); ESP-Miner holds reset
100 ms and waits 100 ms after it, then 20 ms for the UART.

## Step by step, on the wire

`all` = `CMD_WRITE` to `GROUP_ALL`, `chip n` = `CMD_WRITE` to one chip
(the BM1373 takes single-chip commands at address ÷ 4). Register
bytes as they go out. NerdOS sends its frames back to back (about 1 ms
each at 115200); ESP-Miner waits 10 ms between steps and retries a
failed UART write three times.

| # | NerdOS = Gaia stock = WarpAxe v0.1.24 | ESP-Miner a31e084 (Naja Duo) | Same? |
|---|---|---|---|
| 1 | `A4 ← 80 00 FF FF` all, **four times**: the BIP 320 mask *without* the enable bit | — | differs |
| 2 | read `00` all; count the 9-byte `AA 55 13 72 …` replies (1 s window) | read `00` all; count (1 s per reply, 0x1372 or 0x1373 accepted); **must equal the configured count**, else the init fails and `asic_init.c` retries it up to three times after a 10 ms reset | same read; ESP-Miner strict on the count |
| 3 | `A4 ← 80 00 FF FF` all | — | differs |
| 4 | `A8 ← 00 07 00 00` all (soft reset control) | — (see 15) | differs |
| 5 | `18 ← FF 00 C1 00` all (misc control) | — (see 16) | moved |
| 6 | chain inactive; addresses 0, 16, 32, … | chain inactive; 10 ms; addresses 0, 16, …; 10 ms | same |
| 7 | — | `3C ← 80 00 8D 04` all: **core register 0x0D ("nonce bin overflow ctrl") = 0x04 ("disable")**, the first write after addressing ("match the stock BM1372 setup order") | **new** |
| 8 | `3C ← 80 00 80 0C` all: **core register 0x00 (clock delay ctrl) = 0x0C** | — (see 19) | **differs in value** |
| 9 | `14 ←` ticket mask for the board's difficulty: Gaia stock 2048, WarpAxe 4096 (its profile default, re-written if the pool asks for less once VarDiff settles) | `14 ←` ticket mask, 256 on the Naja Duo | same register, board values |
| 10 | `58 ← 00 01 11 11` all (IO driver strength) | `58 ← 00 01 11 11` all | same |
| 11 | — | `58 ← 00 01 F1 11` **to the last chip of each voltage domain** (Naja Duo: both chips; on the Gaia it would be the only chip) | **new** |
| 12 | `68 ← 5A A5 5A A5` all (PLL3 / ROSC pad disable) | `68 ← 5A A5 5A A5` all; 10 ms | same |
| 13 | — | `C4 ← 00 00 00 00` all (auto work configuration) | **new** |
| 14 | — | `28 ← 80 00 00 00` all; **host to 3 Mbaud** (after `uart_wait_tx_done`); buffer cleared; 10 ms | **differs**: NerdOS switches last, to 1 Mbaud (see 24) |
| 15 | per chip: `A8 ← 00 07 01 F0` | `A8 ← F0 07 01 F0` all ("fast" soft reset); **20 ms** | differs: value, broadcast, position (after the baud switch) |
| 16 | per chip: `18 ← FF 00 C1 00` | `18 ← FF 00 C1 00` all; 10 ms | same value, broadcast only |
| 17 | — | `19 ← 00 00 00 00` all (misc control add, "CRR disable"); 10 ms | **new** |
| 18 | — | `3C ← 80 00 8B 00` all: **core 0x0B (clock select ctrl) = 0x00**; 10 ms | **new** |
| 19 | per chip: `3C ← 80 00 80 0C` (core 0x00 = 0x0C again) | `3C ← 80 00 80 5A` all: **core 0x00 = 0x5A** (ESP-Miner's name: `CLOCK_DELAY_PWTH6_CCDLY2`); 10 ms | **differs: 0x0C vs 0x5A** |
| 20 | per chip: `3C ← 80 00 82 AA` (core 0x02, core enable) | `3C ← 80 00 82 AA` all; 10 ms | same value, broadcast only |
| 21 | `54 ← 00 00 00 02` all (analog mux: temperature diode) | `54 ← 00 00 00 02` all | same |
| 22 | ramp: 56.25 MHz, then +6.25 MHz every 100 ms to the target, each step one `08 ←` PLL word to all | ramp from 50 MHz: 56.25, 62.5, … every 100 ms, same grid | same grid; **PLL words differ**, see below |
| 23 | `10 ← 00 00 1E B5` all (hash counting number; NerdOS calls it the "version rolling frequency", user-settable, default 0x1EB5) | `10 ← 00 00 1E B5` all (fixed: "known-good S21 Pro stock value") | same |
| 24 | `A4 ← 90 00 FF FF` all: version rolling **enabled** | `A4 ← 90 00 FF FF` all | same |
| 25 | `28 ← 11 30 02 00` all; NerdOS waits 500 ms, host to **1 Mbaud**, clears, waits 500 ms. WarpAxe: the same word, 500 ms, host to 1 Mbaud (after TX done + 20 ms) | (`asic_init.c` sets the host to 3 Mbaud once more, no new chip write) | differs: rate and position |
| 26 | core rail to the configured voltage (NerdOS: the power task; WarpAxe: `chain_ready`) | already there | — |

Not in ESP-Miner at all: the five mask writes without the enable bit,
the `A8 ← 00 07 00 00` broadcast, and every single-chip write (its whole
init is broadcast, plus the one domain-end write). Not in NerdOS: core
registers 0x0D and 0x0B, misc control add 0x19, auto work 0xC4, the
domain-end IO strength.

### The PLL words at the Gaia's clocks

Both drivers ramp on the same 6.25 MHz grid, but they solve the divider
differently. ESP-Miner (and WarpAxe, `Solver::EspMiner`, verified
against the C) takes the closest frequency first, then the lowest VCO;
NerdOS takes any solution within 2 MHz with the smallest post-divider
product (`Asic::sendHashFrequency`). Feedback range 160–239 in both.

| Target | WarpAxe = ESP-Miner | VCO | NerdOS = Gaia stock | VCO | Real clock |
|---|---|---|---|---|---|
| 300 MHz | `40 A8 02 60` | 2100 MHz | `40 A8 02 60` | 2100 MHz | 300.00 both |
| 320 MHz | `50 C0 01 42` | **4800 MHz** | `40 B3 02 60` | 2238 MHz | 320.00 vs 319.64 |
| 340 MHz | `50 CC 01 42` | **5100 MHz** | `40 A3 02 50` | 2038 MHz | 340.00 vs 339.58 |
| 350 MHz | `40 A8 02 50` | 2100 MHz | `40 A8 02 50` | 2100 MHz | 350.00 both |
| 360 MHz | `50 D8 01 42` | **5400 MHz** | `40 AD 02 50` | 2162 MHz | 360.00 vs 360.42 |
| 375 MHz | `40 B4 02 50` | 2250 MHz | `40 B4 02 50` | 2250 MHz | 375.00 both |
| **380 MHz** | `50 E4 01 42` | **5700 MHz** | `50 D5 02 60` | 2662 MHz | 380.00 vs 380.36 |
| 400 MHz | `40 A0 02 40` | 2000 MHz | `40 A0 02 40` | 2000 MHz | 400.00 both |
| 420 MHz | `40 A8 02 40` | 2100 MHz | `40 A8 02 40` | 2100 MHz | 420.00 both |

At the Gaia's operating point, 380 MHz, WarpAxe runs the PLL with
refdiv 1 and a 5.7 GHz VCO where the stock firmware runs refdiv 2 and
2.66 GHz. The same at 320, 340 and 360 MHz. This is not an init-list
item, and it does not explain the error rate on its own: at 350 MHz,
where the words are identical, the counter read 3.3 % at 960 mV as
well. But it is a difference in the flow at exactly the clocks the 24 h
measurement ran, and it is cheap to test separately (run at 350 or 400
MHz, or offer NerdOS's solver as an option).

### What the error-rate candidates are

1. **Core register 0x00, clock delay: 0x0C against 0x5A.** The one
   register that decided the Octaxe case (`docs/04`, there 0x0C against
   0x10 on the BM1370). A clock/data delay trim is exactly the kind of
   setting that moves a core's timing margin, and a margin problem is
   what an error rate that falls with voltage looks like. ESP-Miner's
   constant name suggests two fields (a pulse width and a clock delay);
   the bit layout is documented in none of the three sources.
2. **Core register 0x0D = 0x04, "nonce bin overflow disable".** New,
   first write after addressing, and by its name about nonce
   bookkeeping. Beware: it may change what the chip counts as an error
   (register 0x90) without changing what reaches the pool. The A/B must
   be judged pool-side, not by `errorPercentage1m` alone.
3. **Core register 0x0B = 0x00, clock select.** New. Unknown what the
   power-on value is; if it is already 0, the write does nothing.
4. **IO driver strength `00 01 F1 11` on the domain-end chip.** On the
   Gaia the only chip. Pad drive of the chip's outputs: more likely to
   matter for the UART at 3 Mbaud than for the hash cores.
5. Misc control add 0x19 = 0 and auto work 0xC4 = 0: presumably the
   power-on defaults, kept for completeness.
6. The "fast" soft reset `F0 07 01 F0` as a broadcast after the baud
   switch, against NerdOS's `00 07 00 00` broadcast plus `00 07 01 F0`
   per chip before anything else.
7. Outside the lists: the PLL word (above) and the ticket mask (stock
   2048, WarpAxe 4096; the chip's error counter does not depend on it).

Candidates 1–3 come as one block in ESP-Miner (0x0D first, then 0x0B
and 0x00 after the reset); the source for the values is ESP-Miner's
"stock BM1372 setup order", i.e. a Bitmain dump. Nobody has published
what the three registers do.

## Risks of running ESP-Miner's init on the Gaia

- **3 Mbaud before the ramp, and before the core registers.** The Gaia
  run of 2026-09-29 that never hashed (35609a6) had the host at 1 Mbaud
  (`11 30 02 00`) before the ramp with no settle time and NerdOS's
  registers; moving the switch after the ramp fixed it (4d9fb41). That
  is not the same thing as ESP-Miner's order (a different UART word, a
  "fast" soft reset and the core registers *after* the switch, 10–20 ms
  between steps), and the Naja Duo comes up this way on the same
  ESP32-S3. But it is the one sequence that has never run on a Gaia.
  WarpAxe's host-side switch waits for TX done plus 20 ms, ESP-Miner's
  for TX done only; that can only help.
- **The chip count.** ESP-Miner fails the init when the count differs
  from the configured one; WarpAxe only reports it. Keep ours.
- **The error counter's meaning may change** with core 0x0D (see
  candidate 2). Measure pool-side rate (Σ share difficulty × 2³² / t)
  alongside the counter, as the handoff says.
- **Silent chain.** If the ESP-Miner init brings the chain up at 3 Mbaud
  and it then goes quiet, the silent-chain watchdog re-inits; the
  fallback to NerdOS's init must be part of the image, so one bad
  variant cannot strand the board.
- Regulator: the voltages and clock of the A/B are the ones the board
  runs today (380 MHz / 960 mV, 31 A of 33 A). Nothing in the init
  changes the rail; the core current at a given error rate may move a
  little, so log it.

## What step 2 needs in the code

Pattern: `BM1370_NERD` in `bm1370.rs`, chosen in `board.rs` `chip()`.
For a `BM1373_ESP` profile the lists can carry everything except two
things the step set does not have today:

- **A baud switch in the middle of the list.** `baud_switch_after_ramp_ms
  = None` switches after the lists, `Some(ms)` after the ramp; ESP-Miner
  switches between the IO-strength block and the soft reset. One new
  `InitStep` (write the profile's UART word, move the host, clear) does
  it; the simulator takes the profile's `fast_uart` tuple, so
  `(80 00 00 00, 3_000_000)` works there without a change.
- **Version rolling after the ramp without a baud switch.** The driver
  writes the enabled mask after the ramp only on the `Some(ms)` path.
  Either a profile flag for that tail, or the enabled mask as the last
  list step before the ramp (ESP-Miner writes it after the HCN; the
  order relative to the ramp is unlikely to matter, but then the flow is
  no longer the reference's).
- The domain-end write: `WriteChip` in `init_per_chip` reaches every
  chip, which on the Gaia (one chip, one domain) and the Naja Duo (two
  chips, two domains) is the same set; a real "last chip per domain"
  step is only needed for a multi-chip single-domain board.
- The switch: an NVS key (`chipinit` = `nerdos` | `espminer`, default
  `nerdos`), read in `chip()` and acted on **only for the BM1373**; the
  fallback: a chain that stays silent after the ESP-Miner init is
  re-inited with NerdOS's, logged and shown. The log line at chain start
  names the init that ran ("NerdOS init" / "ESP-Miner init", as on the
  BM1370), so every sample of the A/B can be attributed.

## Step 2, the build (2026-10-10, on the branch, not flashed)

Built after Oliver's OK (via dvb-WarpAxe #2), host gates green, ESP
image built, nothing flashed:

- `BM1373_ESP` in `crates/core/src/chip/bm1373.rs`: ESP-Miner's lists
  step for step (the render test pins them item for item, delays and
  the 3 MBaud switch included); everything else is `BM1373`.
- `InitStep::SwitchBaud` renders as the 0x28 write plus
  `Rendered::HostBaud`; the driver moves the host there, resets the
  decoder, skips its own switch after the lists and writes the enabled
  version mask after the ramp and the HCN (ESP-Miner's tail). NerdOS's
  path is untouched (`baud_switch_after_ramp_ms = Some(500)` as before).
- Simulator: one chip under `BM1373_ESP` comes up, moves to 3 MBaud
  with nothing lost to a rate mismatch, lands the reference's register
  values (0x28, 0x10, 0xA4, 0x18, 0x58, 0xA8) and hashes.
- Setting `chipInit` (`nerdos` | `espminer`, default `nerdos`), PATCH
  `/api/system`; read at every chain start (chips off/on or a restart
  applies it); BM1373 only. `/api/system/info` reports `chipInit`
  (`NerdOS`, `ESP-Miner`, `NerdOS (fallback from ESP-Miner)`), the
  chain-start log line names the init.
- Fallback: an init that fails (no chip answers) or a chain that
  answers the id read but never produces a nonce before the 60 s
  all-silent restart puts NerdOS's init back until the next boot
  (setting `nerdos` again also clears it). A chain that hashed and then
  hangs restarts with the same init, as before.

## Result of the A/B (2026-10-10, the Gaia on cd58c79)

Run by dvb-WarpAxe #2 after Oliver's OK: the Gaia at 380 MHz / 960 mV,
WarpTune in dry run, `asicDifficulty` 256, blocks NerdOS, ESP-Miner,
NerdOS, ESP-Miner of 70 min each, the first 10 min of every block
dropped. Before the first ESP-Miner block the log read "chain start …
ESP-Miner init", the chip answered, the chain was ready after ~6.5 s,
`chipInit` read `ESP-Miner`: **3 MBaud before the ramp works on the
Gaia.** No fallback in any block. Valid rate = (Δ`nonces` −
Δ`below_ticket`) × 256 × 2³² / Δt against 6860 × 380 MHz = 2607 GH/s.

| Block | Init | Valid nonces / h | Valid rate | Of nominal | Chip error counter Ø |
|---|---|---|---|---|---|
| 1 | NerdOS | 8122 | 2516.6 GH/s | 96.5 % | 3.64 % |
| 2 | ESP-Miner | 8257 | 2558.3 GH/s | 98.1 % | 3.64 % |
| 3 | NerdOS | 8258 | 2558.9 GH/s | 98.2 % | 3.60 % |
| 4 | ESP-Miner | 8064 | 2498.7 GH/s | 95.9 % | 3.63 % |

`below_ticket` 0.00 % everywhere; core 30.9 A and chip 62.0 °C in every
block. Means: NerdOS 97.35 %, ESP-Miner 97.0 %, a difference of −0.35
points at ±1.1 % per block. **No measurable effect**, on the delivered
rate or on the chip's own error counter: core 0x0D, core 0x0B, core
0x00 at 0x5A, the domain-end IO strength and the 3 MBaud order change
nothing about the ~3.6 % errors. The ~2.7 % gap to the nominal rate
matches the error counter, as in the 24 h run. The raw log (one line
per minute) is in the dvb-WarpAxe session's scratchpad, `gaia-ab/ab.jsonl`.

What this settles: the error rate is not a matter of the init. What is
left is the chip at this voltage and clock (the counter falls with
voltage, and the regulator is at its limit), and, untested, the PLL
word at 380 MHz. Test B was skipped: with no effect from the bigger
change there is no reason to expect one from the smaller, and the
08.10. chainwatch log already put 380 MHz below 350 MHz.

## Recommendation (after the A/B)

- **The Gaia's default stays NerdOS's init**, which is also the stock
  firmware's. The Gaia runs it again with the 4096 ticket mask.
- The `chipInit` switch and `BM1373_ESP` have done their job. Only
  this document went to main (Oliver, 2026-10-10); the code path stays
  on the branch `claude/bm1373-init-ab-handoff-ec3720` (cd58c79) for
  reference, so main has no unused init.
- In short: ESP-Miner's BM1373 init brings the Gaia up and hashes at
  the same rate, but it does not move the BM1373's error counter; the
  ~3.6 % at 380 MHz / 960 mV are the chip's at that point, not the
  init's.

## The plan before the A/B (kept for the record)


Do step 2, with Oliver's OK and one flash. The candidates 1–3 are the
same kind of core trim that fixed the Octaxe, the Naja Duo proves the
sequence on the same chip and MCU, and one image with a setting and a
fallback is enough. Order:

- **A: ESP-Miner's init as it is** (3 Mbaud before the ramp) against
  NerdOS's, alternating, at 380 MHz / 960 mV, WarpTune in dry run. If
  the chain does not come up or does not hash: **A′** with ESP-Miner's
  registers and our proven tail (1 Mbaud after the ramp, 500 ms), which
  isolates the registers from the UART order.
- **Enough samples per block, from the host's own count.** At ~2.6 TH/s
  the pool side gives the Gaia ~150–280 shares an hour (±8 % per hour;
  the 24 h run needed 6187 shares for ±1.3 %), and a lower pool
  difficulty is out: the Gaia mines over Stratum V2, where
  `suggest_difficulty` is not read, and over V1 the pool floors at 1024
  with VarDiff pulling back to ~12 k (Warppool #2, 10.10.). So the
  measure is the firmware's own re-hashed nonces at a low ticket mask:
  `asicDifficulty = 256` (PATCH, both inits set it on the same path)
  gives ~8500 nonces an hour, ±1.1 %. **Not** `hashRate`/`hashRate_10m`:
  that figure credits every reported nonce, including the ones that fail
  the re-hash. Use the cumulative counters in `/api/v1/state`:
  valid = Δ`miner.nonces` − Δ`miner.below_ticket`, rate = valid × 256 ×
  2³² / Δt, against small cores × clock. `below_ticket / nonces` per
  init is the chip's error share as the host sees it. 256 is safe on
  the BM1373: ~2.4 nonces a second, 11 bytes each, one sha256d per nonce
  on the host; the Gamma runs 256 by default. `errorPercentage1m` is only
  the quick early hint within one init, since core 0x0D may change what
  the chip counts.
- **B, the PLL word, only if A brings nothing.** The chainwatch log of
  08.10. has 350 MHz / 960 mV at 3.3 % (identical word) and 380 MHz /
  960 mV at 1.6 % (six samples only), which looks like clock and
  temperature rather than the word. If it comes to it: 350 and 400 MHz
  (identical words in both solvers) against 380 MHz, all at 960 mV,
  pool-side rate per clock against expectation; if 350/400 are clearly
  below 380, offer NerdOS's solver for the BM1373.
- **Blocks:** 1 h per init, alternating, twice each, at 380 MHz /
  960 mV with WarpTune in dry run; the first 10 min after each switch
  dropped (ramp, temperature). Every sample carries `chipInit` from
  `/api/system/info` (it must read `ESP-Miner`, not the fallback),
  clock, core voltage and current, chip temperature, fan. Before the
  first block: after `chipInit = espminer` and chips off/on, the log
  says "chain start … ESP-Miner init", the chain hashes and
  `miner.nonces` climbs. Pool shares (`stratum.shares_accepted`) over
  the whole run as the cross-check. Chainwatch logger in
  `WarpForge-Firmware/messdaten-2026-10/` as the template.

The Gaia's default stays NerdOS's init until the numbers say otherwise;
that is Oliver's call after the A/B. The result goes to Bitronics and
shufps (drafts in `Mail-Gaia-Fehlerrate.md`).

## The Naja Duo: ESP-Miner's init by board (2026-11-12, not yet on a device)

The Bitaxe Naja Duo is the board ESP-Miner's BM1373 init was written
for: two BM1373 in two voltage domains, chip 0 reaching chip 1 through
a discrete level shifter (TLV3544), which is what the IO strength
`00 01 F1 11` for the last chip of each domain is there for. So the
code path of step 2 came back from `keep/bm1373-espminer-init`
(cd58c79) onto main, and the choice moved from the setting to the
board:

- `board::chip()` picks the init **by board**: the Naja Duo starts with
  `BM1373_ESP` (3 MBaud in the middle of the list, as ESP-Miner), every
  other BM1373 board (the Gaia) with `BM1373` (NerdOS's, as before and
  as its stock firmware; the Gaia's path is unchanged).
- A **ladder** behind it: ESP-Miner 3 MBaud → ESP-Miner 1 MBaud →
  NerdOS. `BM1373_ESP_1M` is ESP-Miner's register writes with NerdOS's
  pace (the lists leave the UART at 115200, the chips move to 1 MBaud
  500 ms after the ramp), derived from the 3 MBaud list so the two
  cannot drift; it is the fallback should 3 MBaud not hold across the
  Naja Duo's two level shifters. A chain that does not come up (no chip
  answers, or no nonce before the 60 s all-silent restart) moves one
  rung down until the next boot; the log says which init left and
  which comes, `/api/system/info` `chipInit` reads e.g.
  `ESP-Miner 1 MBaud (fallback from ESP-Miner)`.
- The `chipInit` setting now defaults to the board's init (empty) and
  takes `espminer`, `espminer-1m` or `nerdos` to start at a rung by
  hand (for the bench: the 1 MBaud variant without a reflash). Read at
  every chain start; a changed setting forgets the fallbacks.
- The Naja Duo's copies of the three profiles carry its maker's
  **6725 small cores** (`Profile::small_cores`, ESP-Miner's figure, the
  reseller's 4.4 TH/s at 327 MHz); the Gaia keeps NerdOS's 6860.
- Simulator: two BM1373 under `BM1373_ESP` enumerate at addresses 0
  and 16, both land the domain-end strength, the chain moves to 3 MBaud
  with nothing lost, both chips hash and their nonces are attributed to
  them (the simulator stamps ESP-Miner's address bits, `index × 128`);
  the same for `BM1373_ESP_1M` with the switch after the ramp.

What the device has to answer (the first run): whether both chips come
up under the 3 MBaud init, and how the nonces' address bits fall
(`GET /api/v1/chip/cores`: `addressBits` lists every value of
`(nonce >> 17) & 0xFF` seen and the chip it was credited to; on a
two-chip board they should fall into 0–127 for chip 0 and 128–255 for
chip 1, each about half, with `hashrateMonitor` showing both chips'
counters). Until then the silent-chip watchdog on the Naja Duo only
reports (`Profile::silent_chip_restart` false).


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
  `nerdos`), read in `chip()`; the fallback: a chain that stays silent
  after the ESP-Miner init is re-inited with NerdOS's, logged and shown.

## Recommendation

Do step 2, with Oliver's OK and one flash. The candidates 1–3 are the
same kind of core trim that fixed the Octaxe, the Naja Duo proves the
sequence on the same chip and MCU, and one image with a setting and a
fallback is enough. Order:

- **A: ESP-Miner's init as it is** (3 Mbaud before the ramp), 1 h on /
  1 h off against NerdOS, twice each, at 380 MHz / 960 mV, WarpTune in
  dry run. If the chain does not come up or does not hash: **A′** with
  ESP-Miner's registers and our proven tail (1 Mbaud after the ramp,
  500 ms), which isolates the registers from the UART order.
- **B, separately and cheap:** the PLL word. One hour at 350 MHz and
  one at 400 MHz (identical words in both solvers) against 380 MHz, all
  at 960 mV, pool-side rate per clock against expectation. If the error
  counter at 350/400 is clearly below the 380 reading, offer NerdOS's
  solver for the BM1373.
- Judge by pool-side rate and the counter together, with core current
  and temperature logged (chainwatch logger in
  `WarpForge-Firmware/messdaten-2026-10/` as the template).

The Gaia's default stays NerdOS's init until the numbers say otherwise;
that is Oliver's call after the A/B. The result goes to Bitronics and
shufps (drafts in `Mail-Gaia-Fehlerrate.md`).

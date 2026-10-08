# NerdNOS (BM1397) under WarpAxe, against WantClue's firmware and ESP-Miner

Written 2026-10-08, before anything ran on the chip. The device is
Oliver's NerdNOS (`192.168.178.46`, stock firmware "NerdNOS" 2.0.5, a
NerdMiner fork by WantClue). The references:

* **WantClue** — github.com/WantClue/NerdMiner_v2, branch `nerdnos`
  (head 22588347, 2026-05-04): `src/drivers/nerd-nos/{bm1397.cpp,
  nerdnos.cpp, nerdnos.h, adc.cpp, serial.cpp, utils.cpp}`,
  `src/mining_nerdnos.{cpp,h}`. What the .46 runs today.
* **ESP-Miner** — github.com/bitaxeorg/ESP-Miner, `components/asic/bm1397.c`
  (v2.15.3), `bm_job_packet.c`, `bm_job_midstate.c`, `main/device_config.h`
  and the tests `test_bm_job_packets.c`, `test_bm_job_packet_building.c`,
  `test_asic_result_job.c`, `test_job_command.c` (master, 2026-10). The
  Bitaxe Max's driver; both derive from cgminer's `driver-gekko.c`.
* **Hardware** — github.com/benjamin-wilson/NerdNOS (KiCad: `bm1397.kicad_sch`,
  `power.kicad_sch`, `T_Display_S3.kicad_sch`, `NerdNOS.kicad_pcb`).

WarpAxe's side: `crates/core/src/chip/bm1397.rs`, `job.rs` (`MidstateJob`),
`result.rs` (`parse_bm1397`), `pll.rs` (`Solver::Gekko`), `sha256.rs`;
`crates/chain/src/driver.rs`; `crates/board/src/profile.rs` (`NERDNOS`),
`ntc.rs`, `detect.rs` (`bare_board`); `firmware/warpaxe/src/{board.rs,
ntc.rs, miner.rs}`.

## The whole flow, step by step

| # | Step | WantClue (the .46) | ESP-Miner (Bitaxe Max) | WarpAxe | Same? |
|---|---|---|---|---|---|
| 1 | Chip run/stop line at boot | `PEN` GPIO13 output high in `BM1397_init`, stays high | TPS546 rail off until init; reset low | GPIO13 held **low** from `quiesce()` until the chain starts (chip in reset / unpowered, like ESP-Miner's boot); high from the first `reset_chain` on | differs on purpose: the chip sits idle until the miner is ready, as on every other board |
| 2 | Reset pulse | `_reset()`: GPIO12 **high** 100 ms, **low** 100 ms, stays low (once, at init) | RST **low** 100 ms, **high** 100 ms, stays high | GPIO13 low 100 ms → high, then GPIO12 high 100 ms → low, then 100 ms (`reset_chain` + `stock_reset_pulse`), before **every** chain start | see "Reset and PEN" below |
| 3 | UART | UART1, TX GPIO17, RX GPIO18, 115200, 8N1 | 115200 | UART1 GPIO17/18 at 115200 (`uart.rs`) | yes |
| 4 | Chip count | `CMD_READ 00` to all, 9-byte replies `AA 55 13 97 18 00 …`, 1 s timeout each | same (`count_asic_chips`, length 9) | `ReadAll 00`, replies of `chip_id_reply_len` 9 collected for 1 s | yes |
| 5 | Wait | 20 ms (`SLEEP_TIME`) | 20 ms | `Delay 20` | yes |
| 6 | Addressing | `CMD_INACTIVE`, `SET_ADDRESS i·(256/n)` | same | same (`ChainInactive`, `SetAddress`) | yes |
| 7 | Register writes | `80←00000000`, `84←00000000`, `20←00000001`, `3C←80008074`, ticket mask, `68←C0700111`, `28←0600000F` | the same seven, then `18←00007A31` (default UART divider 26) | the same seven; no `18←00007A31` | WantClue's list; ESP-Miner's extra write skipped |
| 8 | Ticket mask | `BM1397_INITIAL_DIFFICULTY` at init, then the pool difficulty on every change | family difficulty 256 | 256 at init, then `min(pool difficulty, 256)` as on every chip | as ESP-Miner; WantClue follows the pool fully |
| 9 | Clock | `calc_gsf_freq` (cgminer): `70←0F0F0F00` ×2, `08←<word>` ×2, 10 ms apart; 300 MHz → `40 78 02 15`; jumped to directly | `pll_get_parameters(60..200)` + direct nibbles → 300 MHz = `40 48 02 31`; ramped (`do_frequency_transition`) | cgminer's formula (`Solver::Gekko`, `40 78 02 15` at 300), prewrite `70` ×2 then `08` ×2 per step, **ramped** from 56.25 MHz in 6.25 MHz steps every 100 ms | WantClue's word, ESP-Miner's ramp |
| 10 | Hash counting number (0x10) | never written | never written | never written (`Hcn::None`) | yes |
| 11 | Version rolling (0xA4) | none; host rolls | none | none (`host_midstates: 4`) | yes |
| 12 | Baud switch | after init: `18←00006131` (divider 1 → 1 562 500), 100 ms, host follows, 100 ms | `18←00006031` (divider 0 → 3 125 000) | after the ramp: `18←00006131`, 100 ms, host → 1 562 500 | WantClue's rate |
| 13 | Job packet | 146 B: id, 4, nonce, nbits, ntime, merkle[28..32], 4 midstates; id = `(k·4) % 128` | same layout, id `(id+4) % 128` | `MidstateJob`, ids 4, 8, …, 124, 0 | yes; byte-exact against ESP-Miner's 152-byte test vector |
| 14 | Midstate bytes | `flip32bytes(mbedtls state)` then `reverse_bytes` | `midstate_sha256_bin` then `reverse_32bit_words` | words 7..0, each big-endian | yes — all three give the same bytes (verified: ESP-Miner's packet vector and the old `91DFEA52…` midstate test) |
| 15 | Versions per job | `0x1fffe000`, `increment_bitmask` ×k, k = 0..3 | same, mask from the pool | BIP 320 mask, `increment_bitmask` ×k | yes |
| 16 | Job pacing | new job every 30 ms (`NERDNOS_JOB_INTERVAL_MS`) | every 20 ms (`default_asic_timeout`) | profile `job_interval_ms 30`, plus the nonce-space timeout (≈56 ms at 300 MHz) | WantClue's |
| 17 | Nonce reply | 9 B: nonce LE, midstate_num, id; job = `(id & 0xFC) >> 2`, midstate = `id & 3` | same (job = `id & 0xFC`) | `parse_bm1397`: job = `id & 0xFC`, midstate = `id & 3`, slot = `id >> 2` (32 slots) | yes |
| 18 | Version of a nonce | job version rolled `midstate` times | same | same (`rolled_version`) | yes |
| 19 | Register vs nonce reply | heuristic (`midstate_num == 0 && !(nonce & 0x7f)`) | bit 7 of the last byte | bit 7 of the last byte (CRC-covered) | as ESP-Miner |
| 20 | Hash-rate register | reads 0x04 every 2.5 s: `(v & 0x7fffffff) << 20` H/s, bit 31 = long window, both logged | 0x04 and 0x4C once a second; `v & 0x7fffffff` × 2²⁰ H/s, only the short window, 0x007FFFFF discarded | 0x04 and 0x4C once a second (`REGISTERS_RATE`); short window → `hashrateMonitor.asics[0].hashRate`, long → the minute figure, 0x007FFFFF discarded | yes; see "Register 0x04" below |
| 21 | Temperature | NTC on GPIO2 (ADC1_CH1), 10 k / B 3380 against 3.3 V, every 5 s | EMC2101 diode | the same divider formula (`ntc.rs`), every second, −40…150 °C plausibility | yes |
| 22 | Overheat | > 80 °C: PEN low, wait until < 70 °C, PEN high, `BM1397_init(210, 1)` | 75/90 °C, rail off | > 80 °C (or no reading for 60 s): GPIO13 **low** (reset held), ≥ 60 s, ≤ 70 °C: miner pulses the reset and re-inits at the configured clock; third stop within an hour latches | same line, same limits; WarpAxe re-inits at the configured clock, WantClue drops to 210 MHz |
| 23 | Core voltage | fixed by R12/R13 on the TPSM863257 (0.6 V · (1 + 40.2/30) = 1.40 V) | TPS546, 1100–1500 mV | reported as 1400 mV, no options, no slider | yes |
| 24 | Power / current | none | TPS546 readings | none (the tiles show "–") | yes |
| 25 | Display | T-Display-S3 (ST7789 i80) | OLED | ST7789 i80, flip from the setting (default upright) | orientation unverified |

## Reset and PEN: what the PCB says

The published PCB (`NerdNOS.kicad_pcb`) has, in `BM1397/RST → R3 100k →
RST_N → R4 120k → GND`, a 3.3 V → 1.8 V divider into the chip's **NRSTI**
(active low). Only one header line reaches the chip, and the TPSM863257's
EN pin hangs on R10 19.1k / R11 10k / C11 — no GPIO. GPIO13 ("PEN") is not
on any net of that PCB, GPIO12 ("RST") neither; the designer's header net
names (GPIO1/2/10) do not match WantClue's pins (17/18/2/12/13) either, so
the .46 is probably a later revision.

Two readings fit the fact that the .46 hashes under WantClue's firmware,
whose `_reset()` leaves GPIO12 **low**:

* (A) GPIO13 is the line into NRSTI (high = run, low = reset) and GPIO12 is
  unconnected: WantClue's "PEN high" releases the chip, its RST pulse does
  nothing, its "PEN low" on an overheat holds the chip in reset.
* (C) GPIO13 really is a buck enable and GPIO12 an active-high reset.

WarpAxe drives both lines exactly as WantClue does (13 high to run, low to
stop; 12 pulsed high then held low before each init), which is right under
either reading. **Open for Oliver**: which header pins the .46's carrier
uses, and whether its buck has a GPIO on EN.

## Register 0x04: how the value is read

The BM1397 has no hash counters; it reports a rate. The register reads
back as a 32-bit value: bit 31 names the window (0 short, 1 long), bits
30..0 the rate in units of 2²⁰ = 1 048 576 hashes per second (ESP-Miner
`HASHRATE_UNIT`, WantClue `<< 20llu`). ESP-Miner discards the value
0x007FFFFF as "no reading" and uses the short window only; WantClue logs
both. WarpAxe reads 0x04 and the error counter 0x4C once a second like the
other chips, puts the short window into the chip's rate and the long one
into its minute figure, and discards 0x007FFFFF.

| Value read | Window | Count | Rate |
|---|---|---|---|
| `0x0002EF04` | short | 192 260 | 201.6 GH/s — 300 MHz × 672 small cores, the nominal rate |
| `0x8002EF04` | long | 192 260 | 201.6 GH/s over the long window |
| `0x00023A00` | short | 145 920 | 153.0 GH/s — what the .46 showed in its API on 2026-10-08 |
| `0x007FFFFF` | — | — | no reading (ESP-Miner), ignored |
| `0x00000000` | short | 0 | 0 GH/s: no job in the chip |

Open until a real reply is seen: whether the chip alternates the two
windows on successive reads (the simulator does, short first) and how long
the "long" window is. The error counter 0x4C is read as on the BM1370.

## Gaps and open points before a flash

1. **Midstate byte order** is the one all three references compute; a wrong
   order would show up at once as `below_ticket` nonces (header re-hash
   fails) — watch that counter on the first run.
2. **Clock cap 400 MHz**, options 200–400, stock 300: nothing above has been
   measured on this USB-powered, fanless board.
3. **Display orientation** on the carrier: unknown; the flip setting covers it.
4. **Detection**: on the first start the NTC divider on GPIO2 offers
   `nerdnos` first (never taken); a bare T-Display-S3 with NerdMiner's
   settings stays the devkit. If the .46's NTC does not sit on GPIO2 after
   all, the board can still be chosen by hand.
5. **Chip id reply**: the simulator answers `13 97 18 00`; the real one must
   match (`ChipFound { chip_id: 0x1397 }` in the log).

## Before the first flash on the .46 (rules in force)

Full flash backup over USB first; committed images only; Oliver's OK and a
heads-up to Warppool #2; the preview before the flash. None of this has run
on a chip.

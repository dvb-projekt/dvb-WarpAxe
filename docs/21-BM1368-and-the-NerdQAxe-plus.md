# 21 · The BM1368 and the NerdQAxe+

Written 2026-11-22 on branch `feat/nerdqaxe-plus` (from `main` 43fe804,
v0.1.48), after the owner asked for a NerdQAxe+ profile. The NerdQAxe+ is
the NerdQAxe++'s carrier with four BM1368 instead of BM1370, so the
profile needed a chip WarpAxe had never driven. Nothing here has run on
hardware: no BM1368 is on the bench. What is tested is that the profile,
the driver and the simulator agree; whether a BM1368 agrees is for the
first board to show.

## Sources

| Short | Repository | Release | Commit |
|---|---|---|---|
| ESP-Miner | bitaxeorg/ESP-Miner | v2.15.4 | d212a0f |
| NerdOS | shufps/ESP-Miner-NerdQAxePlus | v1.1.2 | 3f73884 |
| bm13xx-rs | GPTechinno/bm13xx-rs | main | 8de5f56 |

ESP-Miner: `components/asic/bm1368.c`, `main/device_config.h`. NerdOS:
`components/bm1397/bm1368.cpp`, `components/bm1397/asic.cpp`,
`main/boards/nerdqaxeplus.cpp` (the NerdQAxe++ is `NerdQaxePlus2`, a
subclass with BM1370). bm13xx-rs has a `bm1368` crate from S21 dumps:
the best reference for register meanings. mujina (256 Foundation) has
no BM1368. No other open-source BM1368 driver was found.

## The chip (`crates/core/src/chip/bm1368.rs`)

Shared with the BM1370 and therefore no new code in the chain driver:
11-byte replies, the 82-byte job with the chip rolling the version,
job ids in steps of 24, the nonce's address bits (`256 / chips`: 0, 64,
128, 192 on four chips in both references), the PLL register and its
VCO-scale bit, the domain counters 0x88–0x8B, the switch to 1 MBaud
through 0x28 (`11 30 02 00`).

Different: chip id 0x1368; 80 cores and 1276 small cores; PLL feedback
divider 144–235 (`PllSpec::bm1368`); no hash-counting erratum;
`MISC_CONTROL` broadcast as `FF 0F C1 00`; ClockDelayCtrl 0x18; IO
driver strength `02 11 11 11`; no PLL3 write and no tail after the
per-chip block.

Two inits, as for the BM1370, chosen by the board (`board::chip()`):

- **`BM1368_NERD`, the NerdQAxe+'s.** NerdOS `BM1368::init`: version mask
  ×4, chip count, version mask, `A8` and `18` broadcast, chain inactive,
  addresses, `3C` ×2, ticket mask, `54`, `58`, the five per-chip writes
  with no pause; ramp; hash counting number fixed at 0x15A4 (NerdOS's
  default `vr_frequency`, the S21 value); the board switches to 1 MBaud
  500 ms after the ramp and waits 500 ms more; job interval 1200 ms.
- **`BM1368`, ESP-Miner's (the Bitaxe Supra's).** Chain inactive first,
  the seven broadcasts, then the addresses; 500 ms after each chip's
  block; ticket mask; ramp; computed hash counting number
  (`2³² / 128 / chips · 25 MHz / f / 2`, 213 995 for four chips at
  490 MHz); 1 MBaud right after. No board uses it yet; it is there so
  the two references' differences are written down and a Supra profile
  has its init.

Tests: the two inits write the same registers (only order and pauses
differ), the job-id sequence NerdOS lists, the PLL words for 400, 490,
500 and 575 MHz, every option of both references exact on the PLL grid,
the computed hash counting number; and against the simulator
(`crates/chain/tests/sim.rs`), four BM1368 under each init enumerate at
0/64/128/192, take their hash counting number, switch with the host
after the ramp and have every nonce attributed to its chip; a BM1368
chain under the BM1370 driver and the reverse find no chips.

## The board (`NERDQAXE_PLUS` in `crates/board/src/profile.rs`)

The NerdQAxe++ profile with: BM1368 ×4, TPS53647 on two phases (60 A,
fault 55 A: `tps53647::Config::NERDQAXE_PLUS`, already there), 490 MHz /
1250 mV, brought up at 1250 mV, 70 W (`m_maxPin`), NerdOS's option lists
(400–575 MHz, 1100–1350 mV), absolute limits 800 MHz / 1050–1400 mV,
job interval 1200 ms, a silent chip reported but not restarted (see
below). Pins, I²C, enable GPIO10, LDO GPIO13, reset GPIO1,
TMP1075 + EMC2302, fan target 55 °C, stop 70 °C, the ST7789 display and
the NerdHat pins are the NerdQAxe++'s, as in NerdOS, where the NerdQAxe++
inherits all of them from the NerdQAxe+.

The chip temperature: NerdOS takes the hotter of the TMP1075s and the
BM1368's own diode readings (register 0xB4 after four `B0` writes,
`T = raw · 0.171342 − 299.5144`) and adds no offset; the NerdQAxe++
adds 10 °C to its TMP1075 instead. WarpAxe does not read the BM1368's
diode yet and keeps the NerdQAxe++'s TMP1075 + 10 °C as the stand-in.

## How a NerdQAxe+ is found

NerdOS stores no device model in NVS (one image per board), and the
NerdQAxe+'s bus is the NerdQAxe++'s: EMC2302, TMP1075, a TPS53647 with
the same device code. So a first start offers all three boards that
look like that, NerdQAxe++ first, NerdOCTAXE-γ, NerdQAxe+ last, and the
chips stay off until someone picks one. A wrong pick is caught on the
cautious first start: the chip id (0x1368 against 0x1370) refuses it
before the full voltage, at the lowest bring-up voltage of the
siblings (1200 mV for a NerdQAxe+ picked on a NerdQAxe++).

The NerdQAxe+'s CAN extension carries an FXL6408 at 0x43, the address
the NerdQX's expander answers at: a NerdQAxe+ with the CAN extension
fitted is offered the NerdQX instead, its chip id refuses that, and a
hand pick of the NerdQAxe+ is refused by the bus check too (an FXL6408
belongs to no NerdQAxe in `choice::needs`). The NerdQAxe++ has had the
same limit all along. Take the CAN extension off for the first start;
once the board is verified, the bus check is not run again.

## Open, for the first board

1. **Counters.** ESP-Miner reads the domain counters 0x88–0x8B and the
   total at 0x8C; NerdOS reads 0x90 by broadcast; bm13xx-rs names 0x8C
   the counter and 0x90 a frequency-sweep register. WarpAxe reads
   0x88–0x8B. Check that they move with the clock on a real chip.
2. **Hash counting number.** NerdOS's fixed 0x15A4 against ESP-Miner's
   computed ~214 000. The BM1373 experiment (computed against fixed, no
   difference) suggests either works; duplicates would show it.
3. **Chip diode.** Reading 0xB4 would give the real chip temperature
   NerdOS stops on; until then the +10 °C stand-in.
4. **Per-chip clock.** `single_chip_clock` is `false` until measured:
   write one chip's PLL, read the register back from all four, watch
   whose counter follows. Then `true`, and the per-chip offsets open.
5. **Silent-chip restart.** `silent_chip_restart` is `false` on the
   NerdQAxe+ (report only) until the per-chip nonce counts on a board
   agree with its hash counters, as for the Naja Duo.

## Not done

A Bitaxe Supra or Supra Hex profile: WarpAxe now drives their chip, but
nobody has given their boards a profile, and the installer and the first
start still name them unsupported.

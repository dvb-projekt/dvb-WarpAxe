# Temperature stop limits against the stock firmware

Written 2026-10-20 (branch `fix/temp-limits`), after a Bitaxe owner found
145 °C "too high" for the regulator stop and the check showed that
WarpAxe's Bitaxe profiles carried the wrong ESP-Miner constants: the
defined-but-unused `MAX_TEMP` / `TPS546_MAX_TEMP` (90 / 145) instead of
the lines ESP-Miner actually compares against, `THROTTLE_TEMP` /
`TPS546_THROTTLE_TEMP` (75 / 105). A Bitaxe under WarpAxe up to v0.1.31
therefore stopped 15 °C (chip) and 40 °C (regulator) later than under
AxeOS. The regulator's own `OT_FAULT` at 145 °C was the only line
before that.

## What the stock firmwares do

Read from the sources, not from notes. Line numbers are the file's at the
named commit.

| Board(s) | Firmware | Chip stop | Regulator stop | Where | After the stop |
|---|---|---|---|---|---|
| Bitaxe Gamma 60x, Gamma Duo, Gamma Turbo, Naja Duo, Gamma Hex | ESP-Miner v2.15.3 (`fb31c4f`) | > 75 (`THROTTLE_TEMP`) | > 105 (`TPS546_THROTTLE_TEMP`) | `main/tasks/power_management_task.c` 16–17, 24–25 (constants), 159–163 (compare). `MAX_TEMP` 90 and `TPS546_MAX_TEMP` 145 appear nowhere else in `main/` | fans 100 %, at least 30 s (6 × 5 s), until regulator ≤ 95 (`TPS546_THROTTLE_TEMP - 10`, line 185); boards with the EMC's internal sensor (Ultra/Supra, not the Gamma) also wait for chip ≤ `SAFE_TEMP` 45 (199). Then restart at voltage − 100 mV and clock − 100 MHz (`ASIC_REDUCTION`, 210–211). No per-board values |
| BitForge Nano (800) | ForgeOS v1.7 (`f75f3ff`, github.com/wantclue/forge-os, open source; read 2026-10-23) | > 75 on either chip (`THROTTLE_TEMP`) | > 105 (`TPS546_THROTTLE_TEMP`) | `main/tasks/power_management_task.c` 21, 28 (constants), 149–151 (compare); `MAX_TEMP` 90 / `TPS546_MAX_TEMP` 145 defined, never compared (20, 29). A failed sensor reads 0.0 and so never stops the chips (`EMC2101.c` 150–181) | core voltage off, NVS set to 1000 mV / 50 MHz, fans 100 % manual, `overheat_mode` 1, then an immediate restart with no cool-down (159–166); the owner sets clock and voltage back by hand |
| NerdQAxe++, NerdQAxe++ Rev 7, NerdOctaxe-γ, Q1370, Q1373 | NerdOS v1.1.0.1 (`762092d`) | > `overheat_temp`, Kconfig default 70, range 50–90 | > `fan1_overheat`, a two-fan board inherits the chip value (70) | `main/Kconfig.projbuild` 24–27; `main/fan_controller.cpp` 89–116 (channel 0 = chips, 1 = regulator, each against its own line); `main/tasks/power_management_task.cpp` 305–314 (either channel: `setVoltage(0.0)`, board error); `main/nvs_config.cpp` 243–254 (channel 1 default). Chip reading = max(TMP1075 + 10 °C, chip-internal) on the NQ++ (`nerdqaxeplus2.cpp` 199, `power_management_task.cpp` 286) | stays off (board error until reboot) |
| NerdAxe Gamma | NerdOS v1.1.0.1 | > 70 (same `overheat_temp`) | **none**: `fan1_overheat` is forced to 0 on the one-fan NerdAxe family (`nvs_config.cpp` 246–247) and the fan loop only runs channel 0 (`board.cpp` 20 `m_numFans = 1`). `m_vr_maxTemp = TPS546_THROTTLE_TEMP` (`nerdaxegamma.cpp` 40) is a dashboard figure (`board.h` 298) | as above | stays off |
| NerdAxe Gaia | BitMaker's NerdOS at `ae509770` (what the reference board shipped with) | > 70 by code (`Kconfig.projbuild` 24–27); the reference board has 75 in NVS | **none** (one fan, `NerdAxe` family: `nerdaxegaia.h` 8, `nvs_config.cpp` 246–247); `m_vr_maxTemp` display only (`nerdaxegaia.cpp` 42) | as above | stays off |
| NerdNOS | WantClue NerdMiner_v2 branch `nerdnos` (`2258834`) | > 80 (`MAX_SAFE_TEMP`) | no regulator temperature | `src/mining_nerdnos.cpp` 51 (constant), 292 (stop), 297 (wait until ≤ 70), 303 (`BM1397_init(210, 1)`) | PEN low, back below 70, re-init at 210 MHz for good |

The TPS546's registers are the same everywhere: `OT_WARN_LIMIT` 105,
`OT_FAULT_LIMIT` 145 (`crates/board/src/tps546.rs` 306–307). 145 is the
part's own shutdown, which stays as the backstop behind every firmware
line above.

## What WarpAxe does from this branch on

`crates/board/src/profile.rs`, the fields now called what they are:
`chip_stop_c` and `vr_stop_c` (the unused `throttle_temp_c` /
`vr_throttle_c` are gone, so the two pairs cannot be mixed up again).

| Profile | chip_stop_c | vr_stop_c | Note |
|---|---|---|---|
| bitaxe-gamma, -duo, -turbo, naja-duo, gamma-hex | 75 (was 90) | 105 (was 145) | ESP-Miner's compare lines |
| bitforge-nano | 75 (was 90) | 105 (was 145) | as ForgeOS v1.7 (read from its source 2026-10-23; until then taken as ESP-Miner's) |
| nerdaxe-gamma | 70 (was 90) | 105 (was 145) | chip as NerdOS; regulator: NerdOS has no stop, WarpAxe keeps AxeOS's 105 on the same TPS546 — **deliberate deviation, Oliver's call** |
| nerdaxe-gaia | 75 (unchanged) | 105 (unchanged) | chip = the reference board's NVS, not the Kconfig 70 — kept, decided by Oliver (below) |
| nerdqaxe-plusplus, nerdoctaxe-gamma, q1370, q1373 | 70 | 70 | unchanged, NerdOS |
| nerdqaxe-plusplus-rev7 | 70 | 70 (was 105) | the 105 in the profile was NerdOS's dashboard figure, not its stop |
| nerdnos | 80 | 105 (was 145, reported only) | no regulator temperature |
| t-display-s3 | 85 | 105 (was 145, reported only) | the ESP32-S3's own line, no regulator |

Unchanged and checked against the new lines:

- Resume (`crates/board/src/overheat.rs`): both 10 °C under their line,
  at least 60 s off, the third stop within an hour latches. On the Gamma
  that is regulator ≤ 95 — AxeOS's `TPS546_THROTTLE_TEMP - 10` — and
  chip ≤ 65 (AxeOS waits for the regulator only on the Gamma, since the
  diode reads nothing with the chip off). Test
  `bitaxe_gamma_resumes_where_axeos_does`.
- AxeOS restarts at voltage − 100 mV / clock − 100 MHz after a stop;
  NerdOS stays off; WarpAxe comes back at the configured point, except a
  board without a fan to drive, which comes back one clock option lower
  (`clockStep`). Compared only, not changed.
- `overheat_temp` (the chip line as a setting, 0 = the profile's) now
  has a floor of 50 and a ceiling per board (`Profile::chip_stop_max_c`):
  90 on the NerdOS boards, as NerdOS's Kconfig range allows, and the
  board's own line everywhere else, since AxeOS has no such setting. So a
  Bitaxe's chip line can only go down. `Profile::chip_limit_c` applies the
  ceiling to a value already stored, too (a Gamma set to 90 before now
  stops at 75); `expert_refuse` refuses a PATCH above it, and the settings
  field takes its `max` from `overheatTempMax`.
- API: `vrOverheatTemp` and NerdOS-style `fans[1].overheatTemp` report
  `vr_stop_c`; `overheat_temp` and `fans[0].overheatTemp` the chip line
  in force. The web UI's help text and the swarm card's `|| 105`
  fallback for stock AxeOS boards were right already.
- WarpTune (dvb-WarpPool `crates/tune/src/axeos.rs` 423–431,
  `runner.rs` 2311–2318, 2882) reads `fans[0].overheatTemp` and
  `fans[1].overheatTemp` only — the array WarpAxe emits for the
  TMP1075/EMC2302 Nerd boards, whose lines are unchanged — and derives
  its failsafe (chip line − 1) and regulator ceilings (− 2 / − 6) from
  them. It does not read `vrOverheatTemp` or `overheat_temp`. For the
  Bitaxe boards it keeps its own defaults ("ESP-Miner hardcodes 75/90").
  Nothing to change there.

## The Nano's diode scale (2026-10-23, v0.1.36)

The EMC2101 reads the BM1370's diode through an ideality setting, and
the two makers tune it differently: ESP-Miner writes 0x24 (1.0319) on
every Gamma board (`device_config.h`, v2.15.4), ForgeOS writes **0x37
(1.0566)** on the Nano since its v1.0 (`main/ThermalMonitoring/src/
ThermalMonitoring.c` 27–28, 43–44; values `EMC2101.h` 8, 80). A remote
diode reads roughly in proportion to 1 / ideality, so the same chip at
the same temperature shows about 2.4 % more kelvin, around 8 °C, under
0x24. Measured on the Nano .44 on 2026-10-23 (test image `19ca20a`, both
settings back to back on each chip, 106 pairs, warm at 525 MHz /
1150 mV): 0x24 − 0x37 = 8.25 °C on chip 1 (7.9–8.6) and 8.22 °C on
chip 2, for example 59.1 / 59.9 °C against 51.0 / 51.5 °C; the error
rate did not move.

Until v0.1.35 WarpAxe ran the Nano on 0x24, so its 75 °C line stopped
the board where ForgeOS would have shown about 67 °C, its 60 °C fan
target held the chips where ForgeOS shows about 52 °C (louder than
stock), and the Nano's temperatures were not comparable with a stock
Nano's in a swarm or a tuner. **Since v0.1.36 the Nano profile uses
0x37** (`emc2101::Config::BITFORGE_NANO_DIODE`, Oliver's decision
2026-10-23); every other EMC2101 board keeps 0x24. What follows from
it:

- The stops stay 75 / 105 °C and now apply on the maker's scale, as
  ForgeOS applies them (`power_management_task.c` 21, 28, 149–151).
- The fan target stays 60 °C on that scale: the chip really runs about
  8 °C warmer than it did under WarpAxe before, which is how it runs
  under ForgeOS at the same setting; the fans get quieter.
- The self-test's temperature check compares the readings against the
  same lines (`chip_limit_c`, `vr_limit_c`), so it needs no new limit;
  the warm-up and overheat logic work on differences (`WARMUP_*`,
  `HYSTERESIS_C`) and are scale-free.
- The API (`temp`, `fans[0]`, the swarm card, the diagnostics file)
  reports the new scale: after the update a Nano shows about 8 °C less
  at the same point. WarpTune's temperature-based steps see that too;
  one new WarpTune calibration on the Nano after v0.1.36 is
  recommended.
- No Nano constant in the code was derived from the old scale: the
  stops are ForgeOS's, the fan target is AxeOS's default, the regulator
  stop reads the TPS546. The historic figures in comments and README
  (82 °C after the download-mode incident of 2026-10-03) are on the old
  scale, about 74 °C on the new one, and say so now.

The A/B test build (`diodeab`, commit `19ca20a`) is not kept: it
switched the ideality register of running sensors every tenth reading
with two 250 ms pauses inside the board loop, which has no place in a
release tree even behind a feature, and unlike `fragtest` (which proves
an update path and is worth re-running) its one measurement is done and
recorded here. The
procedure fits in ten lines if it is ever needed again.

## The Nano's Max preset and the 40 A limit (2026-10-23, v0.1.36)

Since v0.1.36 the Nano's regulator warns at 38 A and shuts down at 40 A,
as ForgeOS v1.7 writes them (`main/power/vcore.c` 17–28; the TPS546D24A
is a 40 A part). The Max preset 625 MHz / 1160 mV stays in the choices,
close to that line. WarpTune's journal on the Nano .44 (settled
readings of the regulator's output current, 30.09.–22.10., room
temperature, old diode scale) shows it at 37.1 A on average and 37.6 A
at most: just under the warning, 2.4 A under the shutdown. 625 MHz at
1050 mV drew 33.8–33.9 A; 650 MHz / 1160 mV reached 41.2 A, which the
new limit no longer allows.

Peaks above the settled reading, a warmer room and the chips running about 8 °C warmer now that the fan
target applies on the maker's scale all add some current, so Max can touch the warning line and, rarely, the shutdown.
A shutdown is handled like an overheat stop: chips off, at least 60 s,
back on; the third stop within an hour keeps the chips off with the
reason on screen. ForgeOS offers 636 MHz / 1160 mV under the same
40 A limit. WarpTune stays below 90 % of the warning line (34.2 A), so a
tuned Nano does not get there (Oliver's decision 2026-10-23: keep 625
and say so, rather than drop it).

## Decided by Oliver (2026-10-20)

1. NerdAxe Gamma and Gaia keep the 105 °C regulator stop that NerdOS
   does not have.
2. Gaia chip line stays 75 (the reference board's setting).
3. `overheat_temp` above the board's line: up to 90 on the NerdOS boards
   (as NerdOS), never above the board's line on a Bitaxe and the others.
4. The soak on the .41 and the Gamma follows the merge.

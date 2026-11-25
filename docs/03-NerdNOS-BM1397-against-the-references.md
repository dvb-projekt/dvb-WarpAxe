# NerdNOS (BM1397) under WarpAxe, against WantClue's firmware and ESP-Miner

Written 2026-10-08, before anything ran on the chip; the first run is
recorded at the end. The device is
the owner's NerdNOS (`192.0.2.46`, stock firmware "NerdNOS" 2.0.5, a
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
| 1 | Core power enable at boot | `PEN` GPIO13 output high in `BM1397_init`, stays high | TPS546 rail off until init; reset low | GPIO13 held **low** from `quiesce()` until the chain starts (rail off, like ESP-Miner's boot); high from the first `reset_chain` on | differs on purpose: the chip sits unpowered until the miner is ready, as on every other board |
| 2 | Reset pulse | `_reset()`: GPIO12 **high** 100 ms, **low** 100 ms, stays low (once, at init) | RST **low** 100 ms, **high** 100 ms, stays high | GPIO13 low 100 ms → high, then GPIO12 high 100 ms → low, then 100 ms (`reset_chain` + `stock_reset_pulse`), before **every** chain start | WantClue's sequence; see "Two board revisions" below |
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
| 23 | Core voltage | fixed by the buck's divider: U7 TPS566247, 0.6 V reference, R12 13.3 k / R10 10 k → 1.398 V (rev 1.3 schematic, WantClue/NerdNOS); 1397–1400 mV read on GPIO3 | TPS546, 1100–1500 mV | reported as 1400 mV, no options, no slider; the rail read on GPIO3 every second as VOUT (plausibility 0–2 V) | yes |
| 24 | Power / current | none | TPS546 readings | none (the tiles show "–") | yes |
| 25 | Display | T-Display-S3 (ST7789 i80) | OLED | ST7789 i80, flip from the setting (default upright) | orientation unverified |

## Two board revisions: the published PCB and the one the .46 is

The dates settle which is newer. The KiCad repository
(benjamin-wilson/NerdNOS) was last changed on **2024-05-23** ("fixed 0v8
and 1v8"; the PCB on 2024-04-30, "fixed 12pin position"). WantClue's
firmware has its first working version on **2024-09-07** (`nerdnos.h`,
unchanged since), the thermistor on 2024-09-09, the higher baud rate on
2024-11-12, release v1.0 on 2024-11-19, the last touch on 2026-05-04.

The two do not describe the same board. Against the LilyGO T-Display-S3
pinout (left header, 3V end down: 3V, 1, 2, 3, 10, 11, 12, 13, NC, NC,
GND, 5V; right header: GND, GND, 43, 44, 18, 17, 21, 16, NC, GND, GND, 3V):

| Line | Published PCB (J2 on the left header) | WantClue `nerdnos.h` (the .46) |
|---|---|---|
| chain TX (ESP → chip CI) | GPIO1 (J2.11) | GPIO17 (native U1 TXD, right header) |
| chain RX (chip RO → ESP) | GPIO2 (J2.10) | GPIO18 (native U1 RXD, right header) |
| thermistor | GPIO10 (J2.8, the net the schematic calls TMP) | GPIO2 (ADC1_CH1) |
| core rail sense | none | GPIO3 (ADC1_CH2, `nerdnos_get_vcore`) |
| chip reset | GPIO3 (J2.9, "RST", 100k/120k divider into NRSTI, active low) | GPIO12 ("RST", pulsed high 100 ms then low) |
| core power enable | none: the buck's EN sits on R10/R11 | GPIO13 ("PEN", high to run, low after an overheat) |

WantClue's pins cannot be wired to the published board, and the .46 hashes
under WantClue's firmware. So the .46 is a **later revision than the
published KiCad** (its schematic turned up later, see below): the UART moved to the S3's
native U1 pins on the right header, the thermistor to GPIO2, and the
carrier got a core rail sense on GPIO3 and two control lines next to each
other on the left header (12, 13). The published PCB is the older, not the
right, variant for the .46; WantClue's firmware is the only description of
the .46's wiring, and it is the one WarpAxe follows.

Since WantClue's `_reset()` pulses GPIO12 **high** and leaves it low while
the chip hashes, the new revision's reset is active high on the ESP side
(an inverter into NRSTI), and GPIO13 is what its name says, the buck's
enable. WarpAxe drives both exactly as the stock firmware does: GPIO13 low
at boot and on an overheat stop, high before an init; GPIO12 high 100 ms
then low after GPIO13 went high. If GPIO13 should turn out to be the reset
line itself (the old revision's wiring on a new header), the same drive is
still right. One assumption stays: that the .46 matches WantClue's
`nerdnos.h` and not a third revision.

**Settled on 2026-10-08 (the owner found github.com/WantClue/NerdNOS):** a
fork of benjamin-wilson/NerdNOS with the later board, rev 1.3, commits
up to 2024-10-31 ("other buck converter" 08-28, "fixed reset" 09-10,
Schottky diode so the display's USB cannot feed the buck 09-16, gerbers
1.3, 2-pin fan header 09-21, heatsink with holes 10-31 by shufps). Its
PCB nets, read against the T-Display symbol's pin numbers:

| Carrier net | U1 pad | GPIO | Parts on the line |
|---|---|---|---|
| TMP | 3 | GPIO2 | R8 10 k to 3V3, NTC to GND |
| VDD (1V4) | 4 | GPIO3 | the core rail, sensed |
| RST | 7 | GPIO12 | R14 10 k to 3V3; Q1 2N7002 inverts into RST_N (R1 5.1 k to 1V8): high = chip in reset |
| PEN | 8 | GPIO13 | U7's EN; R11 10 k to GND: low = rail off |
| 5V | 12 | — | D1 1N5817 from VBUS (carrier USB-C J3, CC 5.1 k) |
| TX / RX | 19 / 20 | GPIO17 / GPIO18 | TXU0202 level shifter to CI / RO |

Buck U7 TPS566247DRLR, 0.6 V reference, R12 13.3 k from VDD to VFB and
R10 10 k to GND: 1.398 V, what GPIO3 reads (1397–1400 mV). J1 is a 2-pin
5 V fan header without control, HS1 a heatsink. Everything the profile
drives is confirmed; the "third revision" caveat is gone.

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

## Overheat on a board without a fan: one clock step down

Since 2026-10-08 (the owner's wish): after an overheat stop on a board the
firmware cannot cool (`hasFan` false, the NerdNOS), the chip comes back
one clock option lower (300 → 275 → 250 …) instead of at the same clock;
the stock firmware drops to a fixed 210 MHz instead. The step lives in
RAM only: a reboot starts at the configured clock, a clock set by hand
(`PATCH /api/system`) clears it. The API reports it as `clockStep`
(`fromMhz`, `toMhz`, `atUptimeSeconds`, `cause`), `frequencyActual`
shows the stepped clock, and the dashboard's clock card comes back with
a note saying from what to what, when, and that a restart begins at the
configured clock. WarpTune never sees `clockStep`: it would set the clock
by hand and so clear the step. The .46 is not in WarpTune; if a board
without a fan is ever tuned, the tuner has to read `clockStep` first.

The chip difficulty starts at the profile default (256) and follows a
lower pool VarDiff only after it has been unchanged for a minute, so the
first seconds after a connect (difficulty 1, 4, 16 …) no longer flood
the host with nonces.

## Warmer than the stock firmware at the same work (2026-10-08)

Stock at 300 MHz for an hour: 69.5–71.3 °C, 179.8 GH/s at the pool.
WarpAxe at 300 MHz right after it, same case, same fan: 74 °C after
26 minutes at 177 GH/s. The thermistor formula and the ADC calibration
are the same in both, so the extra heat is real and not from the chip's
work. Suspects on the T-Display in the same closed case: WarpAxe kept
the Wi-Fi radio awake on every board (`WIFI_PS_NONE`, tenths of a watt
all the time) where the Arduino stock leaves modem sleep on, and the USB
network link to the Mac (TinyUSB NCM) that the stock run did not have.
Since this change a board without a fan leaves modem sleep on, and
`dieTempC` in `/api/system/info` shows the ESP32-S3's own temperature
on every board.

Results the same morning, all at 300 MHz: the USB cable off for 20
minutes changed nothing (74.2–74.3 °C; a 0.8 °C bump coincided with a
vacuum cleaner in the room). Modem sleep took the ESP's die from 59.4 to
56.4 °C and the chip's plateau from 73.9–74.4 to 73.2–73.4 °C. About two
degrees over the stock firmware remain unexplained — the display, the
processor at 56 °C in the same case, or the room; two degrees is what the
vacuum cleaner did. The owner's decision: leave it so, modem sleep stays for
boards without a fan, the rest is not chased.

**A/B at the pool, 300 MHz, one hour each (Warppool #2, Σ share
difficulty × 2³² / time):** stock 05:16–06:16 UTC 131 shares → 179.8 GH/s
±9 %; WarpAxe 662322c 06:29–07:29 UTC 248 shares → 190.9 GH/s ±6 %,
halves 194.3 / 187.4. Neither side lost shares (0 rejected, no low-diff,
stale or duplicate on either). The 6 % lie inside the combined ±11 %, so
the honest reading is "at least on par with the stock, about 90–95 % of
672 × f on the fixed 1.4 V rail" — the chip, not the driver. If the
difference is real, it would be work lost on the chip side under the
stock's mask churn (158 mask changes in the hour, one every 27 s, driven
by its periodic `suggest_difficulty`), which only a much longer run could
show. It is not "more work", so it does not explain the heat.

## First start without a hand: the NerdNOS detects itself (2026-10-08)

The rev 1.3 schematic gave a signature no other board has: the
thermistor divider on GPIO2 (a steady mid-rail voltage where a bare
T-Display's pin floats) together with the carrier's 1.4 V core rail on
GPIO3 (U1 pad 4 = VDD, the TPS566247's 1.398 V). With both, the first
start takes the NerdNOS profile outright and the chips run from the
first boot; the .46 needed `PATCH boardModel` and a restart before. One
catch found on the first try: the buck's enable (GPIO13) has a 10 k
pull-down, so the rail is off at every boot and GPIO3 read 0 mV. With
the thermistor there, the first start drives GPIO13 high for 150 ms for
the reading and back off; the chip stays in reset meanwhile (RST floats
high through R14 into the inverter), and a bare T-Display has nothing on
that pin. Verified on the .46 with 66cdbe1: 1399 mV, profile taken,
chain ready 18 s after boot. The
thermistor alone still only offers the choice, as nothing may drive a
chip's reset line on a guess, and the worst case of a wrong take is a
chain that finds no chip and stops.

## Rollback after a failed update, proven on the devkit (2026-10-08)

Since a51ce13 the bootloader has app rollback: an image written over
the air boots once as pending-verify and the app marks itself valid at
chain ready, or after three minutes up on a board without a chain. The
bootloader is never updated over the air — a device gets it with one
USB flash (bootloader, partition table, app); the web flasher ships it
from the first install. Test on the devkit, three cycles with an image
that panics ten seconds after boot: each time the bootloader came back
to the previous app within a minute; with c8ae5f2 the API reads
otaState "aborted" afterwards and the System page says the miner went
back to this version. Two lessons on the way: after a USB flash of a
device that had been updated over the air, erase otadata or it keeps
booting the other slot; and never read the otadata partition from an
HTTP handler — esp_ota_get_last_invalid_partition there hung the HTTP
server for good (pings fine, port 80 dead) 35 s after boot, so the
image state is read once on the main task at boot. Same family as
0.1.5's reboot every minute: PSRAM-stacked threads and HTTP handlers
keep away from flash, NVS and esp_ota_*.

## Gaps and open points before a flash

1. **Midstate byte order** is the one all three references compute; a wrong
   order would show up at once as `below_ticket` nonces (header re-hash
   fails) — watch that counter on the first run.
2. **Clock cap 400 MHz**, options 200–400, stock 300: nothing above has been
   measured on this USB-powered, fanless board.
3. **Display orientation** on the carrier: unknown; the flip setting covers it.
4. **Detection**: on the first start the NTC divider on GPIO2 (WantClue's
   pin) offers `nerdnos` first (never taken); a bare T-Display-S3 with
   NerdMiner's settings is offered as the devkit (since v0.1.48 never
   taken on its own: an NMAxe on NMMiner looks the same). If the .46's NTC does not sit on
   GPIO2 after all, the board can still be chosen by hand.
5. **Board revision**: the .46 is assumed to be the revision WantClue's
   `nerdnos.h` describes (see "Two board revisions"); the published KiCad
   is older and wired differently.
6. **Chip id reply**: the simulator answers `13 97 18 00`; the real one must
   match (`ChipFound { chip_id: 0x1397 }` in the log).

## Before the first flash on the .46 (rules in force)

Full flash backup over USB first; committed images only; the owner's OK and a
heads-up to Warppool #2; the preview before the flash. None of this has run
on a chip.

## The first run on the .46 (2026-10-08, 01:21–01:35 UTC)

Backup of the stock flash over USB (16 MB, 208 s), flash of de340c871,
first boot 01:26:15Z: the thermistor divider read 1399 mV (33 °C), the
board was offered as `nerdnos` and chosen over the API, the wallet came in
as `<wallet>.NerdNOS`. The chip answered `13 97 18 00` and the PLL word
read back as `c0780215` (bit 7 set: locked). Two things learned:

* **Power.** The chip's core rail comes from the carrier's own USB-C
  port; the T-Display's port (the data port) only reaches the logic
  rails. With the carrier unpowered the chip answered every register read
  and the PLL locked, but the core rail read 0 mV on GPIO3, register 0x04
  reported 0 and no nonce came. With the PSU back on the carrier and a
  chip off/on over the API the first nonce came within three seconds of
  the chain being ready.
* **Clock.** The config's generic default (525 MHz) was the first chain
  target until the API set 300 — about 7 s at 525 MHz on the 1.4 V rail,
  nothing happened. Since then a clock above the profile's ceiling is
  clamped to the profile's default at boot (`board::init_bus`).

Numbers at 300 MHz after ten minutes: register 0x04 177.6 GH/s in the
long window and 199.1–199.3 GH/s in the short one (the chip alternates
the windows on successive reads, short first, as the simulator guessed),
the pool-side rate from nonces 183–275 GH/s over the first minutes,
every nonce above the ticket mask (`below_ticket` 0), 16 of 16 shares
accepted at difficulty 512, core rail 1396–1398 mV, board temperature
33 → 54 °C in the first eight minutes (stock NerdNOS showed 32–48 °C).

Register 0x4C counted 335 in eight minutes against 20 nonces, so on the
BM1397 it is not the error counter the BM1370 has (or it counts every
core result); the web UI showed it as a 35 % error rate. Since
3c58ec8's successor the driver reads only 0x04 on this chip and the
error figures are null (API) and a dash (UI) until 0x4C is understood;
the host checks every nonce against the ticket mask itself
(`below_ticket`, `duplicates` in `/api/v1/state`). The GPIO12 pulse and the GPIO13 line did
what the stock firmware's do; which one is the reset and which the buck
enable is still only inferred.

## What register 0x04 measures (experiment 2026-10-08, 02:10–02:19 UTC)

The .46 ran firmware 85872f8 at 250 MHz with the stock 1.4 V rail. The
driver's job cadence and the clock were changed over the API, each
setting held 60–90 s, then register 0x04 read ten times and the chain's
nonce count noted. The long window lags by minutes, so the short-window
readings carry the comparison.

| Setting | 0x04 short (GH/s) | 0x04 long | Nominal 672 × f | Nonces in the phase |
|---|---|---|---|---|
| 250 MHz, jobs every ~37 ms (default) | 130–153 | 136–138 | 168.0 | 62 in 416 s → 164 GH/s |
| 250 MHz, jobs every 200 ms | 128–140 | 138 | 168.0 | 23 in 154 s (phase mixed) |
| 250 MHz, jobs every 100 ms | 135–149 | 138 | 168.0 | 8 in 102 s → 86 GH/s |
| 300 MHz, default cadence | 169–180 | 144 (rising) | 201.6 | 20 in 127 s → 173 GH/s |
| 200 MHz, default cadence | 85–106 | 139 (falling) | 134.4 | 15 in 131 s → 126 GH/s |
| 250 MHz again | 132–142 | 138 | 168.0 | 16 in 127 s → 138 GH/s |

Nonce figures are rough: at a chip difficulty of 256 a phase holds 8–20
nonces (±25–35 %). Two things are clear anyway:

* **The register is not a count of hashes done.** With jobs every
  100–200 ms the chip runs out of nonce space (2³² nonces at 168 big
  cores × 250 MHz take about 100 ms) and the nonce rate roughly halved,
  while 0x04 did not move. ESP-Miner's reading of 0x04 (count × 2²⁰ H/s,
  bit 31 the long window) was taken over from the BM1370; on the BM1397
  the value behaves like a capacity figure that falls short of 672 × f
  by a roughly constant 50–60 GH/s (86 % at 300, 81 % at 250, 70 % at
  200 MHz), not like a hash counter.
* **The pool decides.** Pool-side share rate (Warppool, 1-min buckets):
  300 MHz 01:33–01:47 UTC 36 shares ≈ 169 GH/s, 250 MHz 02:02–02:10 UTC
  16 shares ≈ 165 GH/s, both ±25 %. A real deficit at 300 MHz of about
  10–15 % is there under both firmwares (A/B below) and is invisible as errors: a core that
  computes wrongly finds no nonce rather than a wrong one (`below_ticket`
  stays 0). Judging a clock step needs an hour at the pool (≈100 shares,
  ±10 %), not the register.

**Second look, 03:00 UTC, worker renamed to .WarpNOS at 02:39.** The
firmware's own hashrate weights every nonce with the chip difficulty in
force when it came (the ticket follows the pool's VarDiff below 256:
219 → 128), so it is the figure to trust on the device: 120–135 GH/s at
250 MHz over the first 18 minutes (163 nonces, 92 shares sent, 92
accepted, 71 under the pool difficulty, 0 under the ticket). The pool's
diff-weighted 105 GH/s ±13 % over 15 minutes and the register's 136–138
GH/s sit around the same value. The naive 164 GH/s above assumed
difficulty 256 for every nonce and is too high. So the register may well
report what the chip delivers — about 80–86 % of 672 × f at 250 and
300 MHz — and its indifference to the 100–200 ms cadence rests on eight
nonces (±35 %) and is not proven either way. What stands: the chip
delivers 15–20 % under 672 × f on the stock 1.4 V rail, invisibly (no
wrong nonces), and an hour at the pool per clock step is the measure.

The web UI's hash-counter card shows the register as it is read; the
dashboard's hashrate comes from nonces weighted with the chip difficulty. The register's short-window
value tracks the clock closely enough to show that the chip follows a
clock change.

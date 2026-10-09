# NerdOctaxe-γ: a hash domain that only a power cycle clears

Written 2026-10-09 for the .45 (`192.168.178.45`, NerdOctaxe-γ Rev 3.1,
TPS53667 six-phase, eight BM1370 at 700 MHz / 1210 mV). Firmware v0.1.23;
the changes below are the candidate for v0.1.24. Nothing here has run on
the board yet; the test plan at the end is what decides.

## What was seen

| Time (UTC) | Event | Chip / domain (1-based) |
|---|---|---|
| 05:13 | Cold power-on (PSU off and on), v0.1.22 | chip 2 domain 2 off |
| 05:17 | Chips off, 12 s, chips on (API) | **all 32 domains back** |
| ~10:17 | OTA to v0.1.23: a warm ESP reboot | — |
| 10:18 | first sample after READY | chip 8 domain 2 off |
| 10:18–11:07 | five re-inits by WarpTune (5.5 s and 15 s off) | chip 8 back, chip 5 off, then chip 2 domain 2 off and it stays |
| 11:34 | Chips off by hand, 12 s, on; core rail read 0 mV within 3 s | chip 2 domain 2 at 0 from the first sample after READY, 6.5 min watched |
| ~11:52 | **Power cycle** (PSU off a few seconds) | **all 32 run** |

What this says: the domain reads 0 from the first counter sample after
the chain's init, so it is a domain that fails to come up, not one that
dies warm. After a cold boot one off/on cleared it (one sample); after a
warm reboot six did not (chips at 66 °C, fan 83 %, no clock or voltage
change around any of them). v0.1.22 → v0.1.23 changed nothing in the
chain, board or regulator code.

## Reference diff: NerdOS against WarpAxe v0.1.23

NerdOS = shufps/ESP-Miner-NerdQAxePlus `develop` 762092d, the firmware
the .45 shipped with. The NerdOctaxeGamma board class derives from
NerdQaxePlus and overrides none of this.

| Step | NerdOS | WarpAxe v0.1.23 | Same? |
|---|---|---|---|
| Lines | TPS53647 EN GPIO10, LDO EN GPIO13, chain RST GPIO1, UART1 TX GPIO17 / RX GPIO18 | the same | yes |
| First thing after a reset | `initBoard` (after NVS, display, I²C, fans): EN, LDO, RST as outputs, low | `quiesce()` right after the I²C bus: RST low, EN low, LDO low (`rail_off("boot")`) — earlier | yes, ours earlier |
| During the ESP reset itself (ROM, bootloader) | the pads are whatever the chip's reset state and the board's resistors make them; neither firmware can act | the same | yes |
| Bring-up (`initAsics` / `start_nerd`) | EN off, LDO off, RST low, **250 ms**, LDO on, 100 ms, regulator init, EN + init voltage, 500 ms, RST released, 250 ms | the same order and times | yes |
| Chips off without a reboot | **does not exist**: `shutdown()` is terminal (core off, 500 ms, LDO off, 500 ms, the system task suspends), only a reboot brings the chips back | core rail off by EN; **LDO stays on, RST stays high, TX idles high**; 500 ms after EN comes back the miner pulses RST 10 ms and inits | **differs** |
| Before a restart (OTA, restart, download mode) | `PowerManagementTask::restart`: `shutdown()` as above, then `esp_restart` | `prepare_reset`: SHUTDOWN flag → the board loop drops EN; wait until it reports the rail off (≤ 2 s) + 200 ms; RST low; `rail_off`: EN low, LDO low; **`esp_restart` at once** | differs in the half second after the LDO |
| Overheat stop | core off by EN, LDO stays, never resumes | core off by EN, LDO stays, resumes 10 °C below the limit | same off state, we resume |

Two things stand out. WarpAxe's chips-off state is one NerdOS never
enters: the chips' I/O rails and the reset line stay up with the core
gone. And neither firmware gives the rails more than a quarter second
before the LDO comes back after a warm reboot, while a power cycle gives
them seconds with the 12 V input gone as well.

## The questions from the handoff

1. **Does the chips-off path cut the LDO and hold RESET low?** No. Only
   `prepare_reset` did (EN and LDO low, RST low, then the reset). The
   board loop's off branch calls `Regulator::off`, which for the Nerd
   regulators was `enable.set(false)` alone; the reset line is held low
   there only on a board without a regulator (NerdNOS). Changed, see below.
2. **What do EN, LDO and RST do while the ESP resets?** The lines are not
   driven until `quiesce()` runs, ~1 s after the reset (ROM, bootloader,
   NVS, config). The pads' state in that window is the chip's reset
   default plus the board's own resistors; the schematic of the Rev 3.1
   carrier is not in hand, and the datasheet's pin table could not be
   read on this machine (no PDF tools). It is the one item that only a
   scope on the three lines during an OTA reboot answers. What can be
   said: NerdOS lives with the same window, and the chips sit in it with
   EN, LDO and RST at the board's idle levels for about a second. If one
   of them idles high (an LDO with its own pull-up, say), the I/O rails
   come back for that second with the core off and the reset floating.
3. **How long are core and LDO off in an OTA reboot?** Before the change:
   the core from the board loop's off (≤ 2 s before the reset) to
   `start_nerd`'s EN; the LDO from `rail_off` (just before `esp_restart`)
   through the reset window (~1 s, not driven), then driven low from
   `quiesce()` through display, USB and Wi-Fi start (the Wi-Fi start does
   not block) to `start_nerd`'s 250 ms — one to three seconds in all, with
   the first second not under the firmware's control. A power cycle is
   "a few seconds" with the 12 V gone too. Whether the BM1370's internal
   state needs the 12 V-side supplies gone, or just longer, is not known;
   the change gives it the seconds, which is the part the firmware can do.
4. **NerdOS reference.** Quoted above. `PowerManagementTask::restart`
   locks the hardware, calls `shutdown()` (core off, 500 ms, LDO off,
   500 ms) and `esp_restart`s. `initAsics` is the bring-up order WarpAxe
   already follows.
5. **Proposal.** Done as the two items below, plus TX parked low. The
   Gaia is untouched: it runs `start_tps546_nerd` and `Vr::Tps546Nerd`,
   whose LDO stays up (cutting it breaks the chip's enumeration there);
   the changes are confined to `Vr::Tps5364x` boards (NerdQAxe++ Rev 6,
   NerdOctaxe-γ, Q1370, Q1373) and the Nerd bring-up path.

## What changes (branch `claude/octaxe-warmstart-latch-17b556`)

The constants and the one policy function live in
`crates/board/src/warm_boot.rs` with host tests; the firmware uses them.

1. **Chips off = NerdOS's shutdown** (`Regulator::off` for `Tps5364x`,
   used by the manual off, the overheat stop and `prepare_reset`): the
   chain's reset line low first (a reset line held at 3.3 V into a chip
   whose I/O rails are gone would feed it), the core rail off by EN,
   500 ms, the LDO off. Chips on = `initAsics`'s order (`Regulator::on`):
   LDO on, 100 ms, EN on and the voltage, the board loop's 500 ms, then
   the miner releases the reset (`reset_chain`) and inits. The reset line
   stays low for the whole off time.
2. **The chain's TX parked low while the chips are off** (`ChipUart::park`,
   on boards that cut an LDO — `board::parks_uart`): an idle UART keeps
   TX at 3.3 V, and a chip without I/O supply gets that through the
   protection diode of its RX pin, so the rail the LDO was meant to take
   away stays half up. The miner parks TX when it sees the chain stopped
   (within milliseconds, before the LDO goes 500 ms later) and puts it
   back on the UART before the next reset pulse. A cold start has
   nothing driving the line; this is the nearest the firmware gets.
3. **A warm reboot waits like a power cycle** (`warm_boot::settle_ms` in
   `start_nerd`): when the ESP's reset reason is not a power-on, the pause
   before the LDO comes on is stretched so that EN and LDO have been low
   for at least `WARM_OFF_MS` = 5 s, counted from the boot's `rail_off`
   (so the seconds the boot spends on USB and Wi-Fi count). After a
   power-on it stays NerdOS's 250 ms. The log says so:
   `board: warm boot (reset reason software): rails low for N ms, M ms more before the LDO comes on`.
4. **Half a second after the LDO before a reset** (`prepare_reset`), as
   NerdOS's `shutdown` has it before `esp_restart`.

Not done, and why:

- **Holding the pads through the reset** (`gpio_hold_en` on EN, LDO, RST
  before `esp_restart`, so the lines stay low through the ROM and
  bootloader instead of floating). The pad hold lives in the RTC domain
  and survives a software reset, but a USB flash of another firmware
  (back to NerdOS) after our download mode would boot with the pins
  locked low — the regulator could not be enabled until a power cycle —
  and an OTA rollback into an image that does not know to release the
  hold would do the same. Only worth it if the scope (question 2) shows
  a line idling high in the window; then with the hold set only for a
  restart into our own image, never for download mode.
- A minimum off time for a manual chips off/on. WarpTune and the API
  choose the off time; the firmware does not add to it.

## Test plan for the .45

Flash only with Oliver's OK and a heads-up to Warppool #2; a committed
image only; WarpTune's re-inits off (or its latch rule at "needs a power
cycle") during the runs, so the counts are the firmware's alone.

Before anything: `/api/v1/state` → `resetReason`, and the chip grid
(`/api/v1/chips` or the dashboard's hash counters per chip and domain)
as the baseline. "Domains" below means the per-chip, per-domain hash
counters two minutes after `chainPhase` READY, every domain non-zero.

1. **OTA to the candidate** (a warm reboot, the case that latched 5/5).
   Expect in the log, in this order: `chips off before the reset`,
   `chips in reset, LDO off 500 ms after the core`, then after the reboot
   `warm boot (reset reason software): rails low for … ms, … ms more
   before the LDO comes on`, then the bring-up. Domains at +2 min.
   Repeat the OTA (same image, or restart from the System page) three
   times; note the domains each time.
2. **Chips off and on** (`POST /api/v1/chips/off`, 12 s,
   `POST /api/v1/chips/on`), three times. Expect `miner: TX parked low
   while the chips are off`, `board: chips in reset, LDO off …`, and on
   the way back `miner: TX back on the UART` before the chain init.
   Domains at +2 min. Also the regulator telemetry: `vout_mv` 0 while
   off, back to 1210 after; no `REGULATOR FAULT`.
3. **If a domain is dead after step 1 or 2**: one more chips off/on with
   30 s off. If still dead: a power cycle, and the scope question (2)
   becomes the next step — EN, LDO EN and RST on the carrier during an
   OTA reboot.
4. **Side effects to watch**: the first chain init after a chips-on
   (NoChips would mean the LDO's 100 ms is too short for this board —
   NerdOS uses the same, so unlikely); the boot time (+ up to 5 s on a
   warm reboot, visible as the BOARD step on the display); an overheat
   stop and resume, if one happens, now cycles the LDO too.
5. **The other Nerd carrier** on the bench (NerdQAxe++ Rev 6.1): one OTA
   and one chips off/on, domains at +2 min. Its stock firmware cycles
   the LDO at every boot, so the off/on order is nothing new for it.

What a pass looks like: all 32 domains after each of the six re-inits
in steps 1 and 2. What tells us the firmware cannot do it alone: a
domain dead after step 1 that step 2 does not clear but a power cycle
does — then the 12 V-side supplies are the difference, and the carrier
needs a scope or a switched input.

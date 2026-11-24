# Board choice guard: a chosen board is checked before its chips get power

Written 2026-10-22 (branch `feat/board-choice-guard` on `feat/v0135`, for
v0.1.35), after dvb-WarpAxe #2 and the owner asked that a board picked by
hand on the first start cannot hurt the board it actually runs on.

## The problem

When the first-start detection (`crates/board/src/detect.rs`) is not sure,
the web UI offers a choice. Up to v0.1.34 that list carried the boards that
fit on top and **every other board** below them, and the board chosen was
simply saved and run at the next start: its GPIOs driven (reset, enable,
LDO), its regulator window written, its clock and voltage applied. A wrong
chip count only produced a warning in the log; the chain kept running.

A wrong family mostly fails on its own (the regulator does not answer), but
its GPIOs may be supply lines on the real board (the Gaia's GPIO12 is its
LDO enable, GPIO13 the regulator's alert). A wrong **variant inside a
family** is the dangerous case: the regulator answers and the chips get the
other board's voltage and current limits. The self-test (v0.1.34) does not
catch this: it compares the core voltage against the setting of the same
wrong profile, and it only runs once the chips have had power.

## What is built

Three layers, all read-only until the last one, and none of them changes a
board that has already been verified.

1. **The choice is narrowed.** The web UI lists only the boards that fit
   what answered on the bus. *Show all boards* reveals the rest, with a
   warning. The firmware does not rely on this: a `PATCH /api/system
   {boardModel}` with an unknown name is refused (400); a known name is
   saved and goes through the check below at the next start, whatever the
   UI showed.

2. **Plausibility before anything is switched on**
   (`crates/board/src/choice.rs`, firmware `board::gate`). On the start
   after a choice (or after the detection took a board, or after this
   update on a board whose choice was never verified) the firmware probes
   both pin pairs as the first start does (47/48, 44/43: address reads
   only, no register writes except switching off a core rail the stock
   firmware left on, as before), reads the TPS546's `READ_VIN` and the
   TPS5364x's device code, and judges the chosen profile against it. The
   requirements are derived from the profile, not written per board:

   | Profile needs on its own bus | must answer | must be silent | input class |
   |---|---|---|---|
   | TPS546 regulator (`Vr::Tps546*`) | TPS546 0x24 | — | 5 or 12 V from `READ_VIN` (0.75–1.35 × nominal), only where another profile on the same bus shows the same parts on the other class (the NerdAxe Gamma and the Gaia) |
   | TPS5364x regulator | — | TPS546 0x24 (a Rev 7) | — |
   | INA260 (Nano) | INA260 0x40 | — | |
   | FXL6408 expander (Q13xx) | FXL6408 0x43 | — | |
   | no expander | — | FXL6408 0x43 | |
   | EMC2101 sensor | EMC2101 0x4C | the other fan controller at 0x2E, TMP1075 0x48, PCA9544 0x70 | |
   | EMC2103 sensor | EMC2103 0x2E | EMC2101, TMP1075, PCA9544 | |
   | TMP1075 + EMC2302 | both | EMC2101, PCA9544 | |
   | two EMC2101 behind a mux (Nano) | PCA9544 0x70 | — | |
   | NerdQAxe++ only | — | a TPS53667 device code (0x01F8) | |
   | no bus (NerdNOS, bare T-Display-S3) | nothing on either pair; the NerdNOS its thermistor on GPIO2 | | |

   A profile that fails is **refused**: nothing of it is driven (the check
   runs in `board::init_bus`, before `board::quiesce`), the miner runs on
   the chipless `undetected` profile as in the choose state, the web UI
   shows why (the part that did not answer, the part of another board
   that did, the input voltage) and the boards that fit, and the event log
   gets `board: <name> refused: …`.

3. **A cautious first start, then the count.** A profile that fits brings
   its chain up at the lowest bring-up voltage of any board the bus
   cannot tell it from (`choice::cautious_core_mv`: the profile's own
   bring-up voltage, or the configured point when that is lower; never
   above the profile's own bring-up, never above any sibling's bring-up,
   never below the regulator's clean range). A configured point above
   the bring-up voltage (an overclock, or a voltage left over from the
   board configured before) applies only once the choice is verified;
   one outside the profile's own window (`regulatorMinMv` … `absMaxCoreMv`)
   is reset to the factory point at that start, as the clock already is.
   The enumeration then decides (`choice::first_start_verdict`, applied
   in `miner.rs` on `Event::Enumerated` / `UnexpectedChip` /
   `InitFailed`), always against the profile's own chip count, never the
   configured `asicCount` (that is the one tell the NerdQAxe++ / Octaxe
   pair has):

   - the count equals the profile's → **verified**: the name is written
     to NVS (`boardok`), the configured core voltage applies once the
     chain init is over (`chain_ready`, as NerdOS sets its final voltage
     after the init), and 20 s later the self-test runs once (its result
     lands in the event log as before);
   - fewer chips than the profile has, and no board that fits the bus
     has that count (an Octaxe with seven) → **verified with a warning**
     (`verified, 7 of 8 chips` in the event log): a dead chip or a bad
     contact, not another board; the self-test then says which;
   - more chips than the profile has, exactly a sibling's count (four on
     an Octaxe profile: the NerdQAxe++; one on a Gamma Duo profile: the
     Gamma), a chip id the profile does not carry, or no chip in three
     inits → **refused**: the chips are switched off again (rail off,
     reset held) and stay off whatever `/api/v1/chips/on` says (it
     answers 409), until a restart (the same choice is tried once more)
     or another choice in the web UI. Nothing is saved. A regulator that
     did not come up is a board fault, not a verdict: the choice stays
     pending and the board's error shows as before.

   A board without a chain (the devkit) is verified at the check. A board
   whose name is already in `boardok` starts as before; the probe is not
   repeated.

### Every pair, as the test walks it

`choice::tests::every_wrong_choice_is_refused_or_a_harmless_sibling`
builds what each real board shows the probe (from the schematics and the
bench, written by hand so it does not read the rules back from the
profiles) and tries every other profile on it. The outcome, 2026-10-22:

| real board → chosen | result |
|---|---|
| any board → a profile of another family or bus | refused at the check (missing or unexpected part, wrong bus, parts where none belong) |
| Gaia ↔ NerdAxe Gamma | refused at the check: `READ_VIN` 12 V vs the Gamma's 5 V class (and the other way round) |
| NerdQAxe++ ↔ Rev 7 | refused at the check: the Rev 7's TPS546 at 0x24 is unexpected / missing |
| NerdQAxe++ → Octaxe with the six-phase TPS53667 | refused at the check: device code 0x01F8 |
| Gamma → Nano | refused: the PCA9544 mux answered |
| Gamma ↔ Gamma Turbo | refused: EMC2103 vs EMC2101 |
| Gamma ↔ Gamma Duo | **sibling**: same regulator window, one domain, 1150 mV bring-up both; the count refuses the wrong one (a Duo profile on a Gamma counts one, the Gamma's count; a Gamma profile on a Duo counts two, more than it has) |
| Naja Duo ↔ Gamma Hex | **sibling**: two domains both; first start at 1000 mV × 2 = 2.0 V (the Naja's bring-up; the Hex's own is 2.4 V); the chip type (BM1373 vs BM1370) refuses the wrong one before any count |
| NerdQAxe++ ↔ Octaxe with the four-phase TPS53647 | **sibling**: one domain both; first start at 1200 mV (both bring up at 1200 / 1210); the count refuses the wrong one (an Octaxe profile on a NerdQAxe++ counts four, the sibling's count; a NerdQAxe++ profile on an Octaxe counts eight, more than it has) |
| Q1370 ↔ Q1373 | **sibling**: one domain both, four chips both; first start at 1050 mV (the Q1373's bring-up; the Q1370's own is 1200); the chip type refuses the wrong one |
| NerdNOS → T-Display-S3 devkit | **sibling** without a regulator: the ESP hashes on its CPU, the chip stays in reset |

The test asserts for every accepted pair that the chosen profile has no
more voltage domains in series than the real board, that the cautious
VOUT is at or below the real board's own bring-up VOUT, and that the
chip type or the count differs (or neither has a regulator); a second
test (`every_sibling_pair_is_told_apart_by_count_or_chip_type`) runs the
count rule on each pair and expects a refusal. A new profile that
creates a new ambiguity fails these tests.

### Does a sibling ever get too much voltage?

| real board | bring-up VOUT | wrong sibling's cautious VOUT | wrong sibling's own VOUT would have been |
|---|---|---|---|
| Gamma Duo (as Gamma) | 1.15 V | 1.15 V | 1.15 V |
| Naja Duo (as Hex) | 2 × 1.00 = 2.0 V | 2.0 V | 2 × 1.20 = 2.4 V |
| Gamma Hex (as Naja) | 2 × 1.20 = 2.4 V | 2.0 V | 2.0 V |
| NerdQAxe++ (as Octaxe-4p) | 1.20 V | 1.20 V | 1.21 V |
| Octaxe-4p (as NerdQAxe++) | 1.21 V | 1.20 V | 1.20 V |
| Q1373 (as Q1370) | 1.05 V | 1.05 V | 1.20 V |
| Q1370 (as Q1373) | 1.20 V | 1.05 V | 1.05 V |

The current limits of a wrong sibling (the Hex's 160 A fault on a Naja
Duo's two stacked TPS546, the NerdQAxe++'s 90 A on an Octaxe) are not
protective, but they only matter under load, and the chain never gets
work before the count is judged: the enumeration happens first, and a
refused count switches the rail off within a second.

### What the chip-id check really sees

The simulator scenario (`crates/chain/tests/sim.rs`, `board_choice`)
shows the two directions differ: BM1373 chips answer the BM1370 driver
with 9-byte id frames the 11-byte decoder cannot read, so the driver
counts none (NoChips); BM1370 chips under the BM1373 driver carry an id
the profile does not know (UnexpectedChip, then NoChips). Both refuse the
choice; the web UI text differs (no chip in three attempts / a chip of
another type).

## Auto-take on the first start

The detection now takes two more boards without asking, both unique on
the bus and both on our bench for weeks:

- **BitForge Nano**: TPS546, INA260 and the PCA9544 mux on the Bitaxe
  pins (no other board has the mux). ForgeOS's board version 800 alone,
  without the bus agreeing, still only offers it.
- **NerdAxe Gaia**: TPS546 and EMC2101 on the Nerd pins **and** 12 V at
  the regulator (`READ_VIN`). The same parts at 5 V are a NerdAxe Gamma,
  which has never run under WarpAxe and stays a choice (alone in the
  list). Without a reading both are offered, as before.

The TPS5364x's address (0x71) is probed too, with its device code; the
detection does not depend on it (NerdOS reads the part only after the
LDO is up, so the probe may not see it), the check uses the code only to
rule the NerdQAxe++ out on a six-phase part.

Every board taken by the detection goes through the cautious first start
as well: a Gamma Duo without AxeOS keys looks like a Gamma on the bus and
used to run as one, with a warning; now the count refuses it.

## After the update

A `boardModel` saved by a firmware before v0.1.35 has no `boardok` entry
at all: it has run on this board, so `board::init_bus` takes it as
verified, writes the entry and starts as before. Nothing changes for the
fleet. Only a choice made under this firmware goes through the check:
the detection and `PATCH boardModel` write `boardok = "pending"` first,
so the entry is present but not the name. (First version of this branch
gated every board without the entry; the audit showed that a fleet board
with one dead chip, or whose sensor had died, would have been switched
off and kept off at the update: finding 1 below.)

A board whose sensor or regulator has died is refused only when it is
chosen anew; the web UI then says which part did not answer. A missing
*sensor* (not only the regulator) is refused too rather than degraded as
the running path does (fans at full, no chip limit): on a new install a
silent sensor more likely means the wrong board than a broken one
(decided 2026-10-22).

## Web UI, API, events

- `GET /api/system/info` → `boardDetect`: `status` is now one of
  `detected`, `chosen`, `choose`, `unsupported`, `rejected`, `mismatch`;
  `chosen` names the board under test, `choice` where it stands
  (`pending`, `verified`, `rejected`, `mismatch`), `reason` the check's
  verdict (`{code, part, addr}` or `{code: "inputVoltage", readV,
  nominalV}`), `detail` the enumeration's (`{kind: count|chip|none, found,
  expected, chipId, chip}`), `cautiousMv` the first start's core voltage,
  `bus` what the probe found (addresses per pair, `vin`, `tps5364xCode`,
  `thermistor`). `boardDetect` is `null` on a verified board's later
  boots, as before.
- `PATCH /api/system {boardModel}`: 400 for a name the firmware does not
  know; a known name is saved, marked pending in NVS and logged
  (`board: <name> chosen by hand`), and verified or refused at the next
  start. The verified board's own name is a no-op (seen on the Gaia,
  2026-10-22); `{"boardModel": "<its name>", "reverify": true}` marks it
  pending all the same, which is how a fleet board is put through the
  check, the counted first start and the self-test on purpose (the probe
  then also logs `READ_VIN`). `POST /api/v1/chips/on` answers 409 while
  a refused choice holds the chips off.
- Web UI: the choice card lists the boards that fit; *Show all boards*
  with a warning; a refused board shows the reason and the boards that
  fit; a mismatch shows what answered, with *Restart and try again*; a
  pending first start shows a small card with the cautious voltage.
  Texts in eight languages (`bd_showall`, `bd_rej_*`, `bd_r_*`,
  `bd_mm_*`, `bd_pending*`, `evk_board`), translated by native-speaker
  agents and proofread independently.
- Event log, kind `board`: `<name> first start at <mv> mV`, `<name>
  verified, <n> chips`, `<name> refused: …`, `<name> chosen by hand`.

## Tests

- `crates/board/src/choice.rs`: nine tests, among them the pair matrix
  above, the cautious voltages per sibling pair, the reason for each
  tell-tale part, the candidate lists per real board, the input-class
  window, the first-start verdict.
- `crates/board/src/detect.rs`: the Nano taken from its bus (and only
  offered from its board version alone), the Gaia taken on 12 V and the
  NerdAxe Gamma offered on 5 V, the TPS5364x address changing nothing.
- `crates/chain/tests/sim.rs`, `board_choice`: two chips under a
  four-chip profile (refused by the count), BM1373 under the BM1370
  driver and the reverse (refused by the id or as no chips), a silent
  chain (NoChips, which the firmware only turns into a refusal after
  three inits).

## Rules kept

- The check and the probe are read-only (the one write, OPERATION off
  for a rail the stock firmware left on, is the first start's own since
  v0.1.15; the 150 ms pulse on GPIO13 to read the NerdNOS rail on a board
  where nothing answered is the first start's own since v0.1.22).
- No profile pin is driven before the check passed: `board::gate` runs
  inside `board::init_bus`, `board::quiesce` and `board::start` come
  after.
- The cautious voltage is never above the profile's own bring-up voltage
  (a configured point above it waits for the verification, and a PATCH
  of `coreVoltage` during the first start is capped at it), never above
  a sibling's, and never below the regulator's clean range
  (`regulator_min_core_mv`).
- Stop limits, fan logic and the self-test are unchanged; the self-test
  after a verified first start is the ordinary one (`POST
  /api/v1/selftest`), started 20 s after the chain is up.
- Nothing was flashed and no device was restarted for this work; the
  first device runs are the owner's call (a wrong choice is never tried on
  real hardware, only here and in the host tests).

## Image size

`warpaxe.bin` 2,855,360 B, 31.4 KB over the v0.1.34 release (2,823,968 B):
the choice module, the probe's two extra reads, the gate and the UI
texts in eight languages. Above the 30 KB line agreed for one change,
by 1.4 KB; reported with the handover.

## Audit

An independent read-only review of the branch (2026-10-22, every area:
power path, verdict, threading and flash, the update on the fleet, the
probe, API and web UI, docs) found twelve points; all are in this state
of the branch:

1. **High.** Every fleet board (no `boardok`) would have gone through the
   gate and the count at the update; one with a dead chip or a dead
   sensor would have been switched off and kept off. Fixed: a
   `boardModel` from an earlier version is taken as verified; only a
   choice made under this firmware (marked `pending` in NVS by the
   detection and by PATCH) is checked. And the count refuses only
   another board's count (more than the profile has, or exactly a
   sibling's); fewer chips verify with a warning.
2. The cautious voltage took a configured point above the profile's
   bring-up (a Turbo at 1250, a Rev 7's 1300 mV left in the config for a
   Nano). Fixed: capped at the bring-up voltage; a point outside the
   profile's window is reset to the factory point at that start.
3. The configured `asicCount` could make a NerdQAxe++ profile verify on
   an Octaxe. Fixed: the verdict uses the profile's own count.
4. The `READ_VIN` class check refused every TPS546 board on an odd
   reading, though only the NerdAxe Gamma / Gaia pair needs it. Fixed:
   applied only where two profiles share their parts across classes.
5. The `refused` event never reached flash (no board thread to flush it).
   Fixed: flushed on the main task in `gate`.
6. `POST /api/v1/chips/on` claimed success while the chips stayed
   blocked. Fixed: 409 with the choice state.
7. Three failed inits refused the choice even when the regulator had not
   come up. Fixed: no verdict without the regulator; the choice stays
   pending and the board error shows.
8. A `PATCH coreVoltage` during the first start bypassed the cautious
   cap. Fixed: capped in `request_core_mv` while pending.
9. The configured voltage was applied at `Enumerated`, during the chip
   init. Fixed: `chain_ready` applies it after the init, as before.
10. Values the UI inserts from `reason` and `detail` were not escaped
    (firmware constants today). Fixed: `esc()` on each.
11. Docs claims: the cautious cap (now true), the fleet's "no change"
    (now true by the migration), the devkit-on-NerdNOS wording (the
    chip is unpowered, not held in reset), the GPIO13 pulse (noted
    above). `verify_choice` logged at warn level (now info).
12. The choice state was a mutex read in the miner's loop. Fixed: an
    atomic.

Clean: the ordering of the power path (no pin, regulator write or
voltage before `gate`; a refused choice cannot be powered through
`chips/on`, the overheat reset or the board loop), the verdict mechanics,
threading and flash (NVS writes on the main task, the miner thread and
the HTTP task only; no lock-order cycle), the probe's read-only
transactions and the other pin pair on every board, the API shape against
the web UI, the eight languages. Not verifiable on the host: the Nano's
and the Gamma's `READ_VIN` (no longer judged, but logged in the probe
line), the Gaia's EMC2101 answering with the LDO undriven (the pre-change
detection relied on the same probe and found it).


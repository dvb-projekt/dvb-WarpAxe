# Self-test, diagnostics file and event log

Written 2026-10-21 (branch `feat/selftest`, on v0.1.33, for v0.1.34),
after Oliver asked for a guided test with one summarised result, as
AxeOS has for the factory, and then for a file a user can send to
support and for a log of what happened to the miner across restarts.
WarpAxe already checked most of the self-test's points while mining
(chips against the profile, silent chips, hash counters per chip and
domain, error rate, regulator status, plausibility of every reading,
a lost sensor, a stalled fan, the input window); what was missing was
one list with ✓/✗ per point and PASS or FAIL at the end.

## The reference: ESP-Miner's self-test

ESP-Miner v2.15.3, `main/self_test/self_test.c` (line numbers of that
file), read as a yardstick, not copied.

- **When it runs** (`self_test_should_run`, 290–301): after a factory
  flash (`bestDiff` still 0 and the NVS flag `selftest` set) or when the
  boot key is held at power-on. It then runs *instead of* mining:
  `main.c` skips Wi-Fi, the REST server and the stratum task while
  `SELF_TEST_MODULE.is_active` (lines 154, 177, 215–224), feeds the chips
  one mock job (543–557, a hard-coded `mining.notify`) at difficulty 16
  (50, 309) and stops at the first failure (`tests_done`, 764–820:
  reset held low, core voltage off, "SELF-TEST PASS!" and a restart
  after 10 s, or "SELF-TEST FAIL!" until the boot key is held for 2 s).
- **What it checks, with its limits:**

  | Check | Limit | Lines |
  |---|---|---|
  | PSRAM present | | 481–485 |
  | DS4432U (old Bitaxe boards) answers | | 488–492 |
  | input voltage (INA260 boards only) | nominal ± 10 % (`INPUT_VOLTAGE_MARGIN`) | 45, 421–440 |
  | regulator faults | none (`VCORE_check_fault`, `power_fault`) | 442–451 |
  | temperature sensor | a reading (not −1, not 127) | 193–223 |
  | warm-up | waits at 10 % fan until the chip reaches `selftest_warm` 55 °C, then a PID holds `selftest_temp` 65 °C for the test; above `selftest_max` 70 °C: fail | 559–576, 578–586, 620–625; defaults `nvs_config.c` 111–113 |
  | hashrate, 30 s | nonce rate ≥ `hashrate_test_percentage_target` × small cores × clock (0.85 for every chip but the BM1368's 0.80; `device_config.h` 118–124) | 588–594, 737–741 |
  | domains | every domain's 30-s average within ± 33 % of its share (`SELF_TEST_DOMAIN_HASHRATE_TOLERANCE`); a sample above 3× the share is implausible and dropped, a counter with ≥ 25 % such samples is "unreliable" (a warning, the nonce rate decides) | 35–36, 129–168, 649–735 |
  | core voltage | default voltage ± 10 % (`SELF_TEST_CORE_VOLTAGE_TOLERANCE`) | 38, 405–419 |
  | power | ≤ `power_consumption_target` + 3 W per board (`device_config.h` 159–183, e.g. Gamma 22 W, Gamma Turbo 36 W); multiphase regulators: every phase ≥ 1 A | 40–42, 371–403 |
  | fan | tach > `selftest_fan` 1000 rpm, or the board's `self_test_fan_target_rpm` (500 on the Gamma Turbo, Naja Duo and Gamma Hex), at whatever duty its PID holds | 353–369 |

- **What it shows** (`main/screen.c` 263–274, 703–715): one screen,
  "BITAXE SELF-TEST", the current message ("…Gh/s …°C …s" while
  hashing, "FAN:WARN", "ASIC 1 DOMAIN:FAIL", "VCORE:FAIL" …), then the
  verdict and what to do. Nothing in the API: the server is not started.

## What WarpAxe does

The judgement is `crates/board/src/selftest.rs` (no_std, 13 host tests)
and is driven against the simulated chain in `crates/chain/tests/sim.rs`
(`selftest_window`, 5 tests: a healthy chain, a hung chip, a chip that
stops answering, a weak domain, hash errors). The firmware only collects
(`firmware/warpaxe/src/selftest.rs`) and reports (`api.rs`, `screens.rs`).

**Trigger:** the "Run self-test" button under System, or `POST
/api/v1/selftest` (202, or 409 while one runs). `GET /api/v1/selftest`
follows it and keeps the last result until the next run or a reboot;
`DELETE` cancels. `/api/system/info` carries `selfTest: {state, id,
passed, finishedAt, finishedUptime}` so a tuner or a dashboard sees a
running test without polling the detail (WarpTune v1.42 holds its writes
while `state` is `running`; Warppool #2, 2026-10-21).

**It runs while the miner mines**, at the configured clock and voltage,
no restart, no mock job; about 80 s:

1. a snapshot of the board. Since v0.1.42 a test that finds the chain
   not up waits up to 45 s for it (chips that were off for want of a
   pool come back on for the test), and one that finds no work from a
   pool waits up to 10 s for some (a session between two connects);
2. the hash window: 60 one-second samples of every chip's total, error
   and domain rates from the chips' own counters, as the miner already
   reads them for `hashrateMonitor`. Averaging over the window is what
   makes the domains usable: a chip's counters are read in turn, so one
   second shows 65–125 % of a domain's share while a minute shows
   97–104 % (fleet, 2026-10-21: 60-s averages 99–101 % per chip and
   domain on every BM1370 and BM1373, 88 % on the BM1397's rate
   register). A chip whose last counter reading is older than 5 s has
   stopped answering and counts as hashing nothing. Without work from a
   pool there is no window, and a window during which the pool sent no
   work for more than 5 of its 60 s is discarded (a reconnect that
   needs a second attempt is enough for that): the three hash checks
   are then skipped, see below;
3. the fan step: both outputs held at least at 100 % for 10 s, through
   the board task's own loop (`board::fan_boost`), the speed read before
   and at the end.

**The checks and their limits** (`Limits::DEFAULT`):

| Check | PASS when | Skipped when | From |
|---|---|---|---|
| chips | the chain hashes and found = expected (config `asicCount`, else the profile) | the board has no chain (devkit) | ESP-Miner stops at a mismatch |
| hashrate | every chip's window average ≥ 80 % of small cores × clock | the chain is up but no pool sent work (v0.1.42) | ESP-Miner wants ≥ 85 % from nonces; the fleet's counters sit at 99–101 % (BM1370/73) and 88 % (BM1397), so 80 % leaves the BM1397 room and still catches a chip at half rate |
| domains | every domain's average ≥ 67 % of the chip's share; samples above 3× the share dropped | the chip has no domain counters (BM1397); no work from a pool | ESP-Miner's 33 % tolerance and 3× plausibility |
| errors | every chip's error rate over the window ≤ 5 % (BM1370), ≤ 8 % (BM1373) | no error counter (BM1397); no work from a pool | ESP-Miner has none; the fleet runs 0–1.3 % (BM1370 at stock and max points), 2.6 % at WarpTune's eco point (the Nano), 3–6 % (BM1373, docs/05). The rate rises as the voltage drops, so it is a tuning figure as much as a hardware one: the line sits well above any tuned point, and the measured value is always in the report, so a 2.6 % reads as what it is |
| coreVoltage | the regulator's output within ± 5 % of the set value | no regulator (NerdNOS, devkit) | ESP-Miner ± 10 % of the default; the fleet reads back within 0.5 % |
| regulator | no fault condition (AxeOS `power_fault` as a fault, or a stop with cause `power`; a temperature warning is not a fault) | no regulator | ESP-Miner `test_vreg_faults` |
| fan | ≥ 500 rpm at 100 %, and at least 10 % faster than before when the duty before was under 90 %; a second output is judged only when something spins on it, an empty header is a note | no fan the firmware drives (NerdNOS, devkit) | ESP-Miner's lowest floor (500); the reaction is new |
| temperature | chip and regulator read, both under their stop lines (docs/08) | no sensor | ESP-Miner fails at a missing reading and above 70 °C |
| inputVoltage | within ± 10 % of the board's nominal input | no reading (NerdNOS) | ESP-Miner `INPUT_VOLTAGE_MARGIN` |

Not checked, on purpose: **power** against a per-board target. A tuned
miner runs at any point; the regulator's own over-current limit guards
the board, and a figure that fails at an honest 700 MHz would teach the
wrong lesson. **PSRAM** and the DS4432U: the first is a build-time
certainty on the ESP32-S3R8 images, the second a part no WarpAxe board
carries.

**Chips off, overheat stop, bring-up failed:** the chip checks fail with
the note `notHashing` rather than being skipped. A test is asked "does
the miner work"; a skip would say PASS with the chips off.

**No work from a pool** (v0.1.42) is another matter: the chain is up,
every chip was found, and there is nothing to hash through no fault of
the board's. Hashrate, domains and errors are skipped with the note
`noWork`; the chip count (from the enumeration), voltages, regulator,
fan and temperatures are judged as ever, and the run passes when they
do, with six checks judged instead of nine. This is the case of a miner
whose pools are out (the chips come back on for the test) and of the
first start's own test before a pool is entered. The stored result and
`selfTest.passed` do not say that checks were skipped; `judged` and the
checks' own `status` do.

**Result:** `GET /api/v1/selftest` → `state`, `passed`, `judged`,
`failed`, `durationS`, `frequency`, `coreVoltage`, the 60-s figure per
chip (`chips[]`: GH/s, percent of nominal, error percent, the domains'
percent of their share) and `checks[]`, one per check in a fixed order
with `status` (`pass`, `fail`, `skip`), the figure it rests on (`value`,
`limit`, `value2`, `limit2`, meaning per check in the module docs of
`crates/board/src/selftest.rs`), the worst `chip` and `domain` where
per chip, and a `note` (`notHashing`, `noWork`, `noReading`, `noRegulator`,
`noFan`, `noSensor`, `noCounter`, `noDomains`, `noReply`, `noSamples`,
`noReaction`, `regulatorFault`, `overheat`, `secondFanMissing`). The web
UI lists every check with its figure, the device display shows the
running test (progress, step) and then PASS/FAIL with the nine marks for
five minutes or until a key is pressed (LCD: two columns; OLED: the
verdict and the first failed check, or "9/9 OK").

**Safety:**

- The test never touches the clock or the core voltage. If either
  changes during the window (a tuner's PATCH), the test ends as
  `aborted` with `abortReason: pointChanged`: a window across two
  points would judge neither.
- Every stop line stays in force: the board task is not told anything
  about the test. An overheat stop during the window ends the test
  (`chainStopped`).
- The fan step only raises the duty: the board task takes the larger
  of what its loop wants and the boost, for both outputs, so a manual
  setting or the warm-up hold are never undercut. The boost carries a
  deadline (15 s for a 10-s hold) and the board task drops it by itself
  when the time is up, so a self-test thread that died cannot leave the
  fans pinned; the thread releases it on every exit path too (a drop
  guard). The PID keeps computing against the real temperature
  meanwhile and takes over again on the next pass.
- The thread runs on a PSRAM stack (16 KB; the window and the
  report on the heap) and writes no flash; the result goes into the
  event log through the queue below.

## The diagnostics file

`GET /api/v1/diagnostics`, the "Download diagnostics file" button under
System (and on the crash banner), with or without a self-test before.
One JSON document, pretty-printed: readable in any editor and parsed by
any tool, with the same field names as `/api/system/info` so support
reads it without a second vocabulary. Plain text was the alternative
and would have needed its own format for the hash counters per chip and
domain; JSON costs 6 KB of image for the pretty printer.

Sections, in order: `file` (what it is, when, uptime, reset reason,
whether the last run crashed and its summary, whether a core dump is
stored and how big, the install record, the heap figures including
`largestFreeInternal`), `info` (everything `/api/system/info` says:
firmware and gitHash, board, chips, clock and voltage set and measured,
hash counters per chip and domain, error rate, pool status, reject
reasons, regulator status and `power_fault`, overheat stops, fans,
temperatures), `selfTest` (the last result as above), `events` (the
event log), `chain` (the chain driver's counters, the last nonce, the
silent-chip figures), `pool` (reconnects, reject reasons, difficulty,
shares), `config` (the settings, redacted), `savedPools`,
`configPatches` and `log` (the in-RAM ring, up to 128 lines).

**The core dump** stays on the miner: an ELF of up to 64 KB (the
partition) in a file meant for a chat is too much, and support needs
`espcoredump.py` for it anyway. The file says that one is stored and
how big; the button next to the diagnostics button downloads it
(`GET /api/v1/coredump`).

**Privacy** (`crates/diag/src/scrub.rs`, 10 host tests):

- No password is ever written. The `config` section goes in through
  `redacted()` and then loses the three password fields altogether; the
  saved pools carry no password field.
- The worker's address is masked to `bc1q…5mdq.worker` (first four, last
  four, the worker name kept, `mask_worker`); the Wi-Fi name to its
  first letter and its length, `F…8` (`mask_ssid`).
- Every section goes through the scrubber before it is written: the
  Wi-Fi password, both pool passwords, the saved pools' passwords and
  the setup hotspot's password are replaced by `***` wherever they
  appear, the addresses and the Wi-Fi name by their masked forms,
  longest first, in every string *value* of the section (keys stay, so
  a network called "pool" cannot rewrite the file's keys). The log
  excerpt is a section like any other, so a line that ever printed a
  secret is caught the same way. A secret shorter than four characters
  is not searched for (the pool default "x" would eat the text); the
  firmware prints no password anywhere, so this is the second line, not
  the first.
- After scrubbing, a section that still contains a secret is not sent:
  a marker takes its place and the log says which. Cannot happen by
  construction after the replacement, kept as the assertion it is.

## The event log

`crates/diag/src/events.rs` (the ring, 4 host tests) and
`firmware/warpaxe/src/events.rs` (the queue and the store). One NVS
string under `events` beside the config patches: the last 32 events,
each `{t, u, k, d}` (unix seconds or 0 before NTP, uptime seconds, kind,
one line of detail up to 48 characters), at most 3200 bytes (an NVS
string holds 4000). Shown under System (newest first), in
`/api/v1/state` (`events`), `/api/v1/events` and the diagnostics file.

| Kind | Written when | Detail |
|---|---|---|
| `boot` | every boot, on the main task | the reset reason |
| `rollback` | at boot, the image state is `aborted` | back on the previous image |
| `update` | at boot, the image state is `pending-verify` | the install record (`v0.1.34 verified`) |
| `overheat` | the board task stops the chips | chip and regulator temperatures with their limits, or "no chip temperature for N s" |
| `power` | a regulator fault stops the chips | the fault conditions |
| `pool` | the stratum thread switches to the fallback, back to the main pool; a saved pool is made the main pool | which |
| `chain` | a chip goes silent (at most once per 10 min per chip); every chip silent for a minute and the chain restarted | which chip; the voltage |
| `selftest` | a run finishes | `PASS, 9 checks` or `FAIL: hashrate chip 2 61 %, fan 0 rpm` |

**Flash wear:** every write stores the whole string again (NVS writes a
new entry; the partition is 24 KB, six pages, one reserved for garbage
collection, each page holding 126 entries of 32 bytes; a 3 KB string
takes ~100 of them, so a write costs most of a page). Writes happen on
events only, never on a timer: a boot, a stop, a pool switch, a
self-test. The ring drops a repeat of the last event within a minute
without writing, and the chain module rations the silent-chip event to
one per ten minutes per chip, so a chip that flaps cannot write every
few seconds. A miner that runs well writes once per boot. At one write
per minute around the clock the partition's pages would reach the
flash's 100 000 cycles after about a year; at the rates above, never.

**Thread origin:** a flash write from a thread whose stack is in PSRAM
reboots the device (v0.1.5). `events::record` therefore only queues
(any thread, including the self-test's); `events::flush` writes, and it
is called from the board task's loop (once a second, after the sample),
the miner loop (with the counter read) and the HTTP handlers of
`/api/v1/selftest`, `/api/v1/events` and `/api/v1/diagnostics` (the
HTTP server's task has an internal 12 KB stack), all internal. The main
task writes the boot event itself before any thread exists. A board
without a board task or a miner loop (the devkit) is flushed by the
handlers when the page is open; nothing is lost meanwhile, the queue
lives in RAM.

## After a crash

`/api/system/info` says `crashed: true` when the reset reason was a
panic or a watchdog (`diag::last_run_crashed`; a software restart, an
update, a power-on or a brown-out are not crashes) and gives a `bootId`
(random per boot). The web UI shows a banner on every page once per
boot: the reason, the time (from `unixTime − uptimeSeconds`), the two
download buttons (diagnostics, and the core dump if one belongs to this
crash) and "Dismiss", which remembers the `bootId` in the browser's
localStorage. The next crash is a new `bootId`, so the banner comes
back.

### A core dump that is not this crash

A core dump stays in the `coredump` partition until it is erased or the
next crash overwrites it, and not every crash writes one: the panic
handler writes the dump with `esp_flash_write`
(`espcoredump/src/core_dump_flash.c`), which cannot run while a flash
operation is in progress on the other core, and an interrupt watchdog
is exactly what a flash operation that overran produces. The BitForge
Nano .44 came back from an `int-wdt` on 2026-10-21 10:05Z with a dump
whose app ELF SHA (`d7676a381`) matched no build of that day and no
release v0.1.21 to v0.1.33 (checked against the app descriptors of the
release assets): a dump from an earlier test image, which
`log_core_dump`, `lastCrash`, `coreDumpBytes` and the new banner all
presented as the crash at hand, including a wifi-task assert backtrace
that had nothing to do with it.

Since this branch a dump counts as the crash's own only when both hold
(`diag::check_core_dump`, on the main task at boot):

- it was written by this image: the dump's app ELF SHA (the core dump's
  `ESP_CORE_DUMP_INFO` note, `esp_core_dump_summary_t.app_elf_sha256`)
  starts with the running app's (`esp_app_get_elf_sha256`); and
- no earlier boot event names it: the boot event's detail carries the
  dump's tag `dump:<sha9>@<pc>` (`panic dump:fcfbb6da5@4037732c`), so
  the same dump after a later crash of the same image reads as old
  (`int-wdt no dump (dump:fcfbb6da5@4037732c old)`).

Only then does `lastCrash` carry the summary, `coreDumpCurrent` is true,
the banner offers the dump and the diagnostics file calls it "from this
crash"; otherwise the banner says no dump was written, the Diagnostics
card calls the stored one older, and the file says so too. The dump is
never erased by the firmware: an old one may still be worth reading.

Two ELF SHAs for the same commit are normal, by the way: `esp_app_desc.c`
carries `__DATE__`/`__TIME__`, so every rebuild changes the SHA; the
code and its addresses stay. A backtrace from a dump can only be read
with the ELF of the build that was flashed, so `espflash save-image`
and the ELF belong together in the release folder.

## Tests

- `warpaxe-board` `selftest::tests` (13): every check's pass and fail
  side, the per-chip-type error limit, the BM1397 without counters, a
  board without regulator and fan (the NerdNOS), chips off as failures
  not skips, the fan's floor and reaction, the empty second header as a
  note, the report's order and ids.
- `warpaxe-chain` `tests/sim.rs` `selftest_window` (5): the firmware's
  window driven against the simulated chain, with the simulator's new
  `Fault::WeakDomain` (one domain at a third of its share: the domain
  check names chip 1 domain 3, the hashrate check still passes).
- `warpaxe-diag` (14): masking, scrubbing including inside a log line,
  values-only scrubbing with the keys kept, the leak check; the ring's
  round trip, coalescing, capacity and size, a damaged string.
- The web UI against a mock server (scratchpad, not in the repo): the
  System page with the Self-test and Diagnostics cards, the running
  state, a PASS and a FAIL result, the event list, the crash banner; no
  console errors. The display screens from `tools/display-preview`
  (`lcd-ev-selftest-*`, `oled-ev-selftest-*`, with the real judgement on
  sample readings): `docs/screenshots/lcd-selftest.png`,
  `oled-selftest.png`.
- **The first device run** (dvb-WarpAxe #2, 2026-10-21, .42 Bitaxe
  Gamma on the test stand, image 69d4fcae1 = this branch plus main):
  `POST /api/v1/selftest` answered 202 with an empty body, the next
  requests came back empty, and the miner restarted with reset reason
  `panic` and no core dump; the event log recorded `boot: panic`. That
  is what a stack overflow looks like:
  `CONFIG_FREERTOS_CHECK_STACKOVERFLOW_CANARY` aborts at the next
  context switch. (This note first said the core-dump writer cannot read
  a PSRAM stack with the cache off; it can, our dumps carry PSRAM
  segments. The dump was lost because this firmware's dumps are 60–64 KB
  and the partition was 64 KB: docs/12.) The self-test thread had
  8 KB, and the hash window (`Window`, 1.3 KB: 16 chips × 8 domains of
  sums and counts) sat on that stack several times over as it was
  built, returned and matched on, next to the cloned board, miner and
  config states and the log formatting. Since the fix ("selftest: the window and the report on the heap …") the window lives in a
  `Box`, the thread has 16 KB, and the log line at the end says how
  much of it was never used, so the next run measures the real need.
- **The second run on .42** (image 4c0ec36 with the fix, Oliver's OK,
  dvb-WarpAxe #2): PASS, 9 checks, 73 s, no restart. `10240 of 16384
  bytes of the thread's stack never used`: the peak is about 6 KB, so
  the 8 KB of the first run were too little even with the window on
  the heap in reserve; 16 KB stays (PSRAM, nothing else wants it).
  The figures at 575 MHz / 1060 mV: chips 1 of 1, hashrate 98.7 %,
  domains 97.6 %, errors 0.04 %, core 1056 mV (set 1060), regulator
  clear, fan 2446 → 4707 rpm at 100 %, chip 62.1 °C, input 5.19 V.
- **The other three boards** (same day, image 4c0ec36, all PASS, no
  restart; "never used" 10 244–10 468 of 16 384 bytes, so the peak
  stays at about 6 KB on every board):

  | Board | Point | Chips | Hashrate | Domains | Errors | Core | Fan at 100 % | Chip / VR °C | Input |
  |---|---|---|---|---|---|---|---|---|---|
  | .41 NerdQAxe++ | 675 MHz / 1170 mV | 4 | 96.2 % | 94.8 % | 1.02 % | 1169 mV | 1128 → 2839 rpm, M1 empty (`secondFanMissing`, a note) | 55.1 / 48 | 12.06 V |
  | .44 BitForge Nano | 425 MHz / 1000 mV | 2 | 99.1 % | 97.0 % | 2.59 % | 996 mV | 2018 → 5079 rpm (both tachs) | 54.4 / 63 | 12.29 V |
  | .40 NerdAxe Gaia (BM1373) | 350 MHz / 950 mV | 1 | 101.7 % | 99.1 % | 4.71 % | 945 mV | 5493 → 7814 rpm | 60.6 / 69 | 12.08 V |

  The Nano's 2.59 % sat just under the 3 % line the BM1370 had: it
  runs at WarpTune's eco point, where a lower voltage raises the error
  rate, and nothing is wrong with the chips. The line is 5 % since
  then (`Limits::DEFAULT`): a self-test asks whether the hardware
  works, and a figure that fails at an honest eco point would say the
  wrong thing; a chip with a real problem errs far beyond that (the
  value and the worst chip are in the report either way, and
  `errorPercentage1m` is what a tuner acts on). A third verdict
  ("warning") between pass and fail was the alternative; it would have
  added a status value to the API that WarpTune v1.42 already reads,
  and a colour to every list, for one check.

## Image size

v0.1.33 (0fab581): 2,875,968 bytes. With everything here: 2,953,216
bytes, **+77.2 KB** (above the 30 KB the handover asked to be
reported): the judgement and its report, the diagnostics builder (the
`json!` sections and the pretty printer, ~6 KB), the scrubber and the
ring with serde, the self-test screens, and the 77 web-UI strings in
eight languages plus 18 display strings in five (~12 KB of HTML, gzipped
into the image). 127 KB remain below the 0x2F0000 import window
(`tools/release-firmware.sh`); the OTA slot has 192 KB left.

## Audit

Read through once more with the three things that can go wrong in
mind: the fan, flash, secrets.

- **The fan step cannot lower the duty.** The boost is applied as
  `max(want, boost)` on both outputs after the loop computed `want`,
  including the overheat and warm-up cases that already want 100 %.
  There is no path that sets a lower duty than the loop wants.
- **The boost cannot stay on.** Three independent ends: the self-test's
  drop guard on every return (`?`, cancel, error), an explicit release
  after the read, and the deadline in the board task (`fan_boost_now`
  clears an expired boost on the next pass, 100 ms later). A
  self-test thread that is killed mid-hold leaves the fans at 100 % for
  at most 15 s.
- **No clock or voltage is touched**: the self-test calls nothing but
  `board::fan_boost` and readers. A change from elsewhere ends it.
- **No flash from a PSRAM thread**: `selftest` records through the
  queue; the writers are the board task (internal, 6 KB), the miner
  task (internal, 16 KB), the HTTP task (internal, 12 KB) and the main
  task. `config::nvs_set_str` is the one new flash writer and is called
  from `events::flush` only.
- **No periodic write**: `flush` writes only when the ring changed; an
  empty queue costs a mutex lock. The ring's coalescing and the
  silent-chip rationing bound the rate; the boot event is the only one
  guaranteed per boot.
- **No secret in the file**: passwords are removed from the config
  section and replaced in every string value of every section; the leak
  check runs on the serialised section and drops it rather than send
  it. The hotspot password is in the scrubber for the time the hotspot
  is up (it is shown on the display only, never in the API). The
  `info` section is `system_info()` as the API serves it, which carries
  no password by design (`config.redacted()` is what `/api/v1/state`
  serves too). A MAC address and LAN addresses in `configPatches` stay:
  they identify the miner on its own network and are what support asks
  for first.
- **A second self-test is refused** (409) while one runs; a cancel is a
  flag the thread sees within 100 ms.
- **Handler slots**: about 51 of 64 in use after the five new routes
  (44 routes plus the captive portal's seven; the boot log line
  `api: … handlers` is authoritative).
- **The display**: the result screen is dismissed by a key; a key while
  the test runs hides the progress screen, and the result still shows
  when it is in (acknowledged per run id and state).

Open for Oliver: the limits (80 / 67 / 5 and 8 / ±5 / ±10 / 500 rpm)
before the first device runs; whether the NerdOS boards' `CHIPS OFF`
display wording should follow the web UI (unrelated, noted by the
Portuguese proofreader); and whether a self-test should be offered on
the first start of a board (ESP-Miner's factory case), which this does
not do.

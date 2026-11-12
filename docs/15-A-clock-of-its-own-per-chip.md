# A clock of its own per chip

Written 2026-11-12 (branch `claude/warpaxe-per-chip-takt-c5dd77`, after
v0.1.43), on a handover from dvb-WarpAxe #2 for Oliver. Built and tested
on the host and against the simulator. The first device runs the same
evening (by dvb-WarpAxe #2) changed three things, told in *What the
devices said*: the mining loop's stack, where its state lives, and
**the address a single BM1373 answers to**, which both reference
drivers have wrong. The runs with offsets on the NerdOctaxe (BM1370)
and, with the address put right, on the Naja Duo are still to come (the
test list is at the end).

## Why

The chips of one board are not alike. All of them get the same core
voltage and, until now, the same clock, so the weakest chip sets the
voltage for the whole board. A NerdOctaxe (eight BM1370 on one rail) at
625 MHz, hash errors per chip over 17 minutes each (2026-11-12, measured
by WarpTune):

| Core voltage | Chip 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | Board |
|---|---|---|---|---|---|---|---|---|---|
| 1170 mV | 0.64 % | 0.00 | 0.10 | 0.00 | 0.10 | 0.02 | 0.01 | 0.00 | 10.14 TH/s |
| 1150 mV | 0.81 % | 0.00 | **1.08** | 0.00 | 0.11 | 0.18 | 0.24 | 0.01 | 10.16 TH/s, 177 W |
| 1140 mV | 0.81 % | 0.04 | **1.80** | 0.00 | 0.13 | 0.62 | 0.62 | 0.21 | 10.09 TH/s, 174 W |
| 1130 mV | 0.84 % | 0.30 | **2.16** | 0.02 | **5.66** | 1.48 | 1.19 | 0.03 | 9.24 TH/s, 168 W |

Chip 2 limits the voltage (the BM1370's 1.5 % line lies between 1150 and
1140 mV), chip 4 is next (one of its hash domains latches at 1130 mV),
chips 1, 3 and 7 have room to spare. With chips 2 and 4 at about
575–600 MHz and the rest at 625 MHz the board could run at 1130 instead of
1150 mV: an estimated 5 % less power for 1 % less hashrate. A NerdQAxe++
shows the same picture (chip 3 at 0.99 % against 0.08–0.22 %). On a Naja
Duo (two BM1373 in series, the midpoint not regulated) a lower clock on
one chip also shifts the voltage towards it, which is a use of its own
once the midpoint has been measured.

An earlier look (2026-10-02, at 650 and 700 MHz) found no gain at the
points measured then; the figures above, at 625 MHz and lower voltages,
are what reopened the question.

## What the firmware does, and what it does not

The firmware provides the mechanism: an offset per chip, stored, checked,
carried out without a chain restart, reported, and taken into every figure
that depends on a chip's clock. It does **not** search for the offsets;
that is WarpTune's part (coarse steps of 20–25 MHz to the error limit, then
5 MHz back). An offset stays as it was set. With no offsets the firmware
behaves exactly as before, on the wire too (see *Tests*).

## The setting

`chipFreqOffsets`: a list of whole MHz, one per chip in chain order (from
0), added to `frequency`. Negative for a slower chip, 0 for none.

```
PATCH /api/system   {"chipFreqOffsets": [0, 0, -25, 0, -50, 0, 0, 0]}
PATCH /api/system   {"chipFreqOffsets": []}          no chip has an offset
```

- Only on boards with more than one chip (`chipFreqOffsetsSupported` in
  `/api/system/info`): the BM1370 boards and the Naja Duo. On a one-chip
  board a list with an offset in it is refused with 400.
- A list longer than the board's chain is cut, a shorter one is filled
  with zeros, a list of zeros is no list. Stored is either nothing or one
  entry per chip.
- It can come alone or with `frequency` and `coreVoltage` in one PATCH.
- Kept across restarts in its own NVS key (`chipoffs`, 33 bytes), not in
  the config document: a tuner changes it often, and a write is skipped
  when the list is the one in force.
- A PATCH that names another `boardModel` drops the stored offsets. A list
  that does not have one entry per chip that answered (a chip missing from
  the chain, another board) is not applied: every chip then runs at the
  chain's clock, with a line in the log, and the list stays stored.

### Limits

The whole PATCH is refused with status 400 and a message naming the chip
when any chip's own clock (`frequency` + its offset)

- is above the board's highest clock (`absMaxFrequency`),
- is below half of `frequency` (rounded up) or below 50 MHz,
- has no PLL setting,

or when, on a board whose chips are in series (Naja Duo, Gamma Hex,
NerdQAxe++ Rev 7), the largest and the smallest offset lie more than
25 MHz apart (8 % of `frequency` where that is less): there a slowed
chip takes voltage from the others, see *What the devices said*.

The same check runs when `frequency` alone is changed, against the offsets
that stay. Half the chain's clock is far below anything a search needs
(the chips of a board lie within a few per cent of each other); it keeps a
slip of the hand from parking a chip at next to nothing.

### What is reported

`GET /api/system/info`:

| Field | Meaning |
|---|---|
| `chipFreqOffsetsSupported` | whether this board takes offsets at all |
| `voltageDomains` | voltage domains in series behind the regulator: 1 = every chip on the full core voltage, more = chips in series |
| `chipFreqOffsetSpread` | how far apart the chips' clocks may lie on a board with chips in series, MHz at the current clock (25, or 8 % of it where that is less); 0 = no such limit |
| `chipFreqOffsets` | the stored list, MHz; `[]` = none |
| `asicFrequencies` | the clock each chip runs at now, MHz on the PLL's grid, in the order of `hashrateMonitor.asics` |
| `expectedHashrate` | the sum over the chips, each at its own clock |
| `hashrateMonitor.asics[i].counterRestarts` | times the chip's hash counter went back since the chain started: a chip that reset itself; `hashCount` and `errorCount` start over with it |
| `hashrateMonitor.asics[i].hashCount` | the chip's total hash counter as read, one step per 2^32 hashes like `errorCount` (wraps at 32 bits, starts over with the chain): two readings give the chip's rate without the noise of `total` |
| `frequency`, `frequencyActual` | the chain's clock, as before |

The PLL has a grid: an offset of 10 MHz at 600 MHz gives 609.375 MHz.
`asicFrequencies` shows what the chip really runs at, also in the middle of
a ramp. When a stored list is not applied, `asicFrequencies` shows the
chain's clock on every chip.

## How a chip gets its clock

The chain driver keeps the clock each chip was told last and the clock it
is to have. Every 100 ms each chip that is not there yet moves one step of
6.25 MHz, as the clock ramp always has.

- While all chips move together (always, without offsets) the step is one
  write to all chips, byte for byte what it was.
- Chips that part ways are written to one by one, each at its own
  address (0x00, 0x10, … on a BM1373; not the shifted address the
  reference drivers use there, see *What the devices said*). A chip
  whose clock does not change is not written to.
- A chip profile says whether a write to one chip reaches that chip
  alone (`ChipProfile::single_chip_clock`: seen on the BM1370 and the
  BM1373). Where it does not, the driver acts on no offset: every chip
  stays on the write to all.

**No chip is ever taken above the higher of the clock it had and the clock
it is to have.** A write to all up to the chain's clock, followed by the
chip's way back down, would run a chip with a negative offset above its
clock for the length of that way, and a chip that gets a lower clock
because a domain latches above it would latch right then (a latch stays
until the chips are switched off and on). So at bring-up the chips ramp
together only up to the lowest clock any of them is to have, and from
there each goes on alone; a new chain clock moves every chip by its own
steps.

Three things the driver sizes by the chips' clocks:

- the hash counting number (BM1370) and the time until a job's nonce space
  runs out are for the **fastest** chip: a slower chip scans less than its
  share per job, a faster one than the number allows for would return
  nonces twice;
- the silence watchdog expects each chip's nonces at that chip's clock.

Single-chip register **reads** of a BM1373 stay by broadcast, as both
reference drivers do them; nothing here reads a single chip.

## A lower voltage waits for the chip's ramp

A PATCH with a lower clock and a lower voltage has always held the voltage
back until the clock has come down (`core_hold`). An offset is a clock
change the chain's own clock does not show, so the same hold now covers
it: from the moment new offsets are asked for until every chip has
arrived, a voltage **drop** waits (30 s at most, as before; a raise is
never held, and nothing is held while no chain runs).

## What follows the chip's own clock

- **Expected hashrate** (`expectedHashrate`, `yieldPercent`, the
  efficiency history's floor, the display's yield line): the sum over the
  chips.
- **Self-test**: each chip's rate and each of its domains are judged
  against small cores × that chip's clock. New offsets during the hash
  window end the run as `pointChanged`, like a new clock or voltage.
- **One chip silent** and **one hash domain stuck**: a chip given another
  offset is at a new point; its restarts and resets count from zero and a
  chip or domain given up gets another chance, that chip alone. (The latch
  check compares a domain with the other domains of the same chip, so the
  clock itself drops out of it.)

The nominal rate told to the pool at connect stays chain clock × chips; a
few per cent off for a few chips is of no consequence there.

## Web UI

Settings, section clock and voltage, on boards with more than one chip: a
fold-out *Clock per chip* with a field per chip (± MHz, steps of 5), and
under each field the chip's clock and its error rate over the last minute
(worked out in the page from the chips' raw error counters). The title and
the status line are in all eight languages.

## Tests

`cargo test --workspace`: 679 tests (655 before).

- `crates/chain/tests/chip_clock.rs`, against the simulator:
  - **nothing changes without offsets**: for eight chains (one, two, four
    and eight BM1370; BM1373 under NerdOS's and both ESP-Miner inits;
    BM1397) a bring-up, mining, ramps up and down, a request in the middle
    of a ramp and one without a PLL setting put the same bytes on the wire
    at the same moments as the driver of main `c24b901` did (digest and
    frame count recorded there);
    the two rows of two BM1373 under ESP-Miner's init were changed on
    purpose, once: chip 1's domain-end write goes to 0x10 now; the one
    BM1373 of a Gaia did not move;
  - a chain of chips that cannot be written to alone takes no offsets
    (a list changes not one byte on the wire);
  - what the Naja Duo did while the driver shifted a BM1373's address
    (an offset for chip 0 at a chain start slowed both chips, one for
    chip 1 neither), and what it does at the full address;
  - `Chain::renew` is `Chain::new`, field for field and on the wire;
  - four and eight BM1370 and two BM1373 under both inits: an offset moves
    that chip alone in four steps of 6.25 MHz, the others are not written
    to; offsets set before the start are part of the bring-up, and the
    highest clock each chip ever has is its own; a new chain clock carries
    the offsets and no chip passes its old or new clock; an offset asked
    for mid-ramp, before the chips are counted and in the BM1373's settle
    time arrives; an offset without a PLL setting or past the chain's end
    changes nothing; counters and nonces of a chip follow its clock; the
    hash counting number is the fastest chip's.
- `crates/chain/src/offsets.rs`: the limits and the stored form.
- `lone.rs`, `latch.rs`: a new offset is a fresh chance for that chip only.
- `crates/board`: the voltage hold for a chip's ramp; the self-test judges
  a chip at its own clock.

Flash: the image grew by 9,296 bytes with the feature (2,999,840 →
3,009,136, built from the same path length), and stands at 3,010,384
bytes after the changes above (69,808 bytes left below the import
window).

## What the devices said (2026-11-12, runs by dvb-WarpAxe #2)

**The mining loop's stack.** The loop kept the chain driver and the two
policies as locals: a frame of 12,720 bytes in a 16 KB stack, 13,072
bytes with a clock per chip. A Naja Duo then overflowed that stack every
one to four minutes, with or without offsets: `format!("{:.3}")` of a
nonce's difficulty takes 2.7 KB on its rare exact path, and an interrupt
on top was one frame too many. A 20 KB stack cured it and took 4 KB of
internal RAM, which a NerdAxe Gaia with a TLS pool did not have (web API
at a crawl, an update upload that timed out). Now the driver and the
policies live in PSRAM, the driver is renewed in place at every chain
start (`Chain::renew`), the loop's frame is 3,328 bytes in a 12 KB
stack, that formatting is done with whole numbers, and `minerStackFree`
in `/api/system/info` shows the least stack the loop has had to spare.
Measured on main 6c68bea: Gaia with TLS 29,787 bytes of internal heap
free (25,443 on v0.1.43), largest piece 18,432 (17,408); `minerStackFree`
6,320 on the Gaia, 6,108 on a Gamma, 6,384 on the Naja Duo.

**A BM1373 answers to its own address, not the shifted one.** Both
reference drivers send a single-chip command for a BM1373 to the chip's
address shifted right by two (ESP-Miner `BM1372_COMMAND_ADDRESS_SHIFT`,
NerdOS's per-chip writes), and WarpAxe did as they do. On the Naja Duo
(two BM1373, ESP-Miner's init, chips at 0x00 and 0x10, 327 MHz) the
first run with offsets gave, as the mean of each chip's hash counter
over four minutes:

| Offsets | Chip 0 | Chip 1 |
|---|---|---|
| none | 2271 GH/s | 2245 GH/s |
| chip 0 −60 MHz | 1841 | 1857 |
| chip 1 −60 MHz | 2246 | 2256 |
| none again | 2272 | 2241 |

Both chips slower for chip 0's offset, neither for chip 1's. Reading
registers back took it apart:

- After the init, register 0x58 held the domain-end IO strength
  (`0x0001F111`) on chip 0 and the value written to all before it
  (`0x00011111`) on chip 1. The init's write for chip 1, to address 4
  (0x10 shifted), had reached no chip; under ESP-Miner, which sends the
  same bytes, it does not either.
- A test build that sends one register write to one address of choice
  (branch `diag/bm1373-single-write`, never released) then wrote to
  0x10: register 0x58 read back from chip 1 alone. A PLL word to 0x10
  in four steps down to 301.56 MHz: chip 1's PLL register changed,
  chip 0's did not; hash counters 2265 GH/s on chip 0 and 2078 on chip 1
  (2076 expected). The same to 0x00: chip 0 alone, 2081 against 2268.

So a BM1373 answers to its full address, like every other chip of the
family, and a single BM1373 can be given a clock of its own. Chip 0
never showed the error: 0 shifted is 0.

That explains the third row of the table (the offset for chip 1 went to
address 4 and did nothing), not the second: the offset for chip 0 was
set while the chain ran, where the driver writes to chip 0 alone, at
address 0, its address either way. The second row is the board.

**Chips in series share one current.** The two chips of a Naja Duo sit
in series across the regulator's output, the midpoint not regulated. A
chip that is slowed takes less current and a larger share of the
voltage, and the other is left with less. With the address put right
(main 5d50c13, 327 MHz, offsets set while the chain ran, each PLL word
read back, no chain restart counted):

| Offsets | PLL changed on | Hash counters | Errors chip 0 / chip 1 |
|---|---|---|---|
| none | – | 2266 / 2267 GH/s | 0.02 % / 0.2 % |
| chip 0 −25 MHz | chip 0 | 2104 / 2269 | 0.02 % / **1.36 %** |
| chip 1 −25 MHz | chip 1 | 2250 / 2082 | 0.04 % / 0.07 % |
| chip 0 −60 MHz | chip 0 | 1845 / counters started over | – |

25 MHz apart, the faster chip pays in errors and the slower one gains;
60 MHz apart, the faster chip's counters went back within two minutes:
a chip that reset itself for want of voltage, its PLL never written,
and nothing in the firmware took note. That is the "both slower" of the
first run. It is also what makes balancing such a pair possible:
slowing a chip a little gives it a larger share of the voltage at the
other's cost, so the chip with more errors is the one to slow.

Two things follow in the firmware:

- On a board with chips in series (`voltage_domains` above 1: Naja Duo,
  Gamma Hex, NerdQAxe++ Rev 7) the offsets may lie at most 25 MHz
  apart, or 8 % of the chain's clock where that is less
  (`offsets::series_spread_mhz`); the API refuses more, and a stored
  list that is further apart is not applied.
- A chip whose total counter goes back while the chain runs on is
  counted (`hashrateMonitor.asics[i].counterRestarts`), with a line in
  the log and `lastError`.

Since then (`ChipProfile::command_address_shift` 0 on the BM1373):
every single-chip write to a BM1373 goes to the chip's own address. That
is the offsets, and it is the init: chip 1 of a Naja Duo now gets the
domain-end IO strength ESP-Miner means it to have. (Written by hand in
the test, it made no clear difference to chip 1's error rate: 0.20 % and
0.22 % over nine and eight minutes against 0.36 % before and 0.195 %
earlier that day, chip 0 at 0.03 %.) The one BM1373 of a Gaia sits at
address 0 and gets the same bytes as before. The single-chip *read* that
had brought nothing back from chip 1 (docs/05) went to address 4 too;
reads stay by broadcast, as the references do them.

## Still to run

Run on 2026-11-12 (v0.1.44 test builds, NerdOctaxe on SV1+TLS, 625 MHz /
1170 mV): chip 2 at −25 MHz, `asicFrequencies[2]` 600 and the others 625;
measured with `hashCount` over 4 minutes chip 2 hashed 1229 GH/s against
1265–1289 for the other seven (expected about 4 % less), and on read-back
only chip 2's PLL word differed (0xd0c00230 against 0xd0c80230); no chain
restart, no latch reset, no silent-chip restart; back to `[]` within about
20 s. The limit texts were seen as documented. Items 3–6 below are still
to run.

Whether a real BM1370 takes a PLL write addressed to it alone while it
hashes, the way it takes the same register written to all, the simulator
cannot show. The short test of 2026-10-02 on a NerdQAxe++ (one chip
±25 MHz, its counter rate followed 1:1, no duplicates) suggests it does.
On the BM1373 the single writes are proven by hand; the offsets through
the driver are not yet. To be run with Oliver's OK, through dvb-WarpAxe
#2:

1. **NerdOctaxe, chip 2 at −25 MHz** (625 MHz, the voltage it runs at):
   `asicFrequencies[2]` 600, the others 625; chip 2's `total` falls by
   about 4 %, the others' stay; `expectedHashrate` falls by 0.5 %; no
   restart, no `chipSilentRestarts`, no `domainLatchResets`, no duplicates
   over 30 minutes.
2. **Back to none** (`[]`): chip 2 back at 625, its `total` back.
3. **With the voltage**: one PATCH with chip 2 −25, chip 4 −50 and
   1130 mV from 1150 mV; the log shows the voltage step after the chips'
   ramps; error rates per chip over 17 minutes against the table above.
4. **Chips off and on, and a restart**, with offsets stored: each chip
   comes up at its own clock (`asicFrequencies` from the first reading),
   chip 4's domains all present.
5. **Limits**: +100 on one chip at 625 MHz (above the board's highest), an
   offset below half the clock and `"x"` are refused with 400 and the
   chip named; the stored list is unchanged.
6. **Self-test** with offsets: passes, each chip near 100 % of its own
   rate.
7. **NerdQAxe++** (four BM1370): chip 3 at −25 MHz, as in 1.
8. **Naja Duo** (two BM1373): `[-25, 0]` and `[0, -25]` are taken and
   slow the one chip (done on 5d50c13, table above); `[-60, 0]` is
   refused with 400 and names the 25 MHz; with `[0, -25]` stored, chips
   off and on: chip 1 comes up at its own clock, chip 0 at the chain's;
   `counterRestarts` stays 0 on both over an hour; then, with the
   midpoint measured, what a small offset does to it.

# Clock per chip on the dashboard

Since v0.1.44 a board with several chips can run each chip at its own
clock (`chipFreqOffsets`, MHz added to `frequency`, negative = slower).
WarpTune uses it to slow down a chip that makes errors before the whole
chain has to go down. Until v0.1.46 the offsets were only visible in the
folded "Clock per chip" fields on the settings page: on the dashboard a
board with a slowed chip looked like every other.

Decision (2026-11-17): show it, but only while it matters. Built for
v0.1.47.

## What the web UI shows

Only while at least one chip has an offset other than 0:

- **Dashboard, power card.** The clock row reads "Clock (base)". Under it
  "Clock per chip", with who set the offsets and how long ago, then one
  line per chip: its clock, the offset in yellow for a chip that has one,
  and its temperature where the board measures one per chip
  (`asicTemps` with one entry per chip, as on the Naja Duo; the
  NerdQAxe++ has none). A chip with an offset has a yellow bar on its left.
- **Hash counters.** Under "ASIC n" the chip's clock, yellow for a chip
  with an offset, so a slowed chip's hashrate and errors can be read
  against its clock.
- **Settings.** "Clock per chip" opens by itself and names who set the
  offsets and when.

With every chip at the chain's clock the pages look as before.

## No voltage per chip

One regulator feeds every chip. On boards whose chips are in series
(Naja Duo, Gamma Hex, …) they share its output, but nothing on the board
measures the share; on boards with the chips in parallel they all get the
same voltage. So there is no voltage per chip to show or to set. A chip
that takes voltage from the other shows in its error rate and temperature,
which the dashboard has.

## Who set them, and when

A PATCH that changes `chipFreqOffsets` can say who it comes from in
`chipFreqOffsetsBy`: `"manual"` (the web UI sends it), `"warptune"`
(WarpTune). Anything else, or nothing, counts as `"api"`. The firmware
stores the source with the time (NVS key `chipoffset`, five bytes: source,
then seconds since 1970) and reports both in `/api/system/info`:

| Field | Meaning |
|---|---|
| `chipFreqOffsetsBy` | `"manual"`, `"warptune"` or `"api"`; null for offsets from before v0.1.47 |
| `chipFreqOffsetsAt` | seconds since 1970 of the last change; null when the clock was not set then, or before v0.1.47 |

A change by hand or through the API also goes into the event log
(`chip_clocks`, `chip_clocks_api`, with the chips that have an offset as
`chip:MHz`, e.g. `1:-25 3:-10`); one through the API at most every ten
minutes, so a tuner that does not name itself cannot fill the log either.
WarpTune's changes do not go in: while it searches it changes the offsets every few minutes, and
the event log keeps 32 events, so boots and crashes would be pushed out.
WarpTune keeps its own journal, and the patch record
(`/api/v1/diagnostics` → `configPatches`) has every change with its sender.

## Cost

Image +6.3 KB at d1aee04 (2,853,488 B against 2,847,232 B at fac2589;
different build paths, so a rough figure). Five bytes of NVS.

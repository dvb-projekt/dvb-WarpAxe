# The display's choices, and an Avalon on the swarm page

Written 2026-10-18 for the release after v0.1.30. Two features, neither
touching the regulator or the safety logic: the device display's pages
and cockpit places become choosable, and the swarm page shows an Avalon
read over the cgminer API.

## A — What the display shows

The layouts stay fixed; what the owner chooses is the content. The
choices are short names (`crates/displaycfg`, host-tested), set over
`PATCH /api/system` and reported as in effect in `/api/system/info`:

| Field | Values | Factory (= v0.1.30) |
|---|---|---|
| `displayPages` | an ordered list of `cockpit`, `bitcoin`, `device`, `pool`, `log`; at least one, none twice | all five, in that order |
| `displayStartPage` | one of the listed pages: shown at boot and after a minute without a key press | `cockpit` |
| `cockpitBig` | `hashrate`, `efficiency`, `power` | `hashrate` |
| `cockpitSlots` | exactly three of `chip`, `vr`, `power`, `efficiency`, `fan`, `fanpct`, `shares`, `bestdiff`, `height`, `price`, `pooldiff`, `uptime`, `vin`, `sharesph`; `[]` = the board's default | `chip, power, efficiency` (the chipless board: `chip, sharesph, uptime`) |
| `cockpitFooter` | `pool-shares-ip`, `pool-ip`, `shares-uptime`, `pool-uptime`, `ip-uptime`, `worker-ip` | `pool-shares-ip` |
| `oledSlides` | an ordered list of `temp`, `power`, `efficiency`, `shares`, `best`, `pool`, `block`, `wifi`, `logo`; at least one | all nine |
| `oledInterval` | 1–60 s per slide | 4 |

`displayKind` (`lcd`, `oled`, `none`) tells the web UI which of these
apply to the board. Anything outside the tables is refused with 400 and
the field named; a start page that is not on the (new) page list too.

Decisions:

- **The curve under the large number follows it.** With `cockpitBig`
  = `power` the sparkline shows the last hour of power, with
  `efficiency` the J/TH of each 10-s sample (power over the ten-minute
  rate). The history already samples power beside the rate, so no new
  storage was needed, and a power figure above a hashrate curve would
  have read as one thing when it is two.
- **The efficiency curve leaves out samples below a tenth of the
  expected rate** (a chain start, a stop): a few watts over a near-zero
  rate would be one spike that flattens the hour. The estimate's tilde
  stands before the power and the efficiency only, never the hashrate.
- **A value the board lacks is a dash, never 0**: no regulator
  temperature or input voltage on the NerdNOS, no fan on the chipless
  board, no power or J/TH there either. The web UI hides those choices
  per board (`hasVr`, `hasFan`, `cpuMiner`); the firmware draws the dash
  if an API client sets them anyway.
- **The digit face has no letters**, so a place's value is digits in
  VAL16 with the unit in the label face beside it: `11.2 M` for a
  difficulty, `968,857` for the height, `999999 /999` for shares (the
  thousands separators go first when six figures and the rejected count
  would not fit, then the unit steps down a size, then it goes). The
  uptime is the one place drawn in the text face, as `12 d 23 h`.
- **The live view in the settings** reads `/api/v1/display/frame` every
  3 s while the page is open (the HTTP server has one task; a frame costs
  it up to half a second and 108 KB, beside the dashboard's 5-s poll) and
  backs off to 10 s after a miss; the firmware answers at once with 503
  while the display is asleep instead of waiting two seconds for a frame
  that never comes.
- **The dots top right show the active pages only**, and none when
  there is one page. The special screens (overheat, chips off, update,
  block found, Wi-Fi setup) cover every page as before and cannot be
  switched off.
- Stages 3 (accent colour, background image) and 4 (free layout) were
  not part of the task and are not prepared for.

Preview: `tools/display-preview` takes the choices from `PREVIEW_PAGES`,
`PREVIEW_START`, `PREVIEW_BIG`, `PREVIEW_SLOTS`, `PREVIEW_FOOTER`,
`PREVIEW_OLED` and `PREVIEW_PRICE`, and `PREVIEW_STRESS=1` puts the
longest values in (12 d 23 h, 999,999/999 shares, 65,535 rpm, 999.99 M,
a seven-figure height, a six-figure price). Every place in every
position, the three large numbers and the six footers were rendered in
en, de, es, fr, it and pt that way; nothing runs into its neighbour.

## B — An Avalon on the swarm page

The swarm page reads every member's `/api/system/info` from the browser.
An Avalon has no such thing: its MM speaks the cgminer API, raw TCP on
port 4028, one JSON command in, one JSON document with a NUL at the end
out. A browser cannot open that socket, so the miner does it.

- A member stored as `cgminer:<host>` (the web UI's "Avalon (cgminer
  API)" kind when adding, or a typed `cgminer:` prefix) is read through
  `GET /api/v1/swarm/cgminer?host=<host>`. The host is an IPv4 literal
  only (no name that could resolve elsewhere later). Plain members stay
  on the HTTP path; old lists keep working.
- The firmware sends `summary`, `estats` and `pools` for a card, and
  `version` once per host in the network search, and nothing else:
  `crates/avalon` has no code path for a write, and `query()` refuses
  any other command name. The card endpoint opens a socket only towards
  a host that is on the stored list with the prefix (403 otherwise).
- The read runs on a short-lived thread of its own (16 KB, internal
  RAM; sockets and a few KB of JSON, no flash), one at a time, with a
  2.5 s connect/read timeout, a 3 s budget for a whole reply (a device
  that trickles bytes is dropped), a 16 KB bound on each reply and a
  nesting limit of 8 levels checked before the parser runs (serde_json
  would take 128, and 127 levels cost 45–74 KB of stack; real replies
  are 3–4 deep). The HTTP task never waits: without a fresh entry it
  starts the read and answers `202 pending`, and the page asks again
  after 700 ms. Answers and failures are cached for 10 s, so several
  open browsers cost the Avalon no more than one. The reply carries no
  CORS header: it is for this miner's own page.
- The reply is an AxeOS-like document so the card code reads it as it
  reads the others, plus the Avalon's own fields: `workMode` (Eco /
  Standard / Super), `tempMax`, `tempTarget`, `fanRpms`, `chipTemps`
  (160 values on the Q), `modePowerCapW`, `dna`, `firmware`,
  `systemStatus`, and `cgminer` as the marker. What the Avalon does not
  have is `null`: `vrTemp`, `coreVoltage`, `overheat_temp`. Only the
  first MM module (`MM ID0`) is read; an Avalon with several would need
  more than one card.

### The network search

"Search network" on the swarm page runs two walks of the same /24 at
once: the browser's (every address, `/api/system/info`, as before) and
the miner's for port 4028 (`POST /api/v1/swarm/cgminer/scan` starts it,
`GET` says where it stands; neither waits on the HTTP task). The miner's
walk takes the subnet from its own Wi-Fi address (a private range only,
RFC 1918, taken as /24) and nothing the browser names (400 otherwise:
the USB link has nothing to find), refuses to start while an update is
being written or less than 20 KB of internal heap is free (503), runs
one search at a time on a coordinator thread with six workers, all with
stacks in PSRAM (sockets and a 4 KB reply, no flash; lwIP has 16 sockets
since this change, `sdkconfig.defaults`, the HTTP server holds up to
five, the pool one, mDNS two), 400 ms to reach a host and 1 s for its
reply, and a hard budget of 30 s for the whole run (253 silent hosts /
6 × 0.4 s ≈ 17 s). The walk goes upwards from .1, where static miners
tend to sit, so what a budget leaves untried is the top of the range. A
host that accepts the connection is asked for `version` only, bounded to
4 KB and four levels (real replies: 425 bytes, three levels), and counts
as a cgminer when the reply carries `VERSION[0].CGMiner` or `.API`
(`PROD` names the product, "Avalon Q"); a port that answers with
anything else, or resets, is simply not one. A host for which no socket
was free is tried three times, then counted apart from what the budget
left; the page names both counts. A finished search answers a second
click within 30 s. Locks: `SCAN` before a scan's `progress`, never the
other way; the coordinator copies its summary out of `progress`, drops
the guard, logs, and only then notes its end under `SCAN`, and only if
the entry is still its own generation. From a Mac the same walk
(`cargo run -p warpaxe-avalon -- --scan <own ip>`) found the .43 and
nothing else in 10 s on 2026-10-18.

Hits join the browser's list with an "Avalon · cgminer" badge; adding
one stores `cgminer:<ip>`, and a member already on the list is not shown
again.

Field meanings, checked on the Avalon Q (MM 25052801, cgminer 4.11.1,
API 3.7) on 2026-10-18 and against WarpTune's Avalon client:

| Source | Field | Used as |
|---|---|---|
| `estats` → `MM ID0:Summary` | `GHSspd`, `GHSavg` | `hashRate_now`, `hashRate_avg_mm` (GH/s) |
| | `PS[0 1208 2295 34 792 2296 840]` | the last field is the draw as the Avalon's own LCD shows it (840 W at 54 TH/s in Eco) → `power`; field 2 is a bus voltage ×100 mV (12.08 V) → `psVoltage`; the first two fields describe a small auxiliary bus, not the mining power |
| | `TMax`, `TAvg`, `TarT`, `ITemp` | `tempMax`, `temp`, `tempTarget`, `tempInlet` (°C) |
| | `Fan1`–`Fan4`, `FanR` | `fanRpms`, `fanrpm` (the first), `fanspeed` (%) |
| | `WORKMODE`, `WORKLEVEL`, `MPO` | `workMode`/`workModeIndex`, `workLevel`, `modePowerCapW` |
| | `DH[3.501%]` | `errorPercentage` (the device's hardware-error share) |
| | `Core`, `TA`, `Ver`, `DNA` | `ASICModel`, `asicCount` (or the length of `PVT_T0`), `version` ("MM 25052801") and `deviceModel` ("Avalon Q" from the letter before the dash), `dna` |
| `estats` → `HBinfo` | `PVT_T0[…]` | `chipTemps`, one value per chip |
| `summary` | `MHS 5m` / `1m` / `15m` / `av` | `hashRate` (the five-minute mean, nearest AxeOS's ten minutes), `hashRate_1m`, `hashRate_15m`, `hashRate_av` |
| | `Elapsed`, `Accepted`, `Rejected`, `Stale`, `Hardware Errors`, `Device Rejected%`, `Best Share`, `Found Blocks` | `uptimeSeconds`, `sharesAccepted`, `sharesRejected`, `sharesStale`, `hardwareErrors`, `deviceRejectedPercent`, `bestDiff` = `bestSessionDiff` (cgminer keeps one figure, since it started), `blockFound` |
| `pools` | the pool with `Stratum Active`, else the first: `URL`, `User`, `Status`, `Stratum Difficulty`, `Current Block Height`, `Priority` | `stratumURL`/`stratumPort`/`activePool`, `stratumUser`, `stratumConnected`, `poolDifficulty`, `blockHeight`, `isUsingFallbackStratum` |

On the card: model "Avalon Q", `160× A3197S`, the work mode as a badge,
chip temperature with the hottest chip noted and a bar that ends at
100 °C (the MM throttles by itself and runs its chips to 92 °C in Super;
100 °C is WarpTune's failsafe line), the fans instead of the regulator
gauge, power and J/TH, shares, best share. The fleet's hashrate, power,
shares and record include it.

Tests: `crates/avalon` parses the three replies recorded from the .43
(wallet masked), a truncated `estats`, a non-JSON reply, an overlong one
and an empty one, and reads the simulated Avalon Q of `warpaxe-cgminer`
end to end, checking that its mode was not written. Live, read-only,
against 192.168.178.43: `cargo run -p warpaxe-avalon -- 192.168.178.43`.

## The web UI, gzip-compressed

The page (`src/ui/index.html`, 292 KB with eight languages) goes into
the image gzip-compressed: `build.rs` packs it deterministically (no
mtime, no file name, OS byte fixed) into `OUT_DIR/index.html.gz`,
decompresses it again and compares it byte for byte with the source
(a mismatch fails the build), and `api.rs` embeds only the compressed
form with `include_bytes!` and serves it on `/` with `Content-Encoding:
gzip`, `Content-Length` and `Vary: Accept-Encoding`, `Cache-Control:
no-cache` as before. Every path the page comes from ends there: Wi-Fi,
the USB network (192.168.7.1) and the setup hotspot, whose captive-portal
probes redirect to `/`. Every browser takes gzip, the captive-portal
sheets of phones and laptops included (they are the system's web view);
a client whose `Accept-Encoding` names no gzip gets one line of plain
text (406) instead of compressed bytes, and a client that sends no such
header (curl, wget) gets the gzip, as RFC 9110 allows (`curl
--compressed` unpacks it). The fonts and the wordmark stay as they are:
woff2 and webp are compressed already. `tools/i18n-apply.py` and
`tools/display-preview` keep working on the uncompressed source.

## Image size

v0.1.30 (a53c909): 2,900,352 bytes. With both features: 3,008,016 bytes; with the network search for an Avalon 3,041,712 bytes, 38,480 bytes below the 0x2F0000 import window of the chipless board (`tools/release-firmware.sh`); with the web UI gzip-compressed 2,840,368 bytes, 239,824 bytes below the import window (the page: 292,502 → 90,330 bytes)
(+105.1 KB; the OTA slot is 3,145,728, so 134.5 KB remain). The growth
is the new web-UI strings in eight languages (about 32 KB of HTML), the
display tables and their validation, the Avalon client with its JSON
translation, and the cockpit's places.

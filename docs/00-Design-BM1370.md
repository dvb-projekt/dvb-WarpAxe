# WarpForge — Firmware Design, Stage 1: BM1370

*Working title. As of 2026-09-22. Authors: Oliver + Claude. Status: draft, none of it built.*

---

## 0. Scope

**Stage 1 = BM1370, one chip, Bitaxe Gamma 60x.** One device, one topology, one
reference board. Everything else is ballast as long as the first accepted share
isn't there.

**Non-goals in Stage 1:**

- BM1373 → waits for the Gaia hardware (NerdAxe Gaia, bitronics.store). The
  architecture keeps the slot open (chip profile), but no profile gets written
  on speculation.
  **Measured on the device 2026-09-22** (NerdOS `v1.1.0-31-gae509770`, `/api/system/asic`
  + `/api/system/info`, 192.168.178.40):

  | | BM1373 (Gaia) | BM1370 (Gamma 602) |
  |---|---|---|
  | Chips | 1 | 1 |
  | `smallCoreCount` | **6860** | 2040 |
  | Stock | **350 MHz @ 940 mV** | 525 MHz @ 1150 mV |
  | Frequency options | 300,320,340,**350**,360,380,400,420,440 | … up to 575 |
  | Voltage options | 900…1040 mV (steps of 20) | … |
  | abs limits | V 800–1300 mV, f **no limit** (`absMaxFrequency: 0`) | |
  | Board | 12 V (10.8–13.2), max 6 A, `maxPower` 40 W | 5 V |

  `6860 × 350 MHz = 2401 GH/s` — the manufacturer's 2.4 TH/s works out exactly.

  **The real insight: BM1373 is wide and slow, BM1370 narrow and fast.** 3.4×
  as many cores at 2/3 the clock rate. For the chip profile that means a
  completely different PLL operating range — none of it can be derived from the
  BM1370, and a shared "BM137x" ladder would be wrong.

  **Evidence for §3.3 (solve the PLL instead of a table):** The frequency list is
  *not* uniform — steps of 20, but around the stock point 340/350/360 in steps
  of 10. That is exactly the pattern of a list built from what the PLL actually
  hits. With a solver you'd have the grid instead of guessing it.

  Further conclusions: (a) **single-chip like the Gamma** — n=1 remains the
  normal case; (b) the 1.9" IPS display needs its own HAL trait
  (`Display`, SPI) plus a UI task that **never** blocks in the mining path;
  (c) ⚠️ **`asicTemps: [0]` — there is no ASIC junction temperature.** The
  only real sensors are `temp` (board) and `vrTemp`. That is exactly the case
  from §7.4: a firmware that regulates on a sensor here that measures nothing
  has no thermal protection. The plausibility check is not a luxury but
  mandatory.

  Still unverified: MCU variant, VR type, actual PLL divider limits.
- BM1366 / BM1368 → **definitively out** (2026-09-22): not in the portfolio, so
  never verifiable against hardware. See §12.1.
- Multi-chip chains (NerdQAxe++, Octaxe) → addressing is designed n-chip-capable
  from the start, but only tested and shipped with n=1.
- Nothing commercial. This is a learning project + tool, not a product line.

**The one goal everything is measured against:** A firmware whose behavior can
be reproduced in CI *without hardware*. Everything else in this document follows
from that.

---

## 1. Platform Decisions

### 1.1 MCU: ESP32-S3 stays

Non-negotiable, because the boards exist. A better MCU is useless if nobody can
flash the firmware.

### 1.2 Language: Rust on ESP-IDF (`std`), not `no_std`

| Option | Verdict |
|---|---|
| C on ESP-IDF (like ESP-Miner) | Rejected. No gain over the existing code, and the bug classes that hurt there (buffers, lifetimes, ignored error returns) remain. |
| Rust `no_std` + `esp-hal` + `embassy` | Rejected for Stage 1. Nice, but WiFi/TLS/OTA would have to be rebuilt or `esp-wifi` accepted. That is risk in the wrong place. |
| **Rust `std` on `esp-idf-svc`/`esp-idf-hal`** | **Chosen.** WiFi, mbedTLS, NVS, OTA, HTTP server come from the IDF. The mining core is nevertheless `no_std`-clean and therefore host-testable. |

The point is not "Rust is better". The point is: the part that counts
(`forge-core`) is then a pure library without hardware dependency, and you can
run its tests on the Mac in milliseconds instead of on the board in minutes.

### 1.3 Toolchain

- `espup` + `esp` Rust toolchain (Xtensa), `ldproxy`
- Build matrix: `forge-core` on **stable host** (x86_64/aarch64) for tests,
  full firmware on Xtensa
- No `cargo-espflash` requirement in CI — the firmware image is built and
  stored as an artifact, flashing is done by the HIL runner (§10.3)

---

## 2. Module Split

```
forge-core/          no_std, no alloc, no HW traits
  ├ frame/           0x55AA frames, CRC5, CRC16, encode/decode
  ├ chip/            ChipProfile types, PLL solver, register constants
  ├ job/             Stratum job → ASIC work, extranonce, version rolling
  ├ nonce/           nonce response → (chip, core, share), difficulty check
  └ sha/             double-SHA256 for local share verification

forge-proto/         no_std + alloc, no HW
  ├ v1/              Stratum V1: incremental parser + encoder
  └ v2/              Stratum V2: Noise NX + mining protocol  (Stage 3)

forge-control/       no_std + alloc, no HW, no clock (time is injected)
  ├ tune/            operating-point state machine (Settle → Judge → Commit)
  ├ thermal/         thermal model + fan control
  └ health/          error budget, degradation levels

forge-hal/           Traits: Uart, Gpio, I2c, Clock, Rng, Storage
forge-esp32s3/       implementation of the traits on esp-idf-hal
forge-app/           tasks, wiring, HTTP/WS API, web assets
forge-sim/           host binary: BM1370 simulator (§6)
forge-tests/         integration tests: forge-app logic against forge-sim
```

**The rule that holds the whole thing together:**
`forge-core`, `forge-proto` and `forge-control` may know **no** HW crate, no
`std::time`, no `log!` with side effects. Time comes in as `now_ms: u64`,
randomness as a seed, I/O as a return value ("I want to send these bytes"),
never as a call. That is exactly the pattern from `warppool_tune::regulator` —
pure decision logic, no I/O — and it works there.

**Consequence:** ~80 % of the firmware logic runs in `cargo test` on the Mac.

---

## 3. Chip Profile: Register Map as Data

### 3.1 The problem we avoid

ESP-Miner has `bm1366.c`, `bm1368.c`, `bm1370.c` — almost the same code three
times. A fix in one file doesn't automatically land in the others. (The same bug
class as V1/V2 in WarpPool, only without the discipline of checking both.)

### 3.2 The shape

```rust
// forge-core/src/chip/profile.rs
pub struct ChipProfile {
    pub name: &'static str,

    // Identity
    pub chip_id: u16,              // response to GET_STATUS 0x00
    pub id_mask: u16,

    // Topology
    pub small_cores: u16,          // BM1370: 2040 (from esp-miner; to be verified)
    pub big_cores: u8,
    pub domains: u8,

    // Clock
    pub xtal_hz: u32,              // 25_000_000
    pub pll: PllSpec,              // divider limits, see 3.3
    pub freq_min_mhz: u16,
    pub freq_max_mhz: u16,         // hard lock, not "recommended"

    // Initialization
    pub init: &'static [InitStep],

    // Protocol
    pub nonce: NonceLayout,
    pub version_rolling: VersionRollingCaps,
    pub uart: UartSpec,

    // Register addresses (family differences!)
    pub reg: RegMap,

    // Physics
    pub thermal: ThermalSpec,
    pub electrical: ElectricalSpec,
}
```

`InitStep` is deliberately a **data list with semantics**, not a byte sequence:

```rust
pub enum InitStep {
    Write   { reg: u8, val: u32 },
    WriteAll{ reg: u8, val: u32 },
    Read    { reg: u8, expect: Option<u32> },  // expect=Some → abort on mismatch
    CoreReg { id: u8, val: u8 },
    ChainInactive,
    SetAddress { step: u8 },
    Delay   { ms: u16 },
    Baud    { rate: u32 },
}
```

**Why with semantics instead of raw:** The simulator can then *validate* a
sequence ("you sent SET_ADDRESS before CHAIN_INACTIVE came — real hardware
won't put up with that") instead of merely comparing bytes. And `Read` with
`expect` turns the init into a self-checking sequence: if the chip doesn't read
back the expected value after the PLL write, we abort *right there* instead of
puzzling over hardware errors ten minutes later. ESP-Miner writes mostly blind.

### 3.3 PLL: solve instead of looking up a table

```
f_out = f_xtal × fbdiv / (refdiv × postdiv1 × postdiv2)
```

```rust
pub struct PllSpec {
    pub fbdiv:    RangeInclusive<u16>,
    pub refdiv:   RangeInclusive<u8>,
    pub postdiv1: RangeInclusive<u8>,
    pub postdiv2: RangeInclusive<u8>,
    pub vco_hz:   RangeInclusive<u64>,   // f_xtal × fbdiv / refdiv must land in here
    pub constraints: &'static [PllConstraint],  // e.g. postdiv1 >= postdiv2
}
```

`solve(profile, target_mhz) -> Option<PllWord>` searches for the combination with the
smallest error that satisfies *all* constraints, and returns the actually
achieved frequency along with it.

Two things this does better than today:

1. **The UI shows the real frequency, not the requested one.** If you set 545 MHz
   and the PLL can only do 543.75 MHz, then 543.75 is what's displayed. Today the
   display lies, and every efficiency calculation (J/TH) lies with it.
2. **The frequency ladder is generated, not hard-coded.** The
   `STEP_MHZ = 25` grid from WarpTune is an approximation of what the PLL can
   actually hit. With the solver you get the *achievable* points — and between
   525 and 550 there are more of them than a 25-step ladder shows.

A `const` test checks in CI that every point of the ladder is solvable and that
the solved frequency lies within ±0.5 %.

### 3.4 Source situation BM1370 — honestly

Everything we know about the register map comes from reverse engineering
(ESP-Miner `main/asic/bm1370.c`, NerdQAxe fork, cgminer patches). There is **no**
datasheet.

From that follows how we handle it:

- Every register value in the profile gets a provenance attribute:
  `#[src("esp-miner v2.15.1 bm1370.c:142")]` or `#[src("measured 2026-xx-xx")]`
- Values without measurement count as **unconfirmed** and are marked as such in
  the boot log
- A `verify` mode runs the read-back check over all writable registers on the
  first boot of a new firmware and logs deviations — that is the data source
  from which the map becomes *proven* over time instead of inherited

Safe enough for the start (from the existing code, pinned in CI against fixtures):

| Item | Value |
|---|---|
| Preamble | `0x55 0xAA` |
| Command checksum | CRC5 (Bitmain variant, over header+payload) |
| Work checksum | CRC16 |
| Init baud rate | 115200 |
| Nonce response | `0xAA 0x55` + nonce + job ID + version bits + CRC5 |
| Version rolling mask | `0x1fffe000` |
| Ticket mask | local difficulty threshold, power of two |
| Stock point Gamma | 525 MHz @ 1150 mV |

Everything beyond that: extract, pin, measure.

---

## 4. Wire Layer

### 4.1 Frames

```rust
// Encode is allocation-free into a &mut [u8]
pub fn encode_cmd(buf: &mut [u8], cmd: Cmd) -> Result<usize, FrameErr>;

// Decode is incremental: feed bytes, get events
pub struct RxDecoder { /* fixed ring buffer, no heap */ }
impl RxDecoder {
    pub fn push(&mut self, byte: u8) -> Option<RxEvent>;
}
pub enum RxEvent {
    Nonce(NonceResponse),
    RegRead { chip: u8, reg: u8, val: u32 },
    Desync { dropped: u16 },     // ← does not exist in ESP-Miner
}
```

`Desync` matters: when the UART stream loses step (it happens, at high baud
rate and with interference on the cable), the firmware should **count and
report** it, not silently discard bytes. A rising desync rate is an early
indicator of a signal integrity problem, long before the hashrate collapses.

### 4.2 UART operation

- RX via DMA into a ring buffer, ISR only pushes, parsing in the task
- Baud rate change as an atomic step: write chip register → drain →
  switch MCU UART → read-back probe → on failure back to 115200
  (ESP-Miner has no way back here; a failed baud switch means reboot)
- TX budget: commands at defined points, never in the middle of a work burst

---

## 5. Job Pipeline and Memory Model

### 5.1 No allocation in the hot path

```
[WiFi/TCP] → [V1 parser, incremental] → [job arena, N=8 slots, static]
                                              ↓ SPSC ring
                                        [work builder]
                                              ↓
                                        [UART TX]

[UART RX] → [RxDecoder] → SPSC ring → [share validator] → [submit queue, persisted]
```

The Stratum parser is a **state machine over the byte stream**, not a
`cJSON_Parse` over an assembled string. It writes fields directly into the job
slot and has hard upper bounds for every field (merkle branches, coinb1/2,
extranonce). A pool that sends oversized fields gets an error and a disconnect —
not a heap overflow.

Job arena: 8 slots of fixed size, ring allocation, slot generation as part of the
job ID. A nonce arriving for an overwritten slot is counted as `stale_local` and
**not** submitted. (Today it is sometimes submitted and produces pool rejects,
which are then wrongly counted as instability — exactly the case that
`is_benign_reject()` in WarpTune has to filter out again after the fact.)

### 5.2 Mining survives network outage

On connection loss:

1. ASIC keeps running on the last job, `ntime` is rolled within the allowed
   window
2. Found shares go into a persistent submit queue (NVS ring, ~64 entries)
3. After reconnect: re-subscribe, then submit whatever can still lie within the
   ntime/job window

After 120 s without connection the job is considered dead and the ASIC is driven
to a low point (save power instead of hashing pointlessly).

### 5.3 Failover with hot standby

Two pool slots, **both** connected, subscribed, authorized. The standby receives
jobs and discards them. Switching = pointer swap + one work push, no handshake.

Cost: a second TCP socket and ~4 KB. Gain: failover without a hashrate hole.
Today a pool switch costs the full reconnect cycle.

### 5.4 Local share verification

Every nonce is double-SHA256-checked locally against the pool difficulty
**before** submit. Costs a few µs per share on the S3 (the rate is low). Gain: a
chip delivering garbage no longer produces pool rejects but a clean local
HW-error counter. That separates "chip unstable" from "pool rejected" at the
source — instead of having to guess afterwards from reject strings.

---

## 6. The Simulator — the Actual Centerpiece

`forge-sim` is a host binary that behaves like a BM1370 on the UART.

### 6.1 Transport

- **PTY mode:** creates a `/dev/ttys00x` that the firmware-under-test opens
- **In-process mode:** `forge-sim` as a library, directly against the
  `forge-hal::Uart` trait implementation — that is the fast path for
  `cargo test`, no OS involved
- **TCP mode:** for manual debugging with real tooling

### 6.2 What it models

```rust
pub struct SimChip {
    profile: &'static ChipProfile,
    regs: [u32; 64],
    addr: Option<u8>,
    baud: u32,
    state: ChipState,      // Reset | Addressed | Configured | Hashing
    pll: PllWord,
    // Physics
    junction_c: f32,       // simple RC model from power
    // Work
    current_work: Option<Work>,
    rng: Pcg32,            // deterministic! seed = test parameter
}
```

The sim **doesn't actually hash**. From the nonce rate that results from
frequency × cores × ticket mask it deterministically draws points in time and
delivers nonces that match the work — constructed, not found. For the firmware
that is indistinguishable, and the test runs in milliseconds instead of hours.

### 6.3 Fault injection — the reason this is worth it

Control channel (separate socket / API in in-process mode):

```rust
pub enum Fault {
    NoResponse { for_ms: u32 },
    GarbageNonces { rate: f32 },
    BitFlipTx { probability: f32 },      // UART interference
    HangAfterFreqChange { above_mhz: u16 },
    BrownoutBelow { mv: u16 },           // delivers HW errors instead of nonces
    ThermalRunaway { rate_c_per_s: f32 },
    RegReadCorrupt { reg: u8 },
    BaudSwitchFails,
    ColdStartSlow { ms: u32 },
    DuplicateNonce { rate: f32 },

    /// ⚠️ NOT made up — happened on the Gaia, 2026-09-22 11:44.
    /// VR output at 0, power 36 W → 5 W, no hashing.
    /// NO fault flag, NO reboot, register access still answerable,
    /// reported hashrate stays stuck at the old value.
    /// See 01-Benefit-for-WarpTune.md, section "Field evidence".
    SilentVrShutdown { above_mhz: u16 },
}
```

`SilentVrShutdown` is the most important test case in the whole list, because it
refutes the most convenient assumption: that a dead device reports itself as
dead. The only honest measurement was the power draw. Expected behavior:
detection within one tick, point marked as bad, fallback to the anchor, entry in
the post-mortem log — without manual intervention and without network.

Each of these is a field failure that nobody tests today, because you can't
produce it on real hardware at the push of a button. With the sim each one is a
regression test:

- "Chip doesn't respond for 5 s" → expected: recovery sequence, no reboot,
  event in the post-mortem log
- "Hangs above 600 MHz" → expected: tuner anchors 575 MHz, marks 600 as bad,
  doesn't come back for an hour (the step-memory behavior from WarpTune, just
  one layer deeper)
- "Brownout below 1050 mV" → expected: voltage is raised *before* the
  frequency, not after

### 6.4 Record and replay

`forge-sim record` attaches (via a Y-cable or the board debug UART) to real
hardware and logs the dialog. `forge-sim replay <trace>` plays it back. That
turns every real field failure you capture once into a permanent CI test.

**This is the part that has value independently of the rest** — see Part 2 of
the cost analysis.

---

## 7. Control

### 7.1 Division of labor firmware ↔ Sidecar

This is the central design question, because WarpTune already has the ladder
control.

**In the firmware, because only possible there:**

| Function | Why not in the Sidecar |
|---|---|
| Ramp ordering (V before f going up, f before V going down) | milliseconds, on the bus |
| PLL change at job boundaries | milliseconds |
| Emergency stop on over-temperature | must not depend on WLAN |
| Fallback to last anchored point | must work without network |
| Fan control | control loop, 1 Hz, local |
| Per-chip error attribution | needs raw nonce data |

**In the Sidecar (WarpTune), because better there:**

| Function | Why |
|---|---|
| Search strategy over operating points | needs history, comparison across devices, pool view |
| Energy mode / solar attribution | needs house context |
| Long-term journal | needs space |
| Coordination across multiple devices | needs the overall view |

**The interface between them is a contract, not a command:**

```
POST /api/v1/operating-point
{ "frequency_mhz": 575, "core_voltage_mv": 1190, "anchor": true, "ttl_s": 3600 }
```

- `anchor: true` → this point is anchored in NVS after a successful settle phase
  and is the fallback target from then on
- `ttl_s` → if the Sidecar goes silent, the device falls back to the last anchor
  after expiry, **not** to factory defaults

That is your philosophy directly in the firmware: the device point is the
validated upper limit, and the firmware holds it. No Sidecar, no network, no
reboot may throw it away.

### 7.2 Operating-point state machine

```
        ┌──────────┐
        │  ANCHOR  │ ◄── boot, TTL expiry, watchdog, emergency-stop recovery
        └────┬─────┘
             │ new point requested
        ┌────▼─────┐
        │  RAMP    │ V/f in the right order, PLL at job boundary
        └────┬─────┘
        ┌────▼─────┐
        │  SETTLE  │ measurements discarded until temperature τ-stable
        └────┬─────┘
        ┌────▼─────┐
        │  JUDGE   │ evaluates after N shares OR T seconds — whichever comes first
        └──┬────┬──┘
     good  │    │  bad
        ┌──▼─┐ ┌▼──────────┐
        │ HOLD│ │ FALLBACK │ → ANCHOR, point blocked for 1 h
        └──┬──┘ └──────────┘
           │ anchor requested
        ┌──▼─────┐
        │ ANCHOR │
        └────────┘
```

**JUDGE evaluates in shares, not in seconds.** At 1.2 TH/s and pool diff
`d`, a share takes on average `d × 2^32 / 1.2e12` seconds. A fixed 60-second
evaluation is statistically worthless at high diff. The state machine waits for
`n_min` shares (default 30 → relative error ~18 %) with a time cap as an
emergency exit. *("VarDiff is measured in shares" — applies here just the same.)*

**Point-change detection:** If someone outside (UI, another API, another tool)
changes frequency or voltage, SETTLE/JUDGE is restarted. A point that was
evaluated half under old and half under new conditions is a lie. (That is the
fix you already built in WarpTune — it belongs in both layers.)

### 7.3 Hashrate: two numbers, reported separately

| Field | Meaning |
|---|---|
| `hashrate_nominal_ghs` | `f_real × small_cores / 1000` — what the chip *could* do |
| `hashrate_effective_ghs` | from accepted shares, EWMA over a **share window** |
| `hashrate_confidence` | 0..1, from the share count in the window |

ESP-Miner shows only the first and calls it "hashRate". It stays high while the
chip produces 40 % garbage. The third number is the point that every tool has
to guess today: *How reliable is this measurement at all?*

### 7.4 Thermals

Feed-forward from an RC model (`τ` per board in the profile) plus PI on the
junction temperature. Concretely:

- **Plausibility check** between ASIC diode and board NTC. Difference outside
  the expected window → mark both as unreliable and run conservatively. Not:
  "0 °C, all good, full throttle."
- **Throttling soft and early**: small frequency steps from `target - 3 K`,
  not a hard jump at `overheat`.
- **Fan with an acoustic cap**: a configurable upper limit that is only exceeded
  in a real emergency. These things sit in your home.
- **Emergency stop in three stages**: frequency → voltage → stop. Each stage
  logged.

### 7.5 Error attribution per chip

The nonce carries the chip address. So: HW-error counter, nonce rate and desync
rate **per chip**, not per board.

At n=1 (Gamma) this is bookkeeping. At n=4/8 (NerdQAxe++, Octaxe) it is the
difference between "board has 3 % errors" and "chip 3 is the outlier, the others
are clean". That is why it is built in Stage 1, even though it gains nothing
there yet — retrofitting means rebuilding the data model.

---

## 8. Operation

### 8.1 Configuration

NVS with **schema version** and atomic commit: the entire configuration is
written as one transaction, not field by field. A power failure in the middle of
saving must not leave a half state behind.

Migration at boot: read `schema_v` → apply migration chain → write back.
No "the field didn't exist before, so it's 0".

### 8.2 OTA that doesn't brick

```
1. Upload into free slot          (A/B)
2. Signature check                (ed25519 over the image)
3. Boot into new slot, flag PENDING
4. Health gate:
     - WiFi connected             within 60 s
     - pool connected             within 120 s
     - ASIC initialized           within 30 s
     - ≥1 accepted share          within 600 s
     - no panic, no WDT reset
5. all met  → mark slot as VALID
   otherwise → reboot into old slot, reason in the post-mortem log
```

That is the difference between "the update broke something, I'm rolling back"
and "device is dead, get the USB adapter".

Secure Boot v2 and flash encryption: prepared, but **off by default**. Once
burned in it is irreversible, and a device that can no longer be reflashed is
the wrong risk for a hobby project.

### 8.3 Post-mortem log

Ring buffer in **RTC RAM** (survives soft reset and deep sleep), mirrored to
NVS on clean shutdown:

```
[reset_reason] [uptime_s] [freq_mhz] [core_mv] [junction_c] [vr_c]
[last_asic_evt] [desync_count] [hw_err_10min] [heap_free_min] [task_wm_min]
```

After every unexpected reboot the UI shows **why**. Today you guess.

### 8.4 Clock

NTP with multiple servers, and — importantly — the firmware rolls `ntime` only
within the window the pool allows, measured against the **pool time from the
Stratum job**, not the local clock. A wrongly set local clock must not lead to
systematically rejected shares.

---

## 9. API Surface

### 9.1 Native

```
GET  /api/v1/state          complete state, one object
GET  /api/v1/stream         WebSocket, deltas, ~1 Hz
POST /api/v1/operating-point  §7.1
GET  /api/v1/history        ring buffer last 24 h, 1-minute resolution
GET  /api/v1/postmortem     §8.3
GET  /metrics               Prometheus
POST /api/v1/ota            signed image
```

Versioned from day 1. `/api/v1` never breaks; new things come under `/api/v2`.

### 9.2 AxeOS compatibility layer — non-negotiable

```
GET   /api/system/info      → mapped from /api/v1/state
PATCH /api/system           → frequency, coreVoltage → operating-point
POST  /api/system/restart
GET   /api/swarm/info
```

**Why mandatory:** WarpTune (`crates/tune/src/axeos.rs`), your dashboard, the
swarm tools, every community script — they all speak AxeOS. A firmware that
breaks the ecosystem won't be used, no matter how good it is.

The compatibility layer additionally reports `smallCoreCount`,
`sharesRejectedReasons` and (new) `hashrateConfidence` — fields that WarpTune
already reads optionally or could use immediately.

### 9.3 Authentication

Optional token on all writing endpoints, default **off** (otherwise you lose
every hobbyist), but offered in the setup wizard. That today anyone on the LAN
can change the VCore unasked is not a decision but a gap.

### 9.4 Web UI

Static, pre-compressed (brotli), target < 120 KB, **no external resources**.
No Google Fonts, no CDN. A miner in the basement must not depend on something
loading from the internet.

i18n from the start with a CI guard against hard-coded strings — the same rule
as in WarpPool. Initial languages: de, en. Others only after native-speaker
review.

---

## 10. Test Strategy

### 10.1 Host (every commit, seconds)

- Unit tests `forge-core`, `forge-proto`, `forge-control`
- **Property tests** (`proptest`): PLL solver (solved frequency always within
  tolerance & constraints), frame round-trip (encode∘decode = id), job→work
  (merkle root against an independent reference implementation)
- **Fuzzing** (`cargo-fuzz`) on the V1 parser and the `RxDecoder`. Goal: no
  panic, no overflow, on arbitrary byte garbage.
- Golden frames: pinned byte sequences from real hardware, against which the
  encoder is checked

### 10.2 Simulator integration (every commit, minutes)

The complete app logic against `forge-sim` in in-process mode, with virtual
time (24 h of operation in ~2 s). Every scenario from §6.3 as a test case.

### 10.3 HIL (nightly + before release)

One Gamma on a runner. Flash → boot → pool (local WarpPool on regtest) →
at least one accepted share → OTA cycle → rollback cycle.

Rule from the WarpPool history that applies here just the same: **positive
evidence instead of text patterns.** The test passes when a share has
*arrived*, not when "accepted" appears in the log.

### 10.4 What is NOT tested

Real hashrate numbers in the simulator. The sim claims nonce rates, it doesn't
earn them. Anything that answers "is this frequency really faster" only goes
through HIL and the field.

---

## 11. Milestones

| # | Content | Effort* | Acceptance criterion |
|---|---|---|---|
| ✅ M0 | *done 2026-09-23:* repo `~/code/dvb-WarpAxe`, workspace, CI (fmt/clippy/test/no_std), GPLv3 | — | `cargo test` green, `thumbv7em` build green |
| ✅ M1 | *done 2026-09-23:* `crc`, `frame`, `decoder` (single-byte resync, desync counter). Fuzzing deliberately replaced by proptest (no nightly). | — | 50 tests green, pushed |
| ✅ M3 | *done 2026-09-23:* `pll::solve` (integer, C oracle), `job`, `result`, `regs`, `chip::ChipProfile` + `InitStep` + `render()`, profile `bm1370` as data. 80 tests, commit 05b0ccd. | — | Init sequence reproduces all 12 commented wire frames in order |
| 🔶 M2 | *core done 2026-09-23:* `cmd` decoder, `SimChip`/`SimChain` (register file, state machine, Poisson nonces, 7 chip faults), 19 sim tests; chip-ID prediction falls out of the sim end-to-end. Plus `board` (watts/degrees/VR/fan, Gamma from WarpTune measurements) and `warpaxe-axeos`: AxeOS API on top of the sim with `/sim/fault`. **Open:** WarpTune live test, PTY, record/replay | — | commit 53bfaa4, 127 tests |
| **M0** | Repo, toolchain, CI (host + xtensa build), `forge-core` skeleton | 2–3 d | `cargo test` green on host, image builds |
| **M1** | Frame layer + CRC + golden frames + fuzzing | 3–4 d | round-trip property green, fuzzer 1 h without findings |
| **M2** | `forge-sim` basic function (init, regs, nonce generation) | 4–5 d | init sequence from esp-miner runs through against sim |
| **M3** | Chip profile BM1370 + PLL solver | 3 d | ladder generatable, all points solvable, ±0.5 % |
| **M4** | Stratum V1 (parser, job→work, submit) against sim | 5–6 d | sim "finds" share → correct `mining.submit` |
| **M5** | **First real share on Gamma hardware** | 3–5 d | accepted share on WarpPool regtest |
| **M6** | Fault injection + recovery paths | 4–5 d | all scenarios §6.3 as tests, none ends in a reboot |
| **M7** | Operating-point state machine + thermals + anchor/TTL | 5–7 d | sim: point is held, TTL fallback works |
| **M8** | API v1 + AxeOS compat + WarpTune drives the device | 4–5 d | WarpTune tunes WarpForge without code change |
| **M9** | OTA + health gate + post-mortem | 4–5 d | deliberately broken image → automatic rollback |
| **M10** | Web UI, i18n, setup, docs | 5–7 d | device set up from zero without a terminal |

\* focused working days, not calendar days. Sum M0–M5 (first share):
**~20–26 days**. Sum through M10: **~45–55 days**.

That is honestly calculated and no weekend project. The most honest sentence
about it: up to M5 everything is learning and risk, only from M6 on does it
become better than the existing code.

**The exit point with the best ratio is M2+M3.** See Part 2.

---

## 12. Open Decisions

### 12.1 BM1366 / BM1368 — DECIDED: no (2026-09-22)

**Oliver has neither BM1366 nor BM1368 in the portfolio.** My original argument
"the hardware is there, so it's testable" was simply wrong — the miner list
contains BM1370 (Bitaxe 602, BitForge Nano, NerdQAxe++, NerdOCTAXE-γ), BM1373
(NerdAxe Gaia) and the Avalon Q. Not a single one of the old chips.

That also eliminates the weakened recommendation to check in the register maps
as unconfirmed profiles: profiles that nobody can ever verify against hardware
are not an architecture proof but dead weight with a maintenance promise
attached.

**Consequence for the design:** The claim "a new chip is just a table" has to be
proven differently — namely on the BM1373, as soon as the Gaia is added as the
second profile. That is the more honest test, because both chips actually exist
and both can be measured.

### 12.2 Further open points

- **Sv2 in the firmware**: Noise NX on the S3 costs RAM (secp256k1 context).
  Feasible, but evaluate only after M8. Not a blocker for anything.
- **Name**: "WarpForge" is a guess. Fits the scheme, but collides conceptually
  with Forge-OS (BitForge Nano), which WarpTune already supports.
  Alternatives: WarpAxe, WarpCore (taken), WarpSilicon.
- **License**: AGPLv3 like WarpPool? For firmware, AGPL is weak (no network
  interaction in the sense of the clause). GPLv3 would be more fitting, and
  ESP-Miner-derived register data could trigger GPL obligations anyway —
  **clarify before M0**, not after.
- **Relationship to ESP-Miner**: own project or upstream at some point? My
  assessment: own project. The architecture decisions (Rust, sim,
  profile-as-data) cannot be introduced incrementally.


---

## 13. Corrections and Findings from Stage A (2026-09-23)

At the start of Stage A, ESP-Miner `v2.15.2rc0-17-g0c798de` was cloned and
`components/asic/` read completely. The picture is better than described in
§3.1, and some claims in this document were wrong.

### 13.1 What I had wrong

| Claim in the doc | Actually |
|---|---|
| §3.1 "`bm1366.c`, `bm1368.c`, `bm1370.c` — the same code three times" | There is now `asic_common.c` (chip counting, `receive_work`, difficulty mask, timeout calculation), `pll.c` (shared solver) and `frequency_transition_bmXX.c`. The chip files only hold the init sequence, response struct and nonce layout now. The criticism applies in weakened form: the init sequences *are* still code, not data. |
| §7.1 "ramp ordering … ESP-Miner doesn't ramp" | ESP-Miner ramps the **frequency** in 6.25 MHz steps with a 100 ms pause (`do_frequency_transition`). What's missing is the *coordination with the voltage* — that is set separately in power management. The open point remains, but is narrower. |
| §3.3 "PLL: solve instead of table" as a novelty | `pll_get_parameters()` already **solves**: refdiv 1–2, postdiv1/2 1–7, constraint `postdiv1 > postdiv2`, fbdiv window per chip (BM1370: 160–239), priorities *smallest deviation → lowest VCO → smallest postdiv product*. What's missing: the *achieved* frequency is logged but not reported as an API value. The Rust solver is verified against the C solver, not reinvented. |
| §12.2 "BM1373 unknown" | `bm1373.c` exists (552 lines). And the test harness (`bm13xx_test_harness.c`) is at its core already a mini simulator: intercepts TX packets, delivers queued responses. Just without physics, without fault injection, without time. |

### 13.2 BM1373 is a different wire format, not a profile diff

From the defines in `bm1373.c` (register names internally "BM1372"):

| | BM1370 | BM1373 |
|---|---|---|
| Chip ID on the bus | `0x1370` | `0x1372`, alias `0x1373` |
| Chip-ID response | 11 bytes | **9 bytes** |
| Nonce/register response | 11 bytes | 11 bytes (same struct) |
| Max. baud | 1 MBaud | **3 MBaud** |
| Address stride | `256 / n` | **`0x10` fixed**, command address shift 2 |
| Write repetition | none | 3× with 50 ms |
| Init steps | ~20 | more, with soft-reset register `0xA8` = `0xF00701F0` |
| HCN | computed (`hcn_max − 2·134` errata) | constant `0x1EB5` ("S21 Pro") |
| PLL fbdiv | 160–239 | 160–239 (same) |
| Nonce layout | job\_id `(id&0xf0)>>1`, asic `nonce>>17`, core `nonce>>25 &0x7f`, small `id&0x0f` | **identical** |

Consequence for `ChipProfile`: besides the register values it also needs
`chip_id_reply_len`, `address_stride: Fixed(u8) | Divided`, `write_retries`
and `hcn: Computed | Fixed(u32)`. That is still representable as data — but
the profile format is determined by it, not only by the BM1370.

**Core counts — researched 2026-09-23, result: neither of the two numbers is proven.**

| Source | `small_cores` | Origin |
|---|---|---|
| ESP-Miner `device_config.h` | 6725 | PR #1890 (2026-08-22), **not a word on the derivation**, unchanged since |
| NerdOS `bm1373.cpp` (BitMaker `develop`) | 6860 | `#define BM1373_SMALL_CORE_COUNT 6860` — **marked with `TODO`** |
| Naja Duo factory measurement in PR #1890 | ~7000 | 4579 GH/s, 2 chips, 327 MHz, 2022 nonces (±2 %) |
| **Gaia, chip counter via NerdOS, 1 h at 400 MHz** | **~6945** | `hashRate_1h` 2778 GH/s, nonce-based, ±2–3 % |

`small_cores` is not a structural number on any BM13xx (BM1366: 894 on 112
cores) but a *measured* effective value. Both firmware numbers are placeholders.
The measurement on the Gaia favors 6860 over 6725, but cannot separate 6860 from
~6950. **Resolution to < 1 %:** one day at fixed frequency, then `hashRate_1d` —
or in Stage B read `TOTAL_COUNT` (0x8C) directly over a known interval, which
counts hashes exactly. The profile value for the BM1373 gets the attribute
`#[src("measured Gaia 2026-xx-xx")]`, not one of the two numbers.

**Domains:** BitMaker (X, Gaia launch): *"Now all 8 domains are active!"* The
BM1373 has **8 domains**; ESP-Miner configures `hash_domains = 4` and
compensates with `domain_hashrate_scale = 2.0` — i.e. reads 4 counters and
doubles. The profile needs `domains = 8` and all eight counter registers.

For Stage A (BM1370) all of this is irrelevant; for WarpTune today harmless
(a 2 % too-low expectation makes the device look *better*, no misjudgment).

Both chips: `core_count = 128`, `hash_domains = 4`. BM1370 stock
525 MHz @ 1150 mV, BM1373 stock 327 MHz @ 1000 mV (ESP-Miner; NerdOS Gaia: 350 @ 940).

### 13.3 Finding: the RX CRC covers the flag bits

While building the frame layer an assumption burst and turned into an insight.
The last byte of a chip response is `flags:3 | crc:5`. ESP-Miner checks
`crc5(all 9 bytes) == 0`. That property holds for a CRC only if **nothing**
sits between the data and the CRC bits — so the 3 flag bits must be part of
what the chip computes its CRC over. For TX frames (last byte `000 | crc:5`)
the property does *not* hold, because three zero bits sit in between there.

Proven on the only RX vector from a `bm1370.c` comment
(`AA 55 13 68 00 00 00 00 00 00 0F`, the chip-ID response of a BM1368):

```
crc5(8 bytes)                = 0x0C   ← not the wire value
crc5(8 bytes + 3 flag bits)  = 0x0F   ← the wire value
crc5(all 9 bytes)            = 0x00   ← ESP-Miner's check works out
```

That yields a **verifiable prediction** for the Gamma:

```
Read Register 0x00  →  AA 55 13 70 00 00 00 00 00 00 10
```

That is the first test that has to run on the real device in Stage B. If the
last byte doesn't match, the flag-bit rule is wrong and `verify5` with it.

In code: `crc::response_tag(payload, flags)` builds the value the simulator
writes into the last byte; `crc::verify5` checks received responses;
`frame::encode_response` / `frame::decode_response` are the pair that connects
simulator and firmware. A proptest pins that every response built this way
verifies.

### 13.4 Name and license decision

Repo: `~/code/dvb-WarpAxe`, crates `warpaxe-core` and `warpaxe-sim`.
**GPL-3.0-or-later**, because init sequences, register values and test vectors
come verbatim from ESP-Miner (GPLv3). "WarpForge" is thus only the name of this
notes folder now.

### 13.5 PLL solver: numbers instead of claims (2026-09-23)

`pll::solve` is done and checked against an `f32` port of `pll_get_parameters()`
on every quarter MHz from 50 to 1200 in both divider windows — identical
solution, ties included. Results that are immediately relevant for WarpTune:

| Target | fb | ref | pd1/pd2 | actual | Register 0x08 |
|---|---|---|---|---|---|
| 525 (Gamma stock) | 168 | 2 | 4/1 | 525.000 | `40 A8 02 30` |
| 600 | 192 | 2 | 4/1 | 600.000 | `50 C0 02 30` (VCO = 2400 → scale bit) |
| **640 (Gamma hand-tuned)** | 205 | 2 | 4/1 | **640.625** | `50 CD 02 30` |
| 690 | 221 | 2 | 4/1 | 690.625 | `50 DD 02 30` |

**496 of 601 integer MHz targets between 100 and 700 do not lie on the PLL
grid.** The Gamma has been running at 640.625 MHz for weeks, not at 640 — every
J/TH calculation in `ladder.rs` that assumes 640 is off by 0.1 %. Small, but
systematic, and an argument that the firmware must report the *achieved*
frequency (`frequencyActual` from §9.2).

**BM1373 peculiarity** (same fb window 160–239): Between 320 and 380 MHz the
window forces the solver to `refdiv = 1` with VCO 4.8–5.7 GHz, while 300/350/400
MHz lie at ~2.1 GHz. Whether the silicon tolerates the high VCO is unknown. But
it fits that NerdOS takes 350 as stock (VCO 2.1 GHz) and ESP-Miner's 327 is
resolved as 326.786. Question for Stage 2.

### 13.6 The simulator meets WarpTune (2026-09-23, late)

`warpaxe-axeos` serves a Bitaxe Gamma over HTTP from `SimChain` + `Board`.
WarpTune's `probe` reads it as *"BM1370 board 602 Gamma, 1128 GH/s of 1071
expected, 15.5 J/TH"* — without a single line of change to WarpTune.

**First live run: the ladder didn't move.** Heartbeat:
`up blocked: VIN 5.00 V < 5.01`. The board model reported exactly the nominal
voltage; WarpTune's VIN guard (exists because a PSU sagging to nominal is a
warning sign) took the simulator for a defective PSU — correctly. Fix: PSU
overshoot 0.15 V, sag 12 mV/A → 5.11 V under load (commit `8dd1370`).

That is the simulator's purpose in the opposite direction: **A regulator with
real guards finds the spots where the model is too ideal.** Every such spot is a
parameter one should measure on the real device.

**Lesson on time:** `settle_s` (45), `judge_after_s` (600) and `startup_s` (60)
are not configurable in WarpTune. A ladder step therefore needs ≥ 10 minutes of
wall time, no matter how fast the simulator computes its physics
(`--time-scale` only accelerates the device). For regression tests that should
run in seconds, WarpTune would need a test hook that sets the limits — or the
Regulator is driven directly against `Device::tick()`, without HTTP and without
a wall clock. The latter is the clean variant: `regulator.rs` is "pure decision
logic, no I/O", and `Device` delivers `Sample`-compatible numbers. That would be
the next sensible step on the WarpTune side.

### 13.7 The proof (2026-09-23, 08:38–08:44)

WarpTune live against `warpaxe-axeos`, sim at 600 MHz, VR death above it
(evidence: `02-Demo-WarpTune-vs-Sim-2026-09-23.txt`):

- **Power-collapse guard** fires 20 s after the step to 625, *inside* the
  settle window — the gap from 09-22 is closed.
- **Dead-chip guard** detects `coreVoltageActual 0` over three ticks and
  triggers the restart; the sim comes back up.
- **Finding:** The step memory treats the collapse point as a hole, not as a
  ceiling — after the restart "skipping 625 (unstable); up 600→650", second
  collapse, second restart. On hardware: two VR deaths before the regulator
  gives up. Proposal to the WarpPool session: collapse/dead chip at X → block
  all steps ≥ X for the hold duration.

With that, the core claim of `01-Benefit-for-WarpTune.md` is fulfilled: the
simulator is a test device the Regulator can run against, that misbehaves, and
that finds errors that cost hardware in the field.

### 13.8 The regression suite (2026-09-23, commit 9278ebf)

On Oliver's go: `crates/scenarios/` in WarpAxe drives WarpTune's `Regulator`
**in-process** against `Device` — no HTTP, no wall clock, `Limits` shortened via
struct literal. Five field failures as tests, **40 ms** for all: healthy ascent;
VR death in the settle window with ceiling (pins dvb-WarpPool 1c80963); chip
hang with power; sensor at 0 → no writes; duplicate nonces block. Package is
decoupled from the workspace (path dep on dvb-WarpPool); run: `cargo test --manifest-path
crates/scenarios/Cargo.toml`. As soon as dvb-WarpPool tags the ceiling: git dep
with fixed `rev` + own CI job.

The live rerun with `--seed 5` against the new binary independently confirmed
the fix over HTTP (one restart, no second climb; evidence in the demo file).

**With that, Stage A is fulfilled beyond its goal.** What the decision doc
promised as the Stage A yield — "the simulator as a test device for the
Regulator" — exists, found a regulator bug on the first day, and the bug is
nailed down as a test before it was merged fixed.

### 13.9 The Avalon block (2026-09-23, on Oliver's go)

`warpaxe-sim::avalon::AvalonQ` models the Q as an MM controller (no chip sim):
three work modes with the measurements from 09-11, mode change = hashboard
reinit (55 W standby, `GHSspd` 0 for 120 s, 60 s ramp), real 900-s window for
`MHS 15m`, `STATE[1]` always, no `coreVoltageActual`. `warpaxe-cgminer` speaks
the cgminer protocol on TCP 4028; WarpTune's own `AvalonClient` reads and writes
the emulated Q **unchanged** (test green).

The scenarios with the *real* Avalon limits (settle 180, judge 900, collapse-hold
300) delivered three findings — the path WarpTune had "never seen live":

| | Finding | Proposal |
|---|---|---|
| **A** | Healthy Standard→Super change is condemned at 900 s with 86 %, because `MHS 15m` contains the ~180-s reinit → urgent down → next reinit → Eco. **Cascade on a healthy device.** | `judge_after_s = settle_s + 900` for Avalon: the window then lies behind the reinit |
| **B** | Collapse is detected (dead-chip hashrate branch +270 s, before the 300-s power hold), restore revives — then A. | done with A |
| **C** | Refused `ascset` is repeated endlessly every 240 s, step never marked. | after 2–3 failed verifications mark as unstable |

The three scenarios exist as `#[ignore]` tests with the finding as justification;
they go live as soon as WarpTune changes the points. Model assumptions one may
doubt: reinit 120 + ramp 60 s ("1–3 min" according to the WarpPool session),
standby 55 W.

**Addendum, same evening:** A and B are fixed in WarpTune (`83faac4`: share
counter as a second voice on the urgent descent, share window from `settle_s`,
Avalon judge at `settle + 900`) and live at Oliver's. The model was switched to
cgminer's real behavior — `MHS 15m` is an exponentially decaying average
(τ 900 s), not a window — which makes scenario A *stricter*; the fix holds
regardless. C was an artifact of my harness: the real runner switches off live
control after 12 unverified ticks (`live_off`) and never writes a second time;
after rebuilding this path the test confirms exactly that. **All 14 scenarios
green**, no more `#[ignore]`. Open (Oliver's decision): whether `live_off` on a
refused `ascset` is too harsh.

## 14. Transport: USB-C and LAN alongside WLAN (request from Oliver, 2026-09-23)

**DECIDED (Oliver, 2026-09-23): the default is the USB bridge (14.6, path 2).**
The miner is a USB-NCM device; a Linux host next to it bridges USB to LAN.
LAN directly on the miner (W5500) and miner-as-USB-host (14.6, path 1) are
options, not the default.

**Question:** Can WarpAxe use the USB-C socket of the Bitaxe/NerdAxe hardware as
a data interface, and does LAN work?

### 14.1 What the hardware offers

| Board | MCU | USB-C | LAN |
|---|---|---|---|
| Bitaxe Gamma 601/602 | ESP32-S3-WROOM-1 | yes, D+/D- directly on GPIO20/GPIO19 (no bridge chip, no series resistors, no ESD protection) — **checked in the schematic, see 14.5** | no |
| NerdQAxe+/++, NerdOctaxe-γ, Q1370 (Pmaxsd; `shufps/qaxe`, KiCad open for the QAxe line) | **LilyGO T-Display-S3 dev board** plugged onto the carrier (schematic: one `T-Display-S3` symbol, four 12-pin headers, no USB socket on the carrier) | on the LilyGO module: native S3 USB on GPIO19/20; **VBUS powers the module** (diode-ORed with the carrier's 5 V); **CC open** on LilyGO's board too → USB-A-to-C cable | no |
| NerdAxe Gaia = firmware board `Q1373`, **4 × BM1373** (Pmaxsd) | LilyGO T-Display-S3, as above | as above | no |
| NerdAxe Gamma (Bitmaker, `BitMaker-hub/NerdAxe`, KiCad open; firmware board `nerdaxegamma` in Pmaxsd's tree) | LilyGO T-Display-S3 on a BM1370 carrier | as above | no |

The ESP32-S3 has **one** USB full-speed PHY (12 Mbit/s), which via a mux hangs
either on `USB_SERIAL_JTAG` (factory state: flashing, console) or on the
`USB_OTG` controller. The S3 does **not** have an Ethernet MAC (unlike the old
ESP32). LAN therefore always means: external chip via SPI (W5500, KSZ8851SNL,
DM9051 — all with `esp_eth` driver in the IDF) and thus a **board revision or a
hat**. The current boards have no header for it; only test pads.

### 14.2 USB-C as network: CDC-NCM

The IDF ships `esp_tinyusb` with the classes CDC-ACM, MSC, HID, DFU,
**ECM/NCM/RNDIS**. With NCM the miner appears on the host as a network card:
macOS and Linux natively, Windows from 11 without a driver, older Windows via
RNDIS. The IDF example `examples/peripherals/usb/device/tusb_ncm` (S2/S3/P4)
does exactly that, only the other way round (ESP as WLAN dongle). We turn it
around:

```
[Host]  usb0: 192.168.7.1/30  ←USB-C→  [WarpAxe] usbnet: 192.168.7.2
```

- The miner runs a **DHCP server** on the USB netif (like the Wi-Fi AP mode),
  address from a link-local /30 or /24, so that multiple miners on one hub don't
  collide (prefix from the MAC).
- **AxeOS API, web UI and Stratum run unchanged** — it is just another
  `esp_netif`. WarpTune sees `http://192.168.7.2/api/system/info`. No special
  paths, no second protocol.
- **Stratum over USB:** if the pool sits on the host (WarpPool on Mac, Umbrel,
  Hetzner gateway), the miner reaches it directly via the USB address —
  **without WLAN, without NAT**. Pool on the internet → host must forward (macOS
  Internet Sharing, Linux `ip_forward` + NAT); that is host configuration, not
  ours.
- **Fallback priority:** USB netif if link is up; otherwise WLAN. Both at the
  same time is allowed; Stratum takes the route with the pool.
- **Cost:** NCM occupies the OTG controller. `USB_SERIAL_JTAG` is then gone
  during operation: console runs over CDC-ACM as a second interface (composite
  NCM + ACM is standard with TinyUSB), **flashing remains**, because the ROM
  bootloader in download mode (BOOT pressed) muxes the PHY back itself. That
  has to be tested before it hits device no. 1 — a brick path would be fatal
  here. Until then: Kconfig switch, default WLAN + Serial-JTAG as today.
- **Power:** VBUS is **not connected** on the Gamma (see 14.5). USB-C supplies
  the board with no energy whatsoever, not even for the logic; without a PSU on
  the barrel jack the ESP32 is dead, even for flashing. Conversely the miner can
  never load the host port. For WarpAxe that means: the USB link is a pure data
  path, no power state the firmware would have to distinguish.

### 14.3 The simple variant: CDC-ACM with a line protocol

If NCM is too much: Serial-JTAG stays as it is, and WarpAxe speaks a
**JSON-Lines protocol** on the console (`{"cmd":"info"}` → `{"hashrate":…}`),
the same vocabulary as the native API from §9.1. Host-side this needs a tool or
a WarpTune transport for `/dev/tty.usbmodem*`. Advantage: zero risk on the USB
stack, works immediately. Disadvantage: WarpTune needs a second transport,
Stratum doesn't go over it. **That is the stopgap, not the goal.**

### 14.4 What it means for WarpAxe

1. **Architecture now:** the network layer is `esp_netif`-agnostic. No code may
   assume "WLAN" — neither the emergency stop (§7, "must not depend on WLAN")
   nor the reconnect logic nor the UI display "signal strength". Netifs are a
   list: `wifi`, `usb`, later `eth`.
2. **Stage B (firmware, BM1370):** WLAN + NCM/ACM composite as a Kconfig option,
   default off. First HIL test: flashing after NCM operation.
3. **LAN:** only once there is a board with a W5500. Software-side it is then
   another `esp_netif` (`esp_eth` + SPI), ~1 day.
4. **Simulator:** the scenarios need nothing new — the AxeOS emulator is already
   "just an IP". A PTY transport for the ACM variant (§13.9 leftover list) would
   make §14.3 host-testable.

**Decided:** NCM (14.2) is the goal, ACM (14.3) only if the flash way-back on
hardware isn't clean. LAN on the miner remains open until a board has it; the
default way into the LAN is the USB bridge (14.6).

### 14.5 Schematic check Bitaxe Gamma (2026-09-23, `bitaxeorg/bitaxeGamma` main, `esp32.kicad_sch`)

Netlist extracted from the KiCad source (own S-expression parser, pins via
symbol transformation, wire segments incl. T-junctions checked; the
`no_connect` markers sat exactly on the computed pin positions, which validates
the transformation):

| J5 USB-C pin | Net |
|---|---|
| A6, B6 D+ | → U4 pin 14 `GPIO20/USB_D+`, direct, nothing in between |
| A7, B7 D- | → U4 pin 13 `GPIO19/USB_D-`, direct |
| A4, B4 VBUS | **no_connect** |
| A5 CC1, B5 CC2 | **open** — no 5.1-kΩ pulldowns on the board |
| A8, B8 SBU | no_connect |
| A1, B1, S1 | GND + shield |

On the ESP32 sheet there is only a single resistor (R16 10k, EN pullup), no
diodes, no ESD devices, no USB-UART chip. The 3V3 LDO (RT9080) hangs on the 5V
net of the PSU, not on VBUS.

**Consequences:**

1. **Native USB confirmed.** CDC-NCM/ACM over `USB_OTG` is possible on the
   hardware side; nothing on the board stands in the way.
2. **No CC pulldowns → C-to-C cable unreliable.** A real USB-C host port
   detects no device without Rd and usually doesn't even switch D+/D- through.
   For continuous operation as a data path therefore: **USB-A-to-C cable** or a
   hub with A ports. That belongs in the docs, otherwise "doesn't work".
3. **No ESD protection on D+/D-.** For a permanently plugged connection during
   operation a note, not a blocker; the S3 has internal protection.
4. **VBUS unconnected** — see the power paragraph in 14.2.

NerdQAxe/Gaia (Pmaxsd) are not checked against sources; the boards are closed
(only Gerber/firmware public). The assumption remains "same standard S3
wiring" until a device is on the bench.

**Two hardware classes (corrected 2026-09-23 after Oliver's pointer):**
the Bitaxe Gamma has an ESP32-S3-WROOM-1 on its own PCB with a bare USB-C
socket (14.5). **Every Nerd device — NerdAxe Gamma, NerdQAxe+/++,
NerdOctaxe-γ, Q1370, Gaia/Q1373 — carries a complete LilyGO T-Display-S3
dev board** as its controller; the carrier has no USB socket at all. In
Oliver's portfolio the Bitaxe is the exception, the T-Display-S3 the
rule. WarpAxe treats both the same at the netif level; what differs is
in 14.7.

### 14.6 USB ↔ LAN purely in software? (question from Oliver, 2026-09-23)

Yes, two paths — one on the miner, one on a host next to it.

**Path 1: miner as USB host, USB-Ethernet adapter on the USB-C.**
The ESP32-S3 can do USB host (OTG). Espressif ships in `esp-iot-solution` the
components `iot_usbh_ecm` and `iot_usbh_rndis` (S2/S3/P4), which mount a
CDC-ECM or RNDIS device as `esp_netif` — built for 4G sticks, but works with any
adapter that speaks the class. For WarpAxe that would be another netif, as
foreseen in 14.4. Catches:

- **VBUS is not connected on the Gamma (14.5).** The miner cannot power the
  adapter. It needs an adapter with its own power input (the "OTG Ethernet with
  power-in" type for Chromecast / Pi Zero) or an externally powered hub. Normal
  adapters stay dead.
- **The adapter chip must speak ECM/NCM/RNDIS.** RTL8153-based adapters have an
  ECM configuration; ASIX AX88772 (Apple's adapter among others) speaks only a
  vendor protocol and is out. That means: a short positive list of tested
  adapters, otherwise support hell.
- Host stack costs RAM on the S3 and occupies the same PHY → Serial-JTAG gone,
  as with NCM device.

Result: works, but it is a tinkering path with hardware dependencies we don't
control.

**Path 2: miner as USB-NCM device (14.2), a Linux host bridges to LAN.**
Pi Zero 2 / Pi 4 / Umbrel box as USB host, `usb0` and `eth0` in a layer-2
bridge (`systemd-networkd`: one `Bridge=` entry, no NAT, no routing). The miner
gets its IP from the home DHCP like any LAN device, WLAN off, WarpTune and the
pool see it exactly as today. Multiple miners on one hub, each a `usbN`, all in
the same bridge. Cable: USB-A-to-C because of the missing CC pulldowns (14.5).

- Miner-side **zero additional code** compared to 14.2.
- Host-side standard Linux, ~10 lines of configuration; documentation task.
- On the Mac: Internet Sharing or a bridge via `ifconfig bridge` — works, but
  nothing for continuous operation.

**Decided (Oliver, 2026-09-23): path 2 is the default ("USB bridge").**
Path 1 becomes an option behind a Kconfig switch, only once a specific adapter
with power-in has been measured as suitable. Both need 14.2 as the foundation;
none of it changes the architecture decision from 14.4.

From that follows for the milestones:

- **Stage B (firmware BM1370):** NCM device + CDC-ACM console as composite,
  Kconfig switch, default off until the flash way-back is proven on hardware.
  Then default on.
- **Docs "USB bridge":** recipe for Pi Zero 2 / Pi 4 / Umbrel box
  (`systemd-networkd`: `br0` with `eth0` + `usb*`, miner side DHCP client on the
  USB netif), hub operation with multiple miners, cable note USB-A-to-C (14.5).
  Belongs to the Stage B release, not after.
- **HIL test case:** miner without WLAN credentials, USB only on a bridge, must
  boot, obtain DHCP, reach the pool and answer via the AxeOS API. That is the
  acceptance test for the transport.

### 14.7 The Nerd family's controller: LilyGO T-Display-S3 (2026-09-23)

Sources: `shufps/qaxe` (NerdQAxe++ `pi.kicad_sch`: symbol `bitaxe:T-Display-S3`,
`3d/T-Display-S3.STEP`, carrier 5 V → 1N5819 → module 5 V pin, no USB
on the carrier), `shufps/ESP-Miner-NerdQAxePlus` (`main/displays/displayDriver.h`,
one driver for every board; `main/boards/` has `nerdaxegamma`,
`nerdoctaxegamma`, `nerdqaxeplus`, `nerdqaxeplus2`, `q1370`, `q1373`,
`nerdhaxegamma`, `nerdeko`, `nerdqx`), LilyGO `schematic/T_Display_S3.pdf`
(read from the rendered page).

| Item | Fact |
|---|---|
| SoC | ESP32-S3R8, bare chip, 8 MB OPI PSRAM; flash W25Q128 16 MB (Bitaxe: WROOM-1 module) |
| USB-C | D+/D- → GPIO19/20 native; VBUS → TP4065 charger and → 1N5819 → +5 V → AP2112K 3V3. **The ESP32 runs from USB alone**, flashing needs no PSU. |
| CC pins | **open, no pulldowns** (pin A5 CC unwired in LilyGO's schematic). Same caveat as the Bitaxe: **USB-A-to-C cable**, C-to-C is unreliable. |
| Display | ST7789, 170 × 320, 8-bit i80 bus, 6.5 MHz pixel clock; backlight AW9364 on GPIO38; LCD power GPIO15; data GPIO39–42, 45–48; WR GPIO8, RD GPIO9, CS GPIO6, DC GPIO7, RST GPIO5 |
| Buttons | GPIO0 (boot), GPIO14 |
| Battery | TP4065 charger, Q4 power path; "battery power must set IO15 HIGH" — irrelevant on a miner, but IO15 is display power |
| Free GPIO to the carrier | GPIO1, 2, 3, 10, 11, 12, 13, 16, 17, 18, 21, 43 (U0TXD), 44 (U0RXD) — that is the whole pin budget for ASIC UART, reset, I2C, fan, VR enable |

**Consequences for WarpAxe:**

1. **Stage B targets the T-Display-S3 first**, not a generic S3 devkit: it
   is the controller of every Nerd device, and a bare T-Display-S3 on the
   desk is the Nerd hardware minus the ASIC. The NCM/flash-back proof of
   14.2 runs on it as-is, no PSU, no ASIC.
2. **Display support is not optional** for the Nerd class. The IDF ships
   the pieces (`esp_lcd_panel_st7789` over an i80 bus, LVGL optional);
   Pmaxsd's driver header is the pin reference. A blank screen on a
   NerdQAxe would read as "dead".
3. **Pin budget:** 13 GPIOs reach the carrier. The board profile (§3) must
   carry the pin map per board; the Bitaxe uses different GPIOs for the
   same functions.
4. **Gaia = Q1373 = 4 × BM1373**, 4- or 6-phase VR, not a single chip:
   the BM1373 profile and the chain driver's `expected_chips` need to
   say 4, and the small-cores question (13.x) is a 4-chip measurement.
5. **Devkit to buy:** LilyGO T-Display-S3 (lilygo.cc or AliExpress; the
   non-touch variant, 16 MB). An ESP32-S3-DevKitC covers only the Bitaxe
   class and adds nothing the T-Display-S3 lacks for the USB proof.
6. **Test bed:** with the Nerd family on T-Display-S3, the NerdAxe Gamma
   (single BM1370, KiCad open, same controller as Oliver's other devices)
   is the better first target than the Bitaxe Gamma; the Bitaxe stays
   the second, for the WROOM/bare-socket class.

### 14.8 Stage-B proof on the NerdMiner V2 (T-Display-S3), 2026-09-23 — PASSED

Code: `firmware/proof-usb/` (Rust std on esp-idf-svc 0.51, ESP-IDF v5.3.3,
esp_tinyusb 1.7.6 vendored as `components/esp_tinyusb`, TinyUSB 0.21).
Full 16 MB flash backup of the NerdMiner taken first (CloudDocs, with
SHA-256); restore is one `esptool write_flash 0`.

`host-proof.sh` passes all six steps on macOS with a USB-A-to-C cable:
the board enumerates as CDC-ACM + CDC-NCM, the host gets 192.168.7.2 by
DHCP from the board, `GET /` and `GET /api/system/info` answer over USB,
`GET /bootloader` puts the chip into ROM download mode and esptool talks
to it. No Wi-Fi at any point. The transport decision of 14.2/14.6 is
therefore hardware-proven for the Nerd class.

Findings that go into the real firmware:

1. **`transmit_wrap` is mandatory.** The Wi-Fi-AP netstack config we borrow
   for the USB netif calls `esp_netif_transmit_wrap`; with only
   `transmit` set, the first gratuitous ARP at link-up jumps to NULL
   (Xtensa cause 20, PC 0xFFFFFFFD, tcpip thread). This was the crash
   behind every symptom from the first flash on: reboot loop, halt,
   "device never answers".
2. **TinyUSB's NCM driver never calls `tud_network_init_cb`.** Link
   readiness must be derived from the first received frame (or from the
   SET_INTERFACE alt=1, which the driver does not expose).
3. **The USB PHY selection lives in the RTC domain.** After OTG use, a
   software reset does not bring USB-Serial/JTAG back; the ROM's
   download mode is then invisible. `tinyusb_driver_uninstall()` before
   `RTC_CNTL_FORCE_DOWNLOAD_BOOT` + restart restores it. This is the
   flash-back path of 14.2, now concrete and tested three times in a row.
   A panic reboot cannot run that code, so the firmware needs a
   recoverable path on boot: if the previous reset was a panic, stay on
   USB-Serial/JTAG for a grace period before switching to OTG.
4. **On this board every DTR/RTS reset via USB-Serial/JTAG ends in ROM
   download mode** (`rst:0x15, boot:0x3`); `espflash -a watchdog-reset`
   starts the app. Power-on and BOOT-held power-on behave as documented.
   And `RTC_CNTL_FORCE_DOWNLOAD_BOOT` is not cleared by the ROM: after a
   firmware-requested download mode the host clears
   `RTC_CNTL_OPTION1_REG` (0x6000812C) before the reset that should boot
   the app, or the chip returns to download mode every time. The
   flash-back round trip (app → `/bootloader` → esptool → clear flag →
   watchdog reset → app with lease and HTTP) passed three times in a row.
5. **Diagnosis without a console works:** core dump to flash, summary
   (task, cause, PC, backtrace) rendered on the display at the next boot,
   symbolised with `xtensa-esp32s3-elf-addr2line` on the host. Keep this
   in the real firmware.
6. Build-system traps: esp-idf-sys does not rebuild local C components by
   itself (touch `sdkconfig.defaults`); it takes bootloader and partition
   table from whichever `out` directory matches; espflash skips
   "unchanged" segments unless `--no-skip`.
7. SPIFFS on the NerdMiner keeps old config versions; the newest copy is
   the last one in flash order.
8. Correction (Oliver, 2026-09-23): the Bitaxe Gamma does have a display —
   a monochrome 128 × 32 SSD1306 OLED on I²C (SDA GPIO47, SCL GPIO48),
   plugged onto header J3 of the schematic. The board profile therefore
   carries a display kind: ST7789 over the i80 bus (Nerd class) or SSD1306
   over I²C (Bitaxe), with one status layer above both.

### 14.9 Stage-B firmware skeleton on the NerdMiner (2026-09-23, evening)

`firmware/warpaxe` runs on the T-Display-S3: USB-NCM with DHCP, config in
NVS with AxeOS field names, `/api/system/info`, `PATCH /api/system`,
restart, **OTA over USB into the other slot** (1.2 MB received in 2 s,
back on the new partition 13 s later), display with log tail and last
crash, core dump summary in the API, a 30 s USB-Serial/JTAG window after
a crash, and both flash-back paths (HTTP and a `B` on the CDC port).
Wi-Fi station is coded but not yet exercised. Stratum and the chain
driver on the UART are the next two steps.

Two more traps for the record: TinyUSB must drive the DWC2 in **DMA
mode**; slave mode stalls the NCM IN direction after ~14 KB and never
recovers. And a handler that runs on the TinyUSB task (the CDC receive
callback) must not uninstall TinyUSB itself; the reboot-to-bootloader
runs on a task of its own.

### 14.10 Stratum V1 on the board (2026-09-23, evening)

`crates/stratum` holds everything that does not touch a socket: line
classification, request builders, coinbase assembly, merkle root, the
header fields in the byte orders ESP-Miner hands the BM13xx (previous
hash in header order, merkle root word-swapped), share difficulty of a
hash. Tested on the host against a session captured from WarpPool
(`tests/vectors/warppool-2026-09-23.json`, reference values computed
independently in Python).

The firmware's `stratum.rs` runs one thread: connect, `mining.configure`
(BIP 320 mask), subscribe, authorize, then notifies become jobs for the
chain driver (not yet attached). On the NerdMiner over Wi-Fi it is
authorized on pool.warppool.org with mask `1fffe000`, difficulty 1, and
receives jobs (a clean job per block). Lesson: ESP-IDF has no `dup()`,
so `TcpStream::try_clone` fails with ENOSYS; read and write through
references to one stream.

### 14.11 The mining loop, ready for chips (2026-09-23, late)

Software side of stage-B step 3 is in place before the cable arrives:
`crates/simuart` puts `SimChain` behind a real serial port on the host
(follows the chips' baud switch; a 3 s silence then traffic counts as a
reset, since the reset line is not on the cable). On the board,
`uart.rs` is `warpaxe_chain::Uart` over UART1 (GPIO17/18) and `miner.rs`
runs the chain driver: work from the newest notify with a fresh
extranonce2 per submission, retire on clean jobs, every nonce re-hashed
on the host side and compared with the pool difficulty before it goes
to the Stratum writer, accept/reject counted from the pool's answers.
Without chips the driver reports `NoChips` and retries every 10 s; the
rest of the firmware is unaffected. ESP-IDF's UART read returns
`ESP_ERR_TIMEOUT` for "nothing pending" — mapped to zero bytes.

### 14.12 Board layer: regulator, sensor, fan, profiles (2026-09-24, pre-work for point 4)

WarpTune refuses to regulate a board that reports no temperature and no
power (`regulator cannot use this device: telemetry lacks required
field temp`), so point 4 needs the board around the chain before it
needs anything else. Written from ESP-Miner's board table and drivers
while the USB-UART adapter was in the post; nothing here has touched a
real regulator yet.

`crates/board` (`warpaxe-board`, `no_std`, over an `I2c` trait, tested
against a scripted bus):

| Module | What | Reference |
|---|---|---|
| `pmbus` | LINEAR11 / ULINEAR16 encode and decode, command codes | ESP-Miner `TPS546.c`, checked against its bit patterns (4.8 V = 0xCA66, 1.2 V at VOUT_MODE 0x17 = 614) |
| `tps546` | device-id check (D24A/D24S), OPERATION off, ON_OFF_CONFIG 0x1B, the whole operating window (`TPS546_write_entire_config`), `set_vout` with the fault/warn/margin ratios and OPERATION on, `vin/vout/iout/temperature/status_word/clear_faults` | `TPS546_init`, `TPS546_write_entire_config`, `TPS546_set_vout` |
| `emc2101` | part id 0x16/0x28, TACH input, fan driver in direct PWM mode (0x4A = 0x23), ideality 0x24 / beta 0x00 for the BM1370 diode, fan duty in 63 steps, tach → RPM (5 400 000 / count, 0xFFFF = stalled), external temperature 11-bit / 8 with open (0x3F8) and short (0x3FF) as faults | `EMC2101.c`, board table 600–603 |
| `fan` | PID on the chip temperature, gains P 5 / I 0.1 / D 2, reverse action, 100 ms sample, output in [minFanSpeed, 100], Arduino-PID semantics (derivative on measurement, clamped integral) | `fan_controller_task.c` |
| `profile` | `BITAXE_GAMMA` (UART 17/18, reset GPIO1 100/100 ms, I²C 47/48 at 100 kHz, TPS546 5 V window 1–2 V, one voltage domain, EMC2101 on the diode, SSD1306 at 0x3C, 525 MHz / 1150 mV, 5 V class, 40 W, +5 W board offset, chip 75/90 °C, regulator 105/145 °C) and `T_DISPLAY_S3_DEVKIT` (UART only) | `device_config.h` families and board rows |

Firmware (`firmware/warpaxe/src/board.rs`): the profile comes from the
config key `boardModel` (default `t-display-s3`); an unknown name falls
back to the devkit profile. Boot order: reset pin low, I²C, TPS546 init
and `set_vout` at the configured core voltage, EMC2101 init with fan
100 %, then the chain is released from reset with the init timing; the
miner pulls reset (10 ms / release) before every chain start, as
ESP-Miner does on retry. A board task samples once a second (chip and
regulator temperature, VIN, VOUT, IOUT, power = VOUT × IOUT + offset,
tach) and runs the fan every 100 ms: PID when `autofanspeed`, else the
configured duty, full fan without a reading. `PATCH /api/system` with
`coreVoltage` goes live through the regulator; `frequency` already ramps
the chain (§14.11). Over the profile's chip or regulator maximum the
task switches the core voltage off, holds the fan at 100 %, and the
miner stops in phase `OVERHEAT` until reboot (ESP-Miner's overheat
mode). `/api/system/info` carries `temp`, `vrTemp`, `power`, `voltage`,
`current`, `coreVoltageActual`, `fanspeed`, `fanrpm`, `nominalVoltage`,
`maxPower`, `overheat_mode`, `boardVersion`; `/api/v1/state` has the
raw board sample.

Checked on the devkit (2026-09-24): profile `t-display-s3`, no I²C
started, nothing else changed — Stratum authorised, USB and Wi-Fi up,
`coreVoltage` PATCH persisted (no regulator to apply it to). Untested and
first on the list for the Gamma: the TPS546 init sequence against a
real part, the diode reading, the fan PID with a real fan, and the
overheat stop.

SSD1306 (same day): `warpaxe_board::ssd1306` drives the Bitaxe's 128×32
OLED at 0x3C over the shared bus — init sequence for a 32-row module,
frame buffer of four pages, the shared 5×7 font (moved from the
firmware into the crate), mirror commands for a module mounted the
other way round. The firmware's `Display` is now an enum over the
ST7789 LCD and the OLED, chosen by the profile; the OLED shows four
lines: hostname, IP and boot step, chips × MHz, hashrate, chip
temperature and power, then shares, chain phase and pool state. The
board's I²C bus is created before the display so both can use it.
Checked on the devkit: the LCD path unchanged. The OLED path is
untested until the Gamma; orientation may need `set_mirror`.

Nerd profiles (same day, from NerdOS `main/boards/*.cpp` on
`develop`, `components/bm1397/serial.cpp`, `boards/drivers/i2c_master.cpp`
and `drivers/nerdaxe/TPS546.h`): every Nerd carrier has the chain UART
on GPIO17/18, I²C on SDA 44 / SCL 43, the chain reset on GPIO1 (Q1370
and Q1373 put it on an FXL6408 expander instead), the ST7789 on the
module.

| Profile | Chips | Regulator | Thermal | Defaults | Limits |
|---|---|---|---|---|---|
| `nerdaxe-gamma` | 1 × BM1370, 5 V | TPS546, NerdOS window (VIN on/off 4.8/4.5, UV warn 5.8, OV fault 6.0, VOUT 1–3 V) — **driver exists** | EMC2101, same diode tuning as the Bitaxe — **driver exists** | 515 MHz / 1150 mV, `flipscreen` true in NerdOS | 25 W, +5 W offset |
| `nerdqaxe-plusplus` | 4 × BM1370, 12 V | TPS53647, 3 phases, enable GPIO10, LDO GPIO13 | TMP1075 (+ EMC2302 fans) | 600 MHz / 1150 mV, up at 1200 mV | 100 W |
| `nerdoctaxe-gamma` | 8 × BM1370, 12 V | TPS53667 6 phases (or TPS53647 4, by a detect pin) | TMP1075 + EMC2302 | 700 MHz / 1210 mV | 300 W |
| `q1370` | 4 × BM1370, 12 V | TPS53647 4 phases behind the FXL6408 (reset 0, VR 1, LDO 2) | EMC2302 | 600 MHz / 1150 mV, up at 1200 mV | 150 W |
| `q1373` (Gaia) | 4 × BM1373, 12 V | TPS53647 4 or TPS53667 6 phases (expander pin 6) | TMP1075 + EMC2302 | 425 MHz / 1010 mV, up at 1050 mV | 180 W |

The profile type now says what a board has even where this crate has
no driver (`Vr::Tps5364x`, `Thermal::Tmp1075Emc2302`); on such a board
the firmware reports the profile and leaves the chain unpowered instead
of guessing. So the NerdAxe Gamma is drivable today with what exists;
the multi-chip Nerds need four more drivers: TPS53647/53667 (PMBus,
multi-phase, NerdOS `TPS53647.cpp`), TMP1075, EMC2302, FXL6408. The
BM1373 chip profile for the Gaia is a separate open item (§13).

OLED orientation: `flipscreen` (mirror both axes) and `invertscreen`
(inverse video), AxeOS/NerdOS names, live on the next draw. ESP-Miner
drives the same panel through LVGL with swapped axes and inverted
colour, so its flags do not map to raw SSD1306 commands; the Gamma
test settles the default with one PATCH.

Backup: `tools/flash-backup.sh <name> [port] [out-dir]` reads the whole
flash (size from the chip) to CloudDocs `WarpForge-Firmware/backups`
with a `.sha256` and a RESTORE.txt (`esptool write_flash 0 <image>`);
refuses to overwrite, checks the image length. First step on the Gamma.

Open: the Nerd multi-chip drivers named above; are not in any source we have (the
T-Display-S3 exposes 13 GPIOs to the carrier, §14.6), so no Nerd
profile yet; profiles with I²C pins other than 47/48 need the pin
mapping in `main.rs` generalised.


### 14.13 BM1373 chip profile (2026-09-24, pre-work for the Gaia)

`crates/core/src/chip/bm1373.rs`, written from NerdOS's
`BM1373::init()` (the firmware on the Gaia today), with ESP-Miner's
`bm1373.c` as the second reference. Both agree on: chip id 0x1372 with
0x1373 as alias, a nine-byte chip-id reply, a fixed chain address stride
of 16, single-chip commands at address ÷ 4 while replies carry the full
address, the PLL of the BM1370, the job-id encoding of the BM1370, and a
fixed hash counting number 0x1EB5 in place of the computed one
(ESP-Miner: "the generic BM13xx nonce-space calculation makes
BM1372/BM1373 chips search overlapping work"). They disagree on the init
sequence; NerdOS's was taken: the BM1370 sequence with the
version-rolling mask written without the enable bit until the end, Misc
Control `FF 00 C1 00`, no extra register writes. ESP-Miner's longer init
(3 Mbaud, soft reset, core registers, a domain-end IO strength) is the
fallback if this one does not bring a chain up. Counts and options are
NerdOS's Q1373 class: 128 cores, 6860 small cores, 425 MHz / 1010 mV
stock, ticket mask 4096, frequencies 250–550 MHz, voltages 980–1080 mV.

What the chip needed beyond a profile: `ChipProfile` gained
`chip_id_alias`, `address_stride`, `command_address_shift` and
`hcn_fixed`, and addressing moved from static helpers to the profile
(`chip_address`, `command_address`, `chip_index`, `is_chip_id`); the
renderer binds per-chip writes to the shifted command address; a new
`InitStep::VersionMaskNoEnable`; the stream decoder switches to nine-byte
frames while a chain answers the chip-id read (`set_frame_len`, driven by
the chain driver around `CountChips`) and back to eleven after; the
simulator answers chip-id reads with nine-byte frames and accepts
single-chip commands at the shifted address; `warpaxe-simuart --chip
bm1373`. The board profile names its chip (`Asic`), and the firmware
builds the chain from it (`board::chip()`), so `boardModel = q1373`
selects the BM1373 without a rebuild.

Checked: render test against NerdOS's `send6` list for four chips,
decoder test for the nine-byte mode, and the chain driver against the
simulator with four BM1373 (addresses 0/16/32/48, no rule violation, no
CRC failure, every nonce attributed). That proves driver and simulator
share one reading of the reference, not that the reading is right; the
first Gaia session settles it, starting with the nine-byte chip-id reply
and a single-chip read at address ÷ 4.


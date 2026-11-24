# WarpForge ↔ WarpTune — which parts of this are actually worth it?

*As of 2026-09-22. Decision paper, not a build instruction.*

---

## The uncomfortable preliminary remark

WarpTune **already** has the control intelligence: `regulator.rs` (3220 lines,
ladder controller with hysteresis, dwell, guards, step memory),
`ladder.rs` (vendor tables incl. BM1370, voltage curve, beyond-spec),
`journal.rs`, `pool.rs`, point-change detection, energy mode.

A firmware of our own rebuilds **none** of that which you do not already have. It
only delivers things the Sidecar *fundamentally* cannot deliver. So the question
is not "build or not", but: **which four or five things are those,
and what does each of them cost individually?**

---

## WarpTune's bottlenecks today — and who can solve them

| # | Bottleneck | Solvable in the Sidecar? | What a firmware would change |
|---|---|---|---|
| 1 | **Hashrate is a nominal figure.** AxeOS reports `frequency × smallCores`. The Regulator has to infer from `error_pct` and reject strings whether the point really holds. | ❌ no | `hashrate_effective` from shares + `hashrate_confidence`. The Regulator would get a *measured* target quantity instead of an estimated one. |
| 2 | **Reject semantics are guesswork.** `is_benign_reject()` classifies by text patterns ("stale", "low difficulty"). Every pool phrases it differently. | ⚠️ only approximately | Local share verification before submit → stale/invalid is never submitted in the first place, HW errors are cleanly separated. Reject rates would become meaningful. |
| 3 | **Latency of the control loop.** Poll → decide → PATCH → 100 ms application → next poll. An unstable point hashes garbage for seconds before anyone intervenes. | ❌ no | Local state machine reacts in ms, the Sidecar keeps deciding the *strategy*. |
| 4 | **Ramp ordering is up to the firmware.** Correction: `patch_operating_point()` sends `frequency` + `coreVoltage` in **one** PATCH — the Sidecar does it right. But which of the two the firmware applies first, it cannot dictate. | ❌ no | Explicit ramp: V before f going up, f before V going down, PLL change at the job boundary. **See the incident below — this is not theory here.** |
| 5 | **Sidecar silent = device unguarded.** If the Mac service goes down, the last point stays put — with no safeguard. | ❌ no | Anchor + TTL: the device falls back on its own to the last *validated* point. |
| 6 | **Reboot causes invisible.** A device restarts, the journal sees a gap. Why? | ❌ no | Post-mortem log via `/api/v1/postmortem`. |
| 7 | **Firmware differences.** ESP-Miner / NerdOS / Forge-OS report the same thing under three names (`temp2` vs `chiptemp1/2`). `axeos.rs` is full of fallbacks. | ⚠️ maintainable | Solves only *one* device. The others remain. No real gain. |
| 8 | **Frequency readout lies.** The PLL does not hit every requested frequency; what is displayed is the requested one. J/TH calculations are correspondingly skewed. | ❌ no | PLL solver reports the achieved frequency. |

**Conclusion of the table:** 6 of 8 bottlenecks are solvable only in the firmware. That is
a strong argument — but only if the firmware actually runs, and M0–M5 are
~20–26 days before the first share arrives.

---

## The three expansion stages, sorted by benefit per day

### Stage A: simulator + `forge-core`, without firmware (M1–M3, ~10–12 days)

**What comes out of it:** A Rust library that speaks BM1370 frames, solves the PLL
and knows the register map — plus a host simulator with fault injection.

**What that delivers *immediately*, without a single line of firmware:**

1. **The PLL solver corrects `ladder.rs` today.** Your ladder is a
   25 MHz grid with interpolated voltages from `device_config.h`. With the
   solver you get the actually reachable points — and the comment in
   `ladder.rs` ("Upstream does not define the pairing; it is our reading of the
   two lists") would no longer be an assumption, but computed.
2. **`forge-sim` as a test device for WarpTune.** Today you test the Regulator
   against fixtures (`crates/tune/src/fixtures`). With the sim + a thin
   AxeOS emulator in front of it you test it against a device that *can
   misbehave*: run away thermally, hang above X MHz, brownout at
   low voltage. Those are exactly the cases the Regulator was built
   for — and which today you only catch when they happen in the field.
3. **Multiple virtual devices.** Test the parallel-session logic and the
   energy attribution against 8 simulated miners, without running 8 devices.

**Verdict: This is by far the best ratio.** It improves what you
*have in production today*, and at the same time it is the foundation for everything
further. If you stop after Stage A, you have still won.

### Stage B: firmware up to the first share + API (M4, M5, M8, ~12–16 days on top)

Now a Gamma runs on our own firmware, WarpTune talks to it via the
AxeOS compatibility layer, and bottlenecks 1, 2, 4 and 8 are gone.

**Prerequisite:** a Gamma you can sacrifice. Not the one currently sitting in the
7-day Gamma test.

**Verdict:** Worth it if Stage A was fun. Do not decide before that.

### Stage C: the rest (M6, M7, M9, M10, ~18–24 days)

Recovery paths, local state machine, OTA health gate, UI. This is the part that turns
"works for me" into a device that you leave running unattended
for weeks.

**Verdict:** Only once Stage B convinces in continuous operation. And honestly: this is the
point where a learning project turns into a maintenance contract.

---

## Concrete coupling points to WarpTune

If it gets built, report these fields **additionally** in the AxeOS compat
layer — `axeos.rs` reads optionally and degrades cleanly, so it costs
nothing there to add them:

| Field | Benefit in the Regulator |
|---|---|
| `hashrateEffective` / `hashrateConfidence` | Replaces the inferences from `error_pct`; JUDGE can wait for confidence instead of for time |
| `frequencyActual` | Correct J/TH calculation |
| `hwErrorsByChip[]` | With multi-chip: outliers instead of board average |
| `desyncCount` | Early indicator of signal integrity |
| `lastResetReason` | Explains journal gaps |
| `anchorPoint` | Sidecar sees what the device falls back to |
| `operatingPointState` | RAMP/SETTLE/JUDGE/HOLD — Sidecar knows when measurements are valid |

The last field is the most valuable one: today WarpTune has to guess when the
settle phase is over. The device knows.

---

## Recommendation

1. **Build Stage A** (~10–12 days), but as a standalone project with its own
   value: "BM1370 protocol library + simulator". Do not frame it as a firmware
   precursor — then you are under pressure to keep going.
2. **Hook `forge-sim` into the WarpTune tests.** That is the day it
   pays off.
3. **Re-evaluate Stage B afterwards**, with the experience from A and once the
   Gaia hardware (NerdAxe Gaia) is there (then you will also know whether BM1373 is a profile diff or
   a surprise).
4. **BM1366/1368**: extract profiles, do not verify. See design doc
   §12.1.

**What does NOT happen now:** None of this gets built as long as v1.32.0 is not
deployed and the Gamma test is not done.

---

## Field evidence: the silent VR death on the Gaia, 2026-09-22 11:44 (09:44 UTC)

On the day of commissioning, less than 12 hours after this document was drafted,
exactly the failure case occurred for which §6.3 in the design doc provides
fault injection. The sequence from the WarpTune journal:

```
09:44:46  decision  up 420→440 MHz, predicted 38.5 W of 40.0 W allowed
09:44:46  write     {"frequency":440,"coreVoltage":970}     ← one PATCH, both
09:44:51  write_verified  ok
09:45:16  heartbeat f=440  V=970  P=4.99 W  hr=2655  temp=48.6  vr=49.5
          "point changed 420→440 MHz @ 960→970 mV: settling 45s, judged after 600s"
   ...    (six minutes of nothing)
09:51:10  write     restore on shutdown          ← the owner intervenes by hand
```

30 seconds after the write: power from **36 W to 4.99 W**, temperature from
61 to 48.6 °C, VR from 72 to 49.5 °C. The VR output was at 0, the chip was
no longer hashing.

**What the device reported about it:** `shutdown: false`. No fault flag, no
overheat flag, no reboot. And — this is the point — **`hashrate_ghs: 2655`**. The
reported hashrate stayed fully in place while the device was doing nothing anymore. That is
bottleneck 1 of this table, demonstrated live: the nominal figure from
`frequency × smallCores` survives the death of the chip.

The only honest measurement in the entire data set was the **power**.

### Three lessons

**1. For WarpTune (implementable immediately):** The Regulator did not react for six
minutes because after a point change it was sitting in `settling 45s, judged after 600s`.
But a drop from 36 W to 5 W does not need 600 seconds of
deliberation — that is an emergency-stop signal, not a measurement. What is missing is a
watchdog that, **during** the settle/judge phase, falls back immediately to the last
running point on a catastrophic power collapse (e.g. < 30 % of the predicted
value). This is independent of any firmware question and
should be built regardless of how the firmware project turns out.

**2. For the ramp ordering:** The step changed frequency *and* voltage
at the same time (420→440 MHz, 960→970 mV). WarpTune correctly sent that as
one PATCH — but the order in which NerdOS applies the two values
cannot be influenced. If the frequency is set before the voltage,
the chip briefly runs at 440 MHz on 960 mV. Whether that was the cause is thereby
not proven — but it is the kind of indeterminacy that a firmware of our own
eliminates and an API never will.

**3. For the firmware design:** This case belongs in the fault injection as a mandatory
scenario — it is more realistic than anything I came up with at my
desk:

```rust
Fault::SilentVrShutdown {
    above_mhz: 440,
    // VR output at 0, power at standby level,
    // NO fault flag, NO reboot, register access stays answerable
}
```

Expected behaviour of the firmware: the power collapse is detected within one
tick, the point counts as bad, fallback to the last anchor,
entry in the post-mortem log. Without manual intervention, without network connection, without
power disconnection.

### What the owner did

`ladder_cap_mhz = 400` — one step below the last point that ran (420),
with the note *"raise only with a judged 400/420"*. That is the right
reaction: the device point is the validated ceiling, and validated is only
what has survived a judgement.

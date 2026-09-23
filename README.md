# dvb-WarpAxe

BM13xx ASIC wire protocol as a `no_std` Rust library, plus a host-side chip
simulator with fault injection. Stage A of a firmware project; the library
and the simulator have value on their own and are built first.

## What is here

| crate | purpose | status |
|---|---|---|
| `warpaxe-core` | frames, CRCs, stream decoders both ways, PLL solver, job/result semantics, register payloads, chip profiles with the init sequence as data — pure, `no_std`, no alloc | complete for BM1370 |
| `warpaxe-chain` | the chain driver: bring-up from the profile's init list, chip enumeration, baud switch, PLL ramp, work dispatch, nonce attribution to work and chip, duplicate and stale filtering, per-chip silence watchdog — over a `Uart` trait, `no_std`, no alloc; the code the firmware will run | BM1370, proven against the simulator |
| `warpaxe-sim` | `SimChip`/`SimChain`: a BM1370 chain that answers on a UART without hashing, plus a `Board` with watts, degrees, a VR and a fan — and faults for both | core done |
| `warpaxe-axeos` | an AxeOS-compatible HTTP device on top: `/api/system/info`, `/api/system/asic`, `PATCH /api/system`, `POST /api/system/restart`, and `/sim/*` to break it | serves WarpTune |

Run the device:

```bash
cargo run --release -p warpaxe-axeos -- --port 8811 --time-scale 30
curl -s http://127.0.0.1:8811/api/system/info | jq .
curl -s -X POST -d '{"board":{"SilentVrShutdown":{"above_hz":600000000}}}' http://127.0.0.1:8811/sim/fault
```

`--time-scale 30` makes the simulated board's thermal and hashrate
dynamics run thirty times faster than wall-clock; a tuner's own timers
are unaffected. `--hostname` and `--mac` let several instances differ.

Nothing in `warpaxe-core` does I/O, reads a clock or allocates. That is what
lets the same code run under `cargo test` on a laptop, inside the simulator,
and — later — on an ESP32-S3.

## Provenance

The BM1370/BM1373 have no public datasheet. What is known comes from reverse
engineering, chiefly [ESP-Miner](https://github.com/bitaxeorg/ESP-Miner)
(`components/asic/`, GPL-3.0). Every constant lifted from there says so in
its doc comment; every test vector says which ESP-Miner test it came from.
Values read from *source comments* rather than test assertions are marked
"comment" and rank lower until a hardware capture confirms them.

This project is GPL-3.0-or-later for that reason.

## Build and test

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build -p warpaxe-core --target thumbv7em-none-eabihf   # proves no_std
```

The regulator scenarios (WarpTune's `Regulator` in-process against the
simulated devices, `crates/scenarios/`) are a separate package with a git
dependency on dvb-WarpPool pinned to a commit:

```bash
cargo test --manifest-path crates/scenarios/Cargo.toml
```

## Design

The design document and the decision record are in [`docs/`](docs/):

- [`docs/00-Design-BM1370.md`](docs/00-Design-BM1370.md) — architecture,
  chip profiles, PLL, simulator, findings from stage A (§13) and the
  transport decision USB-NCM + USB bridge (§14);
- [`docs/01-Benefit-for-WarpTune.md`](docs/01-Benefit-for-WarpTune.md) — what
  this buys WarpTune, concretely;
- [`docs/02-Demo-WarpTune-vs-Sim-2026-09-23.txt`](docs/02-Demo-WarpTune-vs-Sim-2026-09-23.txt)
  — the first run of WarpTune against the emulator, unedited.

The repo copy is canonical and in English; a German draft in Oliver's notes is history, not a mirror. Short version:

- registers and init sequences are **data** (a chip profile), not code per chip;
- the PLL is **solved**, not looked up, so the reported frequency is the real one;
- the simulator exists to make field failures reproducible — including the
  one that started this: a VR that shut down silently while the firmware kept
  reporting full hashrate. `warpaxe-axeos` replays it over HTTP so a tuner can
  be tested against it: `device::tests::silent_vr_shutdown_looks_like_the_real_one`.

# dvb-WarpAxe

BM13xx ASIC wire protocol as a `no_std` Rust library, plus a host-side chip
simulator with fault injection. Stage A of a firmware project; the library
and the simulator have value on their own and are built first.

## What is here

| crate | purpose | status |
|---|---|---|
| `warpaxe-core` | frames, CRCs, PLL solver, chip profiles — pure, `no_std`, no alloc | frames + CRCs done |
| `warpaxe-sim` | a BM1370 that answers on a UART without hashing, and can be made to misbehave | placeholder |

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

## Design

The design document and the decision record live outside this repo, in
Oliver's notes (`Claude/WarpForge-Firmware/`). Short version:

- registers and init sequences are **data** (a chip profile), not code per chip;
- the PLL is **solved**, not looked up, so the reported frequency is the real one;
- the simulator exists to make field failures reproducible — including the
  one that started this: a VR that shut down silently while the firmware kept
  reporting full hashrate.

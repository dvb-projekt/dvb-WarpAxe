# tools/sha-bench: the SHA peripheral bench as a program of its own

The firmware's SHA-peripheral bench (`firmware/components/warpaxe_glue/
warpaxe_shabench.c`, docs/06) compiled as a plain ESP-IDF project, for
boards that do not run WarpAxe: first the ESP32-P4, whose SHA engine
ESP-IDF describes like the S3's (START/CONTINUE, writable digest, DMA
mode), measured for us by mutatrum (OSMU) on a Waveshare ESP32-P4-ETH.
No network, no mining: it hashes the Bitcoin genesis header, 2048 nonces
per variant, checks every hardware hash in software, must find the
genesis nonce, and prints a table plus one JSON line (`SHABENCH {...}`)
on the console, once after boot and then every 30 s.

The hash code is the firmware's own files, compiled from
`firmware/components/warpaxe_glue` (see `main/CMakeLists.txt`), not a
copy: `warpaxe_sha_regs.h` has the register access for the Xtensa (S3)
and the RISC-V (P4), `warpaxe_shabench.c` takes the DMA bus the chip's
SHA hangs on (AHB GDMA on the S3, AXI DMA on the P4, with the cache
write-back the P4 needs), `warpaxe_cpuhash.c` is the software reference
and the firmware's production loop (variant 5).

## Build

A plain ESP-IDF project: any ESP-IDF 5.3 install with the target's
toolchain does (for the P4 that is `riscv32-esp-elf`, which
`install.sh esp32p4` brings along). With ESP-IDF exported:

```bash
cd tools/sha-bench
idf.py set-target esp32p4      # or esp32s3
idf.py build
idf.py -p PORT flash monitor   # or: esptool.py --chip esp32p4 -p PORT write_flash @build/flash_args
```

`build.sh <target>` does the same with one build directory per target,
`build-<target>/`, each with its own sdkconfig (`sdkconfig.defaults`
plus `sdkconfig.defaults.<target>`), and merges the result into
`build-<target>/sha-bench-<target>.bin`, one image for offset 0, with
its SHA-256 beside it. Without ESP-IDF on the PATH it falls back to the
v5.3.3 that esp-idf-sys installs for the firmware under `~/.espressif`.
Flashing and reading the output: `README-mutatrum.md` (the P4
instructions as they go out). Built and tested with ESP-IDF v5.3.3 only
(2026-10-15); a newer ESP-IDF may name the P4's clock and revision
options differently, which `sdkconfig.defaults.esp32p4` then only warns
about.

ESP32-P4 revisions: ESP-IDF v5.3.3 supports v0.x and v1.x (the Waveshare
ESP32-P4-ETH is v1.3). A v3.x part needs ESP-IDF v5.4 or later and an
image of its own; the bootloader of this one refuses it with a message
about the chip revision.

## Output

The table follows docs/06's sections: T (register costs), A (busy
windows, polled and as counted delays, and after a drained bus), B (when
the engine samples TEXT, and whether a block leaves TEXT alone), D (the
DMA path), V (the loop variants 0–4 of docs/06 plus 5, the firmware's
`wa_sha256d_hw_scan`). Cycles are CPU cycles at the printed clock;
`none` is "not found" (0xffffffff in the JSON). The JSON keys are the
firmware's `cpuMiner.bench` names (`/api/system/info` in a `shabench`
build) plus `chip`, `rev`, `arch`, `cores`, `apbMhz`, `xtalMhz`, `idf`,
`vPipelined` and `genesisFound`.

The S3 numbers to compare against: docs/06. A standalone run on the
T-Display-S3 must reproduce them within a few cycles (the firmware's own
run has Wi-Fi and the display worker beside it, the standalone one does
not).

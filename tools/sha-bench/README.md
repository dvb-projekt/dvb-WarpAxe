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

ESP-IDF v5.3.3 at `~/.espressif/esp-idf/v5.3.3` (or exported as
`IDF_PATH`), with the `riscv32-esp-elf` toolchain installed for the P4
(`idf_tools.py install --targets esp32p4 riscv32-esp-elf`).

```bash
tools/sha-bench/build.sh esp32p4
tools/sha-bench/build.sh esp32s3
```

Each target builds in `build-<target>/` with its own sdkconfig
(`sdkconfig.defaults` plus `sdkconfig.defaults.<target>`) and ends with
`build-<target>/sha-bench-<target>.bin`, one image for offset 0, and its
SHA-256 beside it. Flashing and reading the output: `README-mutatrum.md`
(the P4 instructions as they go out).

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

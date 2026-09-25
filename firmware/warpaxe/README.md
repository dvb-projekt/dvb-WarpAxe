# warpaxe (firmware)

The WarpAxe firmware for ESP32-S3 miners, stage B. Rust `std` on ESP-IDF
v5.3 (esp-idf-svc 0.51), TinyUSB through the shared C glue in
`../components`. Runs today on the LilyGO T-Display-S3 (the controller of
every Nerd miner); the Bitaxe Gamma follows with a board profile.

What works, verified on a NerdMiner V2 over USB-C only (no Wi-Fi):

| Piece | State |
|---|---|
| USB-C as CDC-NCM network adapter with DHCP server on 192.168.7.1/24, CDC-ACM console | works, 1.2 MB upload in 1.9 s |
| Wi-Fi station when the config has an SSID (own thread, reconnects) | built, not yet exercised |
| Config in NVS, AxeOS field names | `PATCH /api/system`, persists across restart |
| `GET /api/system/info` (AxeOS shape, mining fields honest zeros), `GET /api/v1/state` | works |
| `POST /api/system/restart` | works |
| Chain over the real UART (GPIO17/18 → USB-UART adapter → `warpaxe-simuart`) | **passes**: init, baud switch, PLL ramp, work, nonces, live frequency PATCH; design doc §14.14 |
| Board layer (`src/board.rs`, `warpaxe-board`): profile from `boardModel`, TPS546 regulator, EMC2101 sensor and fan PID, overheat stop, sensor fields in the info | devkit profile verified (no I²C, nothing changed); Bitaxe Gamma profile written from ESP-Miner, untested on hardware |
| Display by profile: ST7789 LCD (T-Display-S3) or SSD1306 OLED (Bitaxe, `warpaxe_board::ssd1306`, four status lines) | LCD path verified on the devkit; OLED untested until the Gamma |
| Board profiles: `bitaxe-gamma`, `t-display-s3`, `nerdaxe-gamma`, `nerdqaxe-plusplus`, `nerdoctaxe-gamma`, `q1370`, `q1373` (config key `boardModel`, applied at boot) | Nerd multi-chip boards report their regulator as unsupported and leave the chain unpowered |
| Chip by profile: `ASICModel`, `smallCoreCount` and the chain driver come from the board profile's chip (BM1370, or BM1373 for `q1373`) | devkit reports BM1370 as before |
| `flipscreen` / `invertscreen` (OLED orientation, live on the next draw) | keys round-trip on the devkit; effect untested until the Gamma |
| `tools/flash-backup.sh` (full flash image + sha256 + restore notes before the first WarpAxe flash) | script only, run it on the Gamma first |
| `PATCH /api/system` with `coreVoltage` | persists; goes live through the regulator on a board that has one |
| `PATCH /api/system` with `frequency` | works; the mining loop ramps the running chain to the new clock without a restart (WarpTune's write path) |
| `POST /api/system/OTA` (raw app image; received into PSRAM, flashed on its own thread, reboot into the other slot) | works, ota_0 → ota_1 in 13 s |
| Display: ST7789 status screen with the last log lines and the previous crash | works |
| Crash handling: core dump to flash, summary in `/api/v1/state` and on screen, 30 s on USB-Serial/JTAG after a crash before USB OTG takes the PHY | works |
| Flash-back: `GET /bootloader` or `POST /api/v1/bootloader`, or a `B` byte on the CDC port → ROM download mode with the PHY handed back | works |
| Stratum V1 client (`crates/stratum` + `src/stratum.rs`): configure (version rolling), subscribe, authorize, notify → job with coinbase, merkle root and chip-ordered header fields; difficulty and mask tracking; reconnect with backoff; state in `/api/v1/state` and on the display | works against pool.warppool.org over Wi-Fi |

| Chain driver (`warpaxe-chain`) on UART1, GPIO17 TX / GPIO18 RX, on its own miner thread: bring-up, retries every 10 s while no chip answers, work from Stratum (fresh extranonce2 per submission, retire on clean jobs), nonces checked against the pool difficulty with the host-side header hash, shares queued to the Stratum writer, accept/reject accounting, hashrate estimate; state in `/api/v1/state` and the AxeOS fields | runs; waits for chips (a USB-UART adapter to `warpaxe-simuart`, or an ASIC) |

Not yet: board profiles, fan/VR/sensor drivers, Wi-Fi provisioning UI.

## Hardware-in-the-loop without an ASIC

```bash
cargo run -p warpaxe-simuart -- --port /dev/cu.usbserial-XXXX --chips 1
```

on the Mac, with the adapter's TX on GPIO18, RX on GPIO17, GND on GND
(3.3 V logic, VCC unconnected). The simulator answers the board like a
BM1370 chain, follows its baud switch and treats a 3 s silence followed
by traffic as a chip reset.

## Build and flash

```bash
. ~/.espup-export.sh
export PATH="/opt/homebrew/opt/python@3.12/libexec/bin:$PATH"   # ESP-IDF's Python
cargo build --release
```

First flash (board in ROM download mode: BOOT held while plugging in, or
`GET /bootloader` on a running WarpAxe/proof firmware):

```bash
OUT=$(ls -d target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*/out | head -1)
python3 -m esptool --port /dev/cu.usbmodemXXXX --chip esp32s3 --before no_reset --after no_reset --no-stub write_mem 0x6000812C 0 0xFFFFFFFF
espflash flash --no-skip --port /dev/cu.usbmodemXXXX --flash-size 16mb \
  --bootloader $OUT/build/bootloader/bootloader.bin \
  --partition-table $OUT/build/partition_table/partition-table.bin \
  -a watchdog-reset target/xtensa-esp32s3-espidf/release/warpaxe
```

The `write_mem` clears the force-download flag the firmware sets for
`/bootloader` (it lives in the RTC domain and survives resets); the
watchdog reset starts the app, a DTR/RTS reset on this board would not.

Every later update goes over USB without touching the board:

```bash
espflash save-image --chip esp32s3 --flash-size 16mb target/xtensa-esp32s3-espidf/release/warpaxe /tmp/warpaxe.bin
curl -X POST --data-binary @/tmp/warpaxe.bin -H 'Expect:' http://192.168.7.1/api/system/OTA
```

## Traps (all learned on 2026-09-23)

- **TinyUSB must run the DWC2 in DMA mode** (`CONFIG_TINYUSB_MODE_DMA`).
  In slave mode the NCM IN path stalls after ~14 KB of traffic and never
  recovers ("Packet cannot be accepted on USB interface").
- esp-idf-sys does not rebuild `../components` on its own: touch
  `sdkconfig.defaults`. After changing `sdkconfig.defaults` delete the
  generated `target/**/esp-idf-sys-*/out/sdkconfig`.
- espflash skips segments it believes unchanged: `--no-skip`.
- `TcpStream::try_clone` is `ENOSYS` on ESP-IDF (no `dup`): read and write
  through `&TcpStream` instead.
- The CDC console port (`/dev/cu.usbmodem…`, not the JTAG one) carries
  the logs once USB is up; reading it with a plain non-blocking open works.

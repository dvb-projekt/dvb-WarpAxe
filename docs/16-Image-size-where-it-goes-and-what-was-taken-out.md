# Image size: where it goes and what was taken out

Written 2026-11-13 on main after v0.1.44 (`178a1d9`), on a handover
from dvb-WarpAxe #2. Nothing here ran on a device: the sizes are
measured on the build, the behaviour is argued and host-tested, and the
list at the end says what a device has to show.

## Why

The release script refuses an app image above `0x2F0000` (3,080,192
bytes): a T-Display-S3 that ran NerdMiner keeps its `config.json` from
flash `0x310000` on, the first install writes only `ota_0`, and the
first start imports that file (`tools/release-firmware.sh`,
`src/cpuminer.rs import_nerdminer`). v0.1.44 was 66,864 bytes below the
limit, after +38 KB since v0.1.42. One more feature of that size and
the release stops.

| | Image | Below 0x2F0000 |
|---|---|---|
| main `178a1d9` (v0.1.44 and one web UI commit) | 3,012,896 | 67,296 |
| main after this work (`fcac977`) | 2,780,944 | **299,248** |

231,952 bytes less. No function is gone, nothing looks different, no
RAM is added (see "RAM" below for the 24 bytes of padding).

## How it is measured

```bash
cd firmware/warpaxe && cargo build --release
espflash save-image --chip esp32s3 --flash-size 16mb \
    target/xtensa-esp32s3-espidf/release/warpaxe /tmp/warpaxe.bin
python3 tools/image-breakdown.py --image /tmp/warpaxe.bin
python3 tools/image-breakdown.py --items .flash.rodata 40
```

`save-image` is what the release script measures. `image-breakdown.py`
sums the final link's map (`…/esp-idf-sys-*/out/build/libespidf.map`)
by section and by who put the bytes there; `--items` lists a section's
largest pieces. Function sizes come from
`xtensa-esp32s3-elf-nm -S --size-sort -C` on the ELF.

Three things about the numbers:

- The image is its segments one after another. The code segment must
  start at a 64 KB boundary of the file, and espflash fills the gap
  after the read-only data with pieces of the RAM segments, so the
  image shrinks byte for byte with what is taken out (no steps of
  64 KB).
- A build is reproducible: the same tree gives the same image (checked
  twice here, with a full ESP-IDF rebuild between).
- Every change to the firmware crate moves functions between its 16
  codegen units, and with them what gets inlined where. Functions that
  were not touched change by a few bytes from build to build
  (`miner::run` 21,224 → 21,434 across this work). That is the same
  noise every release has.

## Where the bytes were (main `178a1d9`, 3,012,896 bytes)

| Part of the image | Bytes | |
|---|---|---|
| Code in flash (`.flash.text`) | 1,908,870 | 63 % |
| Read-only data in flash (`.flash.rodata`, app descriptor) | 962,936 | 32 % |
| Code that runs from internal RAM (`.iram0`) | 94,942 | 3 % |
| Initial values of RAM data (`.dram0.data`) | 46,028 | 2 % |
| Headers, checksum, padding | 120 | |

### Read-only data, 962,680 bytes

| What | Bytes | Form |
|---|---|---|
| Boot screen pictures (`src/splash`) | 207,522 | raw RGB565; the OLED wordmark twice |
| – LCD frame 320×170 | 108,800 | |
| – eight makers' wordmarks | 93,738 | RGB565 + alpha |
| – header badge, OLED wordmark | 3,960 + 2 × 512 | |
| Web UI `index.html` (432,814 bytes) | 138,472 | gzip, zlib level 9 |
| Web fonts (Inter, JetBrains Mono, Space Grotesk) | 82,492 | woff2, Latin subsets |
| `wordmark.webp`, `favicon.svg` (twice) | 18,836 + 2 × 2,125 | |
| Display fonts (`src/gfx/fonts`) | 59,899 | 40,827 of 4-bit coverage, 1,192 glyphs × 16 bytes of table |
| Other data of the firmware crate | ~75,800 | texts, display translations, tables |
| C strings: log texts, assert texts, names | 147,406 | merged by the linker |
| mbedTLS: CA bundle 54,349, tables | 63,713 | |
| secp256k1: generator table 32,768 | 34,127 | Stratum V2's Noise handshake |
| core, alloc, std | 32,949 | float parsing tables 10,416 |
| Workspace crates | 29,085 | the board profile list twice (2 × 3,240; a third copy is in the firmware crate, see below) |
| Other Rust crates | 19,748 | |
| Wi-Fi, ESP-IDF, libc, lwIP, HTTP, USB | 48,378 | |

### Code in flash, 1,908,870 bytes

| Who | Bytes |
|---|---|
| Rust: the firmware crate | 704,150 |
| Rust: workspace crates (`warpaxe-*`) | 207,768 |
| Rust: other crates (serde_json, esp-idf-svc, noise, rand, …) | 127,834 |
| Rust: core, alloc, std | 105,242 |
| secp256k1 (C, for Stratum V2) | 39,451 |
| C: Wi-Fi, WPA, PHY (Espressif's libraries) | 236,059 |
| C: ESP-IDF system, drivers, FreeRTOS | 120,710 |
| C: lwIP, netif, mDNS | 117,486 |
| C: mbedTLS, esp-tls | 100,954 |
| C: libc, libm, libgcc, pthread | 83,414 |
| C: HTTP server and client | 29,569 |
| C: USB (TinyUSB, NCM, CDC) | 22,964 |
| Alignment, literal pools | 13,269 |

Generic code counts where it is instantiated, so "the firmware crate"
holds serde_json's parsers for the firmware's structs, the maps and
sorts of its types and every thread's body. By function name, over all
Rust code in flash:

| What | Bytes | Functions |
|---|---|---|
| serde and serde_json, with their instances for the firmware's types | 154,726 | 584 |
| core, alloc, std, other | 148,462 | 1,332 |
| `api` | 77,823 | 30 |
| esp-idf-svc, esp-idf-hal | 76,298 | 535 |
| `board` | 59,543 | 57 |
| `screens` | 44,808 | 26 |
| Thread bodies and spawns (22 `Builder::spawn` instances, 15,297) | 33,561 | 116 |
| `BTreeMap` instances | 30,698 | 268 |
| `stratum` | 30,669 | 23 |
| `config` | 29,795 | 52 |
| `miner` | 27,341 | 27 |
| `warpaxe-avalon` | 26,919 | 19 |
| Slice sort instances (seven call sites) | 26,864 | 92 |
| `update` | 26,268 | 21 |
| Drop glue | 23,221 | 269 |
| `warpaxe-board` | 22,548 | 147 |

The single largest functions: `api::system_info` 47,402 (one `json!`
with several hundred keys), `screens::Ui::lcd` 30,796, `board::run`
22,604, `miner::run` 21,224, and the parser of `Config`, which exists
twice (from bytes at boot: 16,177 + 11,621; from a `Value` in a PATCH:
12,748 + 10,344).

Bluetooth is not built (`CONFIG_BT_ENABLED` is not set), ESP-IDF is
compiled for size (`CONFIG_COMPILER_OPTIMIZATION_SIZE`), and unused
components (FAT, SPIFFS, MQTT, …) are dropped by the linker.

## What was taken out

Each line is one commit; the image is measured after it.

| # | Measure | Commit | Before | After | Saved | Below 0x2F0000 |
|---|---|---|---|---|---|---|
| | main | `178a1d9` | | 3,012,896 | | 67,296 |
| 1 | Boot screen pictures packed | `f3a3451` | 3,012,896 | 2,849,232 | 163,664 | 230,960 |
| 2 | Web UI packed with zopfli | `21ee1b6` | 2,849,232 | 2,842,144 | 7,088 | 238,048 |
| 3 | API documents built with one call per entry | `88c3273` | 2,842,144 | 2,789,904 | 52,240 | 290,288 |
| 4 | Debug and trace log lines compiled out | `ce4dc03` | 2,789,904 | 2,782,144 | 7,760 | 298,048 |
| 5 | Splash unpacked once per boot | `fcac977` | 2,782,144 | 2,780,944 | 1,200 | 299,248 |
| | Sum | | 3,012,896 | 2,780,944 | **231,952** | **299,248** |

### 1. Boot screen pictures packed

The LCD's frame and the eight wordmarks (203,538 bytes raw) are stored
as raw DEFLATE streams (35,596 bytes: the frame 17,008, a wordmark
1,805 to 3,087). `tools/make-splash.py` writes each `NAME.z` next to
its raw file (`--pack` does only that, without Pillow and without
drawing anything again); the firmware includes only the packed file.
The raw files stay in the repo as what the packed ones must unpack to.

`crates/inflate` unpacks: `no_std`, no `unsafe`, no allocation, no
static tables, an error instead of a panic on any input. Codes are read
bit by bit (the method of zlib's `puff.c`), which costs speed and saves
the lookup tables: 3.6 KB of code, about 1.3 KB of stack while it runs.

- The frame is unpacked straight into the LCD's frame buffer, which
  exists anyway (108,800 bytes in PSRAM): no second buffer.
- A wordmark is unpacked into a PSRAM block that lives for the one draw
  (at most 14,586 bytes, asked for from PSRAM by name: a plain
  allocation below 16 KB would come from the internal heap). Without
  such a block the model name is drawn instead, as for a board without
  a wordmark.
- The display thread's stack is in PSRAM (16 KB); the splash path uses
  less of it than a normal frame does (`Ui::frame` + `Ui::lcd` have
  3,920 bytes of frames, the splash 576 + 1,280).
- Nothing touches flash or NVS from that thread, as before.

Pixel identity is a host test (`crates/inflate/tests/splash.rs`): every
`.z` in `src/splash` unpacks to its raw file byte for byte, into a
buffer of exactly the picture's size, and the sizes match the names.
The decoder itself is tested against zlib vectors of the three block
types, round trips against miniz_oxide at every level, streams cut
short at every byte and bit-flipped streams (`tests/vectors.rs`).

The header badge (3,960 bytes, on every frame of every page) and the
OLED wordmark (512) stay raw. The favicon and the OLED wordmark were in
the image twice (two `include_bytes!` of the same file); each is one
static now.

### 2. Web UI packed with zopfli

`build.rs` packed `index.html` with zlib's best level: 138,472 bytes.
zopfli writes the same gzip format in 131,403. The browser's path is
unchanged (`Content-Encoding: gzip`), and the build still unpacks the
result with another implementation (flate2) and compares it with the
source. zopfli is compiled optimised (`[profile.release.package.zopfli]`),
so the step takes a second when the page changed.

### 3. API documents built with one call per entry

`serde_json::json!` expands every `"key": value` into the key's
`String`, the value's conversion and the map insert, inline: about 140
bytes of code per key. `crates/jsonval` is the same macro (serde_json's
rules, changed in two places) with those steps in two functions that
are never inlined, `put` and `val`. The value built is the same: same
entries, same order, same conversions. The tests expand every form of
the grammar with both macros from the same tokens and compare
(`tests/same_as_serde_json.rs`).

| Function | Before | After |
|---|---|---|
| `api::system_info` | 47,402 | 17,260 |
| `api::diagnostics` | 6,127 | 3,310 |
| `selftest::status_json` | 4,776 | 1,898 |
| `api::v1_state` | 4,274 | 1,581 |

Used in `api`, `selftest`, `swarm`, `history`, `clock`, `events`,
`regulator`. `board.rs` keeps serde_json's macro: 1,360 bytes there,
and it is the chip path's file. The allocations are the same ones in
the same order, so the heap sees no difference.

### 4. Debug and trace log lines compiled out

The logger's ceiling is `CONFIG_LOG_MAXIMUM_LEVEL` (info), in ESP-IDF
and in esp-idf-svc's logger alike, so a `debug!` or `trace!` can never
be printed. Their texts and formatting code were in the image all the
same (two lines of the firmware's own, 67 in esp-idf-svc). The `log`
crate's `release_max_level_info` removes them at compile time. What the
log shows is unchanged.

### 5. Splash unpacked once per boot

The splash is sent to the LCD twice a second for its five seconds, and
with the packed pictures each call would have unpacked them again
(0.7 ms for the frame on the build host; a few tens of milliseconds are
to be expected on the ESP32-S3, not measured). Nothing else draws into
the frame buffer before the splash ends, so it is composed once, and
again only when the board changed (the board detected on a first start
arrives a moment into the splash). The other calls send the frame as it
stands.

### RAM

| | Before | After |
|---|---|---|
| `.dram0.data` (internal RAM, initialised) | 46,028 | 46,044 |
| `.dram0.bss` (internal RAM, zeroed) | 47,000 | 47,008 |
| `.iram0.text` (code in internal RAM) | 93,915 | 93,915 |
| Heap, internal | | no allocation added |
| Heap, PSRAM | | up to 14,586 bytes for one draw of a wordmark |

The same statics are there with the same sizes; the 16 + 8 bytes are
padding between them, which moves with the order the codegen units are
linked in (two of the measurement builds below came out at 46,028 and
47,000 again). The next change to the firmware crate moves it again.

Stack frames (the `entry` of each function, all 9,838 functions that
exist before and after compared): eight grew by 32 bytes or more, none
in the chip driver or the mining loop. `miner::run` 3,392 → 3,408,
`Chain::poll` 240 → 240, `board::run` 1,872 → 1,872,
`api::system_info` 2,224 → 2,240, `main` 1,808 → 1,840,
`stratum::session_v2` 4,000 → 4,064 (on its 32 KB PSRAM stack).

## What was looked at and left

Measured on a build, not shipped. "Saved" is against the image at the
time of the measurement; the figures do not simply add up.

### Needs a device first

| Candidate | Saved | Why it was left |
|---|---|---|
| `lto = "fat"` | 142,448 | Recompiles everything as one unit: the mining loop, the chip driver and every stack frame change. Hashrate and `minerStackFree` have to be measured on each board family. |
| `opt-level = "z"` for everything | 68,272 | The same, and slower code where it was inlined before. |
| `codegen-units = 1` | 38,160 | The same in small. |
| `CONFIG_COMPILER_OPTIMIZATION_ASSERTIONS_SILENT` | 53,472, and 6,896 bytes of internal RAM (`.iram0.text` −4,468, `.dram0.data` −2,428) | An ESP-IDF assert then says only where (the PC), no longer file, line and expression. The core dump is unchanged. A decision about crash reports, not about size alone. |
| `CONFIG_NEWLIB_NANO_FORMAT` (ROM printf) | 44,624 | Taken for v0.1.47 after the check one by one: see "Nano printf" below. |
| `Config` read through one parser instead of two | 31,856 | Three lines in `config::init` (`from_slice::<Value>` then `from_value::<Config>`, the route every PATCH already takes). The result is the same for every stored document, but the boot would build the document as a tree first: about 7 KB of small internal allocations, freed again around the strings that stay. Too close to "no internal RAM" to do unmeasured. |
| `CONFIG_LWIP_IPV6=n` | 25,808 (`.dram0.bss` −1,768) | A function: no IPv6 address, no AAAA. |
| `CONFIG_ESP_ERR_TO_NAME_LOOKUP=n` | 7,600 | Error texts in the log and the API turn into numbers. |
| `opt-level = "z"` for serde_json, esp-idf-svc, anyhow, avalon, diag, displaycfg | 5,808 | Small, and serde_json and esp-idf-svc are in the pool's path. |

### Would change what is seen, or was not worth its risk

| Candidate | Saved | Why it was left |
|---|---|---|
| Web fonts subset further | 10–15 KB estimated | The three files are Latin subsets already. What is left: glyph names (`post`, 10 KB before compression), Inter's weight axis cut to the 400–700 the page uses (`gvar` 36 KB of 123 KB uncompressed), JetBrains Mono's ligatures (`GSUB` 20 KB of 58 KB; they show in what is typed into a field, `//` or `::`, so not removable). Needs fonttools, which is not on the build host, and a rendered comparison in a browser; a subset that is "optically the same" was not provable here. Japanese and Chinese already use the system's fonts. |
| Web UI minified at build | ~4,600 | 109 comment lines; indentation is worth 650 bytes after gzip. Without a JavaScript parser a line starting with `//` is not provably a comment, and the served page would no longer be the source. |
| Display fonts packed | 18,743 (40,827 → 22,084) | Read on every frame, so they would have to stay unpacked: 41 KB of PSRAM for good. |
| Header badge packed | 2,019 | Drawn on every frame. |
| Glyph table 16 → 8 bytes per glyph | up to 9,500 | The text renderer's inner loop. |
| The board profile list in the image three times | 6,480 | `profile::ALL` is a `const` slice, and its 15 profiles are copied into every codegen unit that uses it; a `static` would be one copy. In `crates/board`, the chip side. |
| Seven slice sorts | ~12,000 of 26,864 | `sort_by` on short lists (the Wi-Fi scan, the Avalon search, the diagnostics masks) instantiates the whole stable sort each time. An insertion sort would give the same order; five small changes in four crates. |
| 22 `thread::Builder::spawn` instances | ~14,000 | One boxed closure for all would cost one small internal allocation per thread. |
| `json!` in `board.rs` | 1,360 | The chip path's file. |
| rustc_demangle (22,990) | – | Linked through std's backtrace printing (the panic hook, `anyhow`'s `Backtrace`); only a std without panic messages drops it. |
| mbedTLS cipher suites, Wi-Fi feature switches (WPA3, enterprise, soft AP) | not measured | On the wire, or functions. |

## Nano printf (v0.1.47, 2026-11-18)

`CONFIG_NEWLIB_NANO_FORMAT=y`, on main `e0d3bbd`, built in the same
path for both sides:

| | Image | Below 0x2F0000 |
|---|---|---|
| main `e0d3bbd` | 2,850,960 | 229,232 |
| with nano printf | 2,806,160 | **274,032** |

44,800 bytes less: libc's and libm's formatting code in flash, 42,352
of code and 2,452 of tables. `.iram0.text`, `.dram0.data` and
`.dram0.bss` are byte for byte the same size (libc's 360 + 360 bytes
of statics now come from `libc_nano.a`). Stack frames change only
inside libc: `printf` and `vsnprintf` went through `_vfprintf_r` /
`_svfprintf_r` (800 bytes, with floats) and now through nano's
`_vfiprintf_r` / `_svfiprintf_r` (160) plus `_printf_i` and
`_printf_common` (48 each), 544 bytes less per formatted line. The one
`scanf` in use, `siscanf` in newlib's `tzset` (the display thread's
clock, `clock.rs`, on a 16 KB PSRAM stack), grows by about 200 bytes
(`__ssvfiscanf_r` 560 → 704, `_scanf_i` 64). Every Rust function's
`entry` is unchanged.

Despite the name, the S3 does not take the ROM's copy:
`ESP_ROM_HAS_NEWLIB_32BIT_TIME` keeps ESP-IDF from linking it
(`esp_rom/CMakeLists.txt`), so the formatter is the toolchain's
`libc_nano.a` in flash. Floats still print: ESP-IDF links
`_printf_float` under nano (`newlib_init.c`).

**What nano cannot do.** Its parser knows the length modifiers `h`, `l`
and `L` and the conversions `diouxXcspn` and `efgEFG`
(`nano-vfprintf.o`'s `"hlL"` and `"efgEFG"`). Everything else is printed
as the letter and **takes no argument**, so every argument after it
shifts (`nano-vfprintf_i.o`, the default case): `%llu` prints `lu`,
`%zu` prints `zu`, `PRIu64`, `%jd` and positional `%1$s` likewise. A
`%s` after such a spec would read half of the 64-bit value as a
pointer.

**Where it would print wrongly.** Every printable string of the image
(`save-image` output, all segments, the Wi-Fi blobs included) searched
for a conversion with `ll`, `hh`, `j`, `z`, `t`, `q` or a position;
`esp_rom_printf` (panic handler, core dump writer, boot ROM) has its own
formatter and is not affected:

| String (source) | When | Matters? |
|---|---|---|
| `read error :-0x%04zX:`, `write error :-0x%04zX:` (esp-tls) | a TLS pool or the update loses its connection | The mbedTLS code is gone from that one line (`-0x000zX`); the stratum line after it still names the `EspError`. Last argument, nothing shifts. |
| `pm stop, total sleep time: %llu us / %llu us` (Wi-Fi blob) | Wi-Fi stops or drops | Reads `lu us / lu us`. Nothing parses it. |
| `CCMP replay detected: … PN=%llu, RSC=%llu seq=%u` (Wi-Fi blob) | a replayed frame | Wrong numbers, no `%s` after. |
| `(mesh)pm stop, sleep:%lld …` (Wi-Fi blob) | ESP-MESH only | Not used. |
| `invalid buffer alignment: %zu`, `buffer not aligned to %zu` (gdma) | a programming error in a driver | Last argument, nothing shifts. |
| `%s:%s:%016llx`, `…:%016llx:%s:%s`, `cnonce="%016llx"` (esp_http_client digest auth) | HTTP digest auth | Linked but unreachable: `CONFIG_ESP_HTTP_CLIENT_ENABLE_DIGEST_AUTH` is off. With it on, the `%s` after would crash. |

`tools/check-printf.py` repeats this search on the image and fails on a
broken spec with a `%s` after it that is not one of the digest-auth
lines above; `tools/release-firmware.sh` runs it on every release image
(CI builds no ESP image). `--all` lists the harmless hits too; since the
Ethernet driver it also finds `got 0x%02hhx` (W5500 version mismatch,
last argument, nothing shifts).

esp_http_server's `%zu` lines switch to `PRIu32` by themselves under
nano (`NEWLIB_NANO_COMPAT_FORMAT`, `esp_httpd_priv.h`) and are gone
from the list on the nano build. Four more hits are machine code that
happens to read `%jx`, `%qA`, `%qF`, `%zE`.

Our own C (`warpaxe_glue`, the vendored `esp_tinyusb`, TinyUSB's level-1
debug lines) uses no 64-bit, `z` or positional format; the core-dump
summary prints with `%lu`/`%08lx`, the last words and the stack-overflow
hook copy strings without printf. Rust's `log!` and `format!` do not go
through printf: esp-idf-svc's `EspLogger` writes the finished text to
newlib's `stdout`, and `wa_log_push` takes a finished line. No Rust code
calls `printf`, `snprintf` or `esp_log_write`. The one C-formatted text
our code reads back is the crash summary that `lastCrash` takes from the
log (`warpaxe_glue.c` `CRASH …`/`BT…` lines, `api.rs`): `%s`, `%lu` and
`%08lx` only, so it reads the same. No code and no WarpTune field parses
a line from the table above.

**Stacks.** Under nano ESP-IDF gives its tasks 512 bytes less stack
(`TASK_EXTRA_STACK_SIZE`, `esp_task.h`; `TASK_STACK_SIZE_ADD` in the
supplicant), since its printf needs 544 bytes less. Rust code runs on
three of those tasks (main, the event loop's handlers, the esp_timer
callbacks) and gains nothing from that, so `sdkconfig.defaults` gives
the 512 bytes back to the four that run here: main 16,000 → 16,512,
tcpip 6,144 → 6,656, event task 2,304 → 2,816, esp_timer 3,584 →
4,096. (Ping, WPS, DPP and the enterprise task are not in the ELF.)

The Wi-Fi blob takes the same 512 bytes from its own task: ESP-IDF
passes `wifi_init_config_t.nano_enable`, and the blob's
`config_get_wifi_task_stack_size` returns `0x1800` with WPA3 and adds
`0x200` only when that flag is 0; no other code in the blob reads it
(checked in the disassembly). No sdkconfig option sizes that task, and it
runs the supplicant's SAE math and every Wi-Fi log line through our
vprintf hook. The vendored esp-idf-svc therefore passes `nano_enable: 0`
(`vendor/esp-idf-svc/WARPAXE-PATCH.md`, 4.), and the Wi-Fi task keeps
its 6,656 bytes. With that every stack is as large as before, and no
internal RAM is added or freed.

`tools/image-breakdown.py` now counts `libc_nano.a` and `libm_nano.a`
as libc.

To check on devices: the list in "Still to check on devices" items 5–8
on a Bitaxe Gamma, a NerdQAxe++ and the devkit; in the log the
`pm stop` line after a Wi-Fi drop and no other stray `lu`/`zu`; the
clock and night mode in a time zone with summer time
(`CET-1CEST,M3.5.0,M10.5.0/3`, parsed by nano's `siscanf` now); a WPA3
join.

## Still to check on devices

Through dvb-WarpAxe #2, with the owner's OK:

1. **Splash on an LCD board of each wordmark**: Gaia, NerdOctaxe,
   NerdQAxe++ (and Rev 7), NerdAxe, NerdNOS, Q1370/Q1373, Naja Duo,
   Gamma Hex if at hand: the frame and the wordmark as before, centred,
   no torn or black frame, for the whole five seconds; with
   `flipscreen` and `invertscreen` set as well. The live picture on the
   web UI's display page shows the same frame.
2. **A board without a wordmark** (T-Display-S3 devkit): model name in
   place of the wordmark. **OLED boards** (Gamma, Gamma Turbo): the
   wordmark and the "logo" slide unchanged.
3. **First start after an erase** on an LCD board: the splash starts
   for the default board and switches to the detected board's wordmark.
4. **Web UI** in all eight languages over Wi-Fi, the USB network and
   the setup hotspot's captive portal: the page loads (it is the same
   gzip format, packed differently), favicon on `/favicon.ico` and
   `/assets/favicon.svg`.
5. **API documents**: `/api/system/info`, `/api/v1/state`, the
   self-test status, the diagnostics file, swarm, history and events
   compared key for key with a v0.1.44 device of the same board
   (`jq -S` on both; only live values differ). WarpTune reads the
   device as before.
6. **Heap and stacks** on the Gaia with TLS after an hour:
   `freeHeapInternal` and its largest piece against v0.1.44 (about
   30 KB free), `minerStackFree` (about 6 KB; `miner::run`'s frame is
   16 bytes larger).
7. **Log**: the same lines as v0.1.44 at boot and in an hour of mining
   (no debug line existed before either).
8. **Update**: an online or uploaded update from v0.1.44 to this image
   and back; settings survive.

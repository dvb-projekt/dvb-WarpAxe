# Internal RAM: where it goes and what was moved

Numbered 20 at the merge (docs/19 is the stuck-pool note). Written 2026-11-21 on branch `claude/internal-ram-c24e8f` (from main
`93f880b`, v0.1.47 + two commits), on a handover from dvb-WarpAxe #2
after the W5500 Ethernet HAT test on .41. The static numbers are measured on the build; the device test on .46
and .42 (2026-11-21) is at the end.

## Why

The ESP32-S3 has 512 KB of SRAM shared by IRAM (code) and DRAM (data,
heap, task stacks). PSRAM is plentiful but cannot be reached while the
cache is off (every flash write or erase), so task stacks that write
flash, DMA buffers and anything an IRAM interrupt touches must stay
internal.

On .41 (NerdQAxe++, 2026-11-21) v0.1.47 sits at 32–34 KB
`freeHeapInternal`. The Ethernet build with the HAT and a cable came
down to 10–21 KB free with a largest piece of ~7 KB after f58f70b
(eth thread on PSRAM, SPI master ISR in flash); before it 832 B, and
mDNS, DNS, I2C and httpd allocations failed.

Note on that number: `CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=32768` keeps
32 KB that a plain `malloc` never gets. A `freeHeapInternal` near 32 KB
means the rest of the internal heap is used up and plain mallocs are
already spilling into PSRAM; the reserve is what stacks, DMA, Wi-Fi and
mDNS draw on.

## How it is measured

```bash
cd firmware/warpaxe && cargo build --release
xtensa-esp32s3-elf-objdump -h target/xtensa-esp32s3-espidf/release/warpaxe
```

"Static internal" is `.dram0.heap_start − 0x3fc88000`: everything the
linker puts in internal SRAM before the heap begins (IRAM code and its
alignment, `.dram0.data`, `.dram0.bss`). One byte less there is one
byte more of internal heap at boot. Each option was built on its own,
one after the other, in the same target dir (a full ESP-IDF rebuild per
sdkconfig change, ~50 s each).

## What was built (commits on the branch, cheapest risk first)

| # | Commit | What | Static internal | Runtime internal | Risk |
|---|---|---|---|---|---|
| 1 | `bcbf6f4` | Log ring (`s_boot`, `s_lines`, 40+128 lines × 120 B) from `.bss` to one PSRAM block | **−20,160** | – | low |
| 2 | `05b276f` | `ESP_WIFI_IRAM_OPT=n`, `ESP_WIFI_RX_IRAM_OPT=n` | **−16,896** (7,168 + 9,728) | – | low |
| 3 | `85c3df8` | `FREERTOS_PLACE_FUNCTIONS_INTO_FLASH`, `RINGBUF_PLACE_FUNCTIONS_INTO_FLASH` | **−10,240** (8,960 + 1,280) | – | low |
| 4 | `f48d1cc` | Stratum V1 session thread (12 KB) and fallback probe (4 KB) via `diag::spawn_psram` | – | **−16 KB** of stacks for good | low |
| 5 | `e143b6d` | `MDNS_TASK_CREATE_FROM_SPIRAM`, `MDNS_MEMORY_ALLOC_SPIRAM`, our waiting thread on PSRAM | – | **−8 KB** of stacks + every mDNS record/packet | low |
| 6 | `4e793b5` | `HEAP_PLACE_FUNCTION_INTO_FLASH` | **−6,656** | – | low–medium |
| 7 | `e83a9ea` | `SPIRAM_ALLOW_BSS_SEG_EXTERNAL_MEMORY` (lwIP, net80211, pp `.bss` to PSRAM) | **−14,256** | – | medium |
| | | **Sum** | **−68,208** (172,440 → 104,232) | **~24 KB + mDNS** | |

Image: 2,815,328 → 2,814,720 bytes (−608): code moves from the IRAM
segment (95,376 → 61,484 bytes) to the flash code segment, which costs
nothing extra.

Segments, `esptool image_info` (main `93f880b` → branch `e83a9ea`):

| Segment | main | branch |
|---|---|---|
| DROM | 0xc66e4 | 0xc6734 |
| DRAM (data) | 0x0990c + 0x01ba8 | 0x098bc + 0x01c28 |
| IROM | 0x1c64c4 | 0x1ce64c |
| IRAM | 0x17490 | 0x0f02c |

### Why each is safe (the argument a device test has to confirm)

1. **Log ring.** Every writer runs on a task with the cache on: the
   vprintf hook and `wa_tu_printf` return early in interrupt context,
   the Rust logger is a task. The panic path writes "last words" to
   RTC memory, not to the ring. The block is taken in
   `wa_log_capture_start`, the second call in `main`, before any thread
   exists; without PSRAM the ring stays empty (`wa_log_count` 0).
2. **Wi-Fi IRAM options.** Espressif's trade: less throughput, same
   function. The Wi-Fi interrupt is in IRAM regardless. Stratum needs a
   few KB/s; an update download will take somewhat longer (the write,
   81 s on .41, dominates anyway, docs/09).
3. **FreeRTOS and ring buffer in flash.** Only the task-level API moves;
   the scheduler, the tick and every `…FromISR` stay in IRAM. Espressif
   (api-guides/performance/ram-usage): "safe to enable in all
   configurations" unless an ISR calls the task variants. Our IRAM code
   (`warpaxe_glue.c` last words and stack overflow hook, the devkit's
   hash loops) calls none.
4. **Stratum V1 on PSRAM.** Read through the call graph: `config::get`
   reads RAM, `events::record` only queues (the store, `events::flush`,
   runs on the board/miner/HTTP threads), a confirmed TLS key is written
   by the HTTP handler, mbedTLS allocates from PSRAM, the CA bundle is
   read through the cache, `board::set_no_pool` is an atomic. The sv2
   session has done the same work on a 32 KB PSRAM stack for months. If
   PSRAM has no room the threads start on internal stacks as before.
5. **mDNS.** The component allocates with `MALLOC_CAP_INTERNAL`
   explicitly unless told otherwise; it runs from lwIP's receive callback,
   its own task and an esp_timer that posts to its queue, never with the
   cache off, and writes no flash. A PSRAM task stack is allowed
   (`CONFIG_SPIRAM_ALLOW_STACK_EXTERNAL_MEMORY=y`).
6. **Heap in flash.** Espressif: safe unless an interrupt that runs with
   the cache off allocates, or `SPI_MASTER_ISR_IN_IRAM` is on. Main has
   no SPI master user (both LCD drivers are i80), so that ISR is never
   installed (turning the option off on main saves only 256 bytes); the
   Ethernet branch sets it off anyway (f58f70b).
7. **lwIP/Wi-Fi `.bss` in PSRAM.** ESP-IDF's own linker fragments move
   exactly these libraries; ESP-Miner and NerdOS both ship it (see
   below). The safety rests on Espressif's placement of the Wi-Fi blob's
   data, not on our code, which is why it is last and can be dropped
   alone.

## What was looked at and not built

| Candidate | Gain | Why not (yet) |
|---|---|---|
| `SPIRAM_TRY_ALLOCATE_WIFI_LWIP` | unknown, possibly **negative** | It forces static Wi-Fi TX buffers (`ESP_WIFI_DYNAMIC_TX_BUFFER` depends on it being off) and raises the static RX default from 10 to 16. TX buffers must be DMA memory, so 16 × 1.6 KB = 25.6 KB of internal RAM are taken at Wi-Fi start for good, plus 9.6 KB more RX unless pinned to 10. What moves to PSRAM is lwIP's pbufs and the blob's other mallocs. ESP-Miner and NerdOS set it, but they also have no RESERVE accounting like ours. Only a device A/B (`freeHeapInternal`, `largestFreeInternal`, with `ESP_WIFI_STATIC_TX_BUFFER_NUM` at 6–8 and `STATIC_RX` pinned to 10) can say whether it pays. |
| `SPIRAM_MALLOC_ALWAYSINTERNAL` 16384 → 4096 or lower (docs/09 already names it) | likely the largest lever (every small Rust allocation leaves the internal heap) | Not a config change only: init-time allocations that IRAM interrupts touch would move to PSRAM. Example found: `spi_master.c:484` allocates `spi_device_t` with plain `malloc`, and the SPI ISR (in IRAM on main) dereferences it with the cache off. Needs an audit of every plain `malloc` reached at init by an IRAM-safe driver, then a device A/B. |
| TinyUSB NCM buffers (`ncm_epbuf`, 19,216 B, now the largest object in internal RAM) | 6,400 B per buffer pair (3+3 → 2+2) | The USB network's DMA needs internal RAM; one buffer per direction lost ACKs under load (comment in `sdkconfig.defaults`). 2+2 needs a USB throughput test (web UI over USB, the web flasher's read). |
| Main task stack 16,512 → 12,288 | 4,224 B | The OTA writer used 7,416 B of 16,000 (docs/09, 2026-10-20), but main's init has grown since. Read the "bytes of the writer's stack never used" line after the next device update first. |
| The 3-minute `mark_app_valid` thread (3 KB, internal: it writes otadata) | 3 KB for the first 180 s | Could be folded into the main task's writer loop (a wait with timeout). Small; the holes form at boot, so maybe worth it later. |
| Board (10 KB), miner (12 KB), httpd (12 KB), Wi-Fi (8 KB) threads | 42 KB | All write NVS (events::flush, config::save, Wi-Fi's own NVS) and must stay internal. Moving them means handing their flash writes to a writer task, like the OTA writer of docs/09. |
| `ESP_EVENT_POST_FROM_IRAM_ISR=n`, `SPI_SLAVE_ISR_IN_IRAM=n` | 0 / 256 B | Nothing linked on main. |
| `SPI_FLASH_ROM_IMPL`, disabling unused flash-chip drivers, HAL asserts off | some KB of IRAM | Loses esp_flash bug fixes or chip support across a fleet of different flash chips (the OTA writer and NVS run on it). Not worth it. |

## The community (checked 2026-11-21, raw `sdkconfig.defaults` on master)

- **ESP-Miner** (bitaxeorg, ESP-IDF 6.0.3): `SPIRAM_TRY_ALLOCATE_WIFI_LWIP=y`,
  `SPIRAM_ALLOW_BSS_SEG_EXTERNAL_MEMORY=y` (#1280 "Misc optimizations to
  free more internal memory"), `MBEDTLS_EXTERNAL_MEM_ALLOC=y` (#1761
  "Free internal memory", which also moved several task stacks to PSRAM),
  main task 8192. No IRAM-to-flash option, no Wi-Fi buffer tuning.
- **NerdOS** (shufps/ESP-Miner-NerdQAxePlus, ESP-IDF 5.3):
  `SPIRAM_USE_CAPS_ALLOC=y` (plain `malloc` never goes to PSRAM at all),
  `SPIRAM_TRY_ALLOCATE_WIFI_LWIP=y` (PR #352 "let lwip use psram"),
  `SPIRAM_ALLOW_BSS_SEG_EXTERNAL_MEMORY=y` with the comment "saves about
  14kB internal RAM", `MBEDTLS_EXTERNAL_MEM_ALLOC=y`. No IRAM-to-flash
  option either.
- ForgeOS: no public sdkconfig found; GPTechinno: no ESP32-S3 firmware.

So commit 7 is common practice; commits 2, 3 and 6 are Espressif's
documented IRAM savings that neither firmware uses (both have far less
code in internal RAM pressure than a Rust image with TLS, Sv2, USB NCM
and a web UI).

## Device test (via Warppool #2, after Oliver's OK)

On a NerdQAxe++ (.41, the Ethernet board) and one Bitaxe, clean builds
of this branch only, never .43, not .40/.47:

1. Boot: `/api/system/info` `freeHeapInternal` and `largestFreeInternal`
   at 1, 10 and 60 minutes, against the same board on v0.1.47. Up to
   ~90 KB more internal heap exist now, but `freeHeapInternal` will not
   rise by all of it: plain mallocs below 16 KB prefer internal RAM
   (`ALWAYSINTERNAL`), and those that spill into PSRAM today because the
   internal heap is full will take part of the new room. What the test
   must show is the largest piece and the free total staying well above
   the 32 KB reserve, with and without the HAT.
2. `/api/v1/log` and the display's log page: boot lines and ring as
   before (commit 1).
3. Mining an hour on V1 with a fallback pool configured; pull the main
   pool to make the probe run (commit 4); the share rate as before.
4. `warpaxe.local` resolves, the swarm page finds the miner (commit 5).
5. Online update and an upload update under that heap; config saves
   (pool page, fan) while the pool runs (commits 3, 6, 7: flash writes
   with Wi-Fi traffic), once with Wi-Fi modem sleep on (the default off
   the devkit): the Wi-Fi blob's power-save state (`g_pm`) now sits in
   PSRAM, and only the blob knows whether its interrupt reads it while
   a flash write has the cache off. The writer's "stack never used" line answers the
   main-task question above.
6. The Ethernet build on top of this branch on .41 with HAT and cable:
   the same numbers as the 2026-11-21 test.

If one of commits 6 or 7 misbehaves, drop it alone; the others do not
depend on it.

## Audit (second agent, read-only, 2026-11-21)

No critical, high or medium finding in the seven commits. Clean by
reading: the log ring (no access before the block exists, ISR callers
skipped, the ring was never in a core dump), the IRAM options (the
project installs no IRAM interrupt; Kconfig itself turns
`SPI_MASTER_ISR_IN_IRAM` off with the heap in flash), the Stratum
call graph on PSRAM (nothing reaches `nvs_*`, `esp_partition_*`,
`esp_ota_*`; ESP-IDF asserts `esp_task_stack_is_sane_cache_disabled()`
on any flash operation from a PSRAM stack, reads included), mDNS (the
TCB stays internal, the stack check accepts PSRAM with
`SPIRAM_ALLOW_STACK_EXTERNAL_MEMORY`).

- **Low, open:** commit 7's residual risk, the Wi-Fi interrupt and the
  blob's sleep state in PSRAM (device test point 5).
- **Fixed** (`e519c1d`): comment nits (wpa_supplicant's `.bss` moves
  too; the SPI caveat is enforced by Kconfig; the hash loops' files).
- **Fixed** (`b07e43d`): `spawn_psram` now gives the FreeRTOS task the
  thread's name, so a panic says "stratum" rather than "pthread".

IRAM figures here are `.iram0.vectors + .iram0.text`; the segment in the
image also holds `.iram0.text_end` padding.

## Device test (.46 and .42, 2026-11-21)

Oliver's OK in this session for .46 and .42; Warppool #2 paused and
logged; images by OTA upload of the clean build `5a052c952`. Heap log
once a minute (`freeHeapInternal` / `largestFreeInternal`, samples
after 120 s up):

| Miner | Firmware | Window (UTC) | free min / mean / max | largest piece | Shares |
|---|---|---|---|---|---|
| .46 NerdNOS (modem sleep on) | v0.1.47 | 09:45–10:21 | 21,819 / 32,162 / 38,011 | 18,432 | rej 0 |
| .46 | 5a052c952 | 10:22–11:10 | 88,703 / 117,782 / 122,519 | 31,744 | 83 acc, 0 rej |
| .42 Gamma | v0.1.47 | 09:45–10:35 | 33,187 / 43,091 / 45,683 | 25,600–26,624 | rej 0 |
| .42 | 5a052c952 | 10:36–11:10 | 96,871 / 123,646 / 128,307 | 31,744 | 150 acc, 0 rej |

At the end of boot (`heap:` log line): .46 157 KB free, largest piece
64 KB; .42 163 KB, 72 KB.

- **Free internal heap about +80 KB** in the mean on both boards, the
  minimum +55–65 KB. The largest piece rises 18 → 31 KB on .46 and
  26 → 31 KB on .42 and then stays put, like docs/09's 20 KB on
  v0.1.31: the holes are still cut at boot, only the room around them
  is larger. The 32 KB piece now fits a 16 KB stack twice.
- **Modem sleep (.46):** 12 config saves (`displayStartPage` back and
  forth, NVS writes) with Wi-Fi traffic, and an upload update of the
  same image written by the new writer while `/api/system/info` was
  polled every second (50 of 50 answered before the restart): written,
  restarted "no crash reset", marked valid at chain ready. No reset in
  48 minutes.
- **mDNS:** "mDNS task will be created from SPIRAM", `nerdnos.local`
  resolves (IPv4 and IPv6). The log page shows the boot lines and the
  ring.
- Both pools are Sv2 here, so the V1 *session* code did not run; the
  stratum thread that hosts the sv2 session did (on PSRAM). The
  fallback probe did not run (no fallback switch). The writer's
  "stack never used" line was lost with the ring at the restart; read
  it after the next update.

### Second round (same day): the rest of the fleet, V1, Ethernet, the click update

Oliver's OK in this session for .44, .45 and .41 and for the
`read_body` fix (`a5c4e96`: the upload body in one allocation of its
Content-Length, `try_reserve` instead of an abort; the doubling Vec
had crashed an upload on .41 on 2026-11-20). Images `a5c4e968d`
(branch) and `e8216e7f1` (test merge of this branch into
`test/ethernet-on-main` 05c709c, branch
`claude/internal-ram-eth-test-c24e8f`, for .41 only; the one conflict
was `net/mdns.rs`: `lan_ip()` and `spawn_psram` together).

| Miner | Firmware | Window (UTC) | free min / mean / max | largest piece | Shares |
|---|---|---|---|---|---|
| .44 Nano | v0.1.47 | 11:34–12:06 | 36,999 / 40,550 / 41,639 | 20,480 | rej 0 |
| .44 | a5c4e968d | 12:08–12:43 | 112,603 / 122,444 / 130,295 | 31,744 | 132 acc, 0 rej |
| .45 Gaia | v0.1.47 | 11:34–12:08 | 19,295 / 24,367 / 28,183 | 18,432 | rej 0 |
| .45 | a5c4e968d | 12:09–12:43 | 93,251 / 105,904 / 108,599 | 31,744 | 136 acc, 0 rej |
| .41 NerdQAxe++, HAT, no cable | v0.1.47 (does not drive the HAT) | 11:34–12:12 | 11,803 / 22,908 / 26,243 | 4,608–7,680 | rej 0 |
| .41, HAT **and cable** | e8216e7f1 (Ethernet + this branch) | 12:13–12:43 | 85,719 / 99,375 / 104,223 | 31,744 | 149 acc, 0 rej |

For .41 the reference of the Ethernet test that morning (05c709c,
Ethernet + f58f70b only): with cable 10–21 KB free, largest piece
~7 KB. With this branch on top: ~99 KB free, 31 KB in one piece.
Link 100 Mbit/s full duplex, the default route over the cable, mDNS
`nerdqaxe.local` on it.

- **V1 and the fallback probe (.46):** main pool port set to a closed
  one; after three failed connects the V1 fallback (192.168.178.3:3333)
  took over, subscribed, authorized, 12 minutes, 23 shares, 0 rejected,
  the probe running every 300 s on its PSRAM stack; port set back, Sv2
  again.
- **The `read_body` fix (.46):** an upload received by the fixed code
  (`ota: … handed to the writer (129 KB internal free, largest piece
  31 KB)`), written, restarted, marked valid.
- **Click update (.41):** e8216e7f1 installed the published v0.1.47 by
  itself over the cable: download, signature verified, write, restart
  "no crash reset", marked valid at chain ready. That was also .41's
  way back; it runs v0.1.47 again (39 s up: 24 KB free, 19 KB largest).
- Still not seen: the writer's "stack never used" line (lost with the
  RAM log ring at every restart; it needs the USB console or a log
  that survives).

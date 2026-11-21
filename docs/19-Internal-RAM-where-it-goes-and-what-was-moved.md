# Internal RAM: where it goes and what was moved

Written 2026-11-21 on branch `claude/internal-ram-c24e8f` (from main
`93f880b`, v0.1.47 + two commits), on a handover from dvb-WarpAxe #2
after the W5500 Ethernet HAT test on .41. **Draft: nothing here ran on
a device.** The static numbers are measured on the build; the runtime
gains are argued and wait for the device test at the end.

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
   with Wi-Fi traffic). The writer's "stack never used" line answers the
   main-task question above.
6. The Ethernet build on top of this branch on .41 with HAT and cable:
   the same numbers as the 2026-11-21 test.

If one of commits 6 or 7 misbehaves, drop it alone; the others do not
depend on it.

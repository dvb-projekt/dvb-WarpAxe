# A panic without a core dump in the restart after an update

2026-10-23. A user's Bitaxe Gamma Turbo came back from the restart that
ends an online update with reset reason `panic` and no core dump. This
note records what the evidence settles, what it does not, and what the
firmware does about it from this branch on. Branch `fix/update-restart-panic`
on main `bad4a85` (v0.1.36).

## The case

John11, Bitaxe Gamma Turbo "gt2" (two BM1370, EMC2103, TPS546, 850 MHz /
1221 mV, 64.5 W, Stratum V2 to a local pool), updated from **v0.1.34** to
v0.1.36 with the Update button on 2026-10-23. From his diagnostics file
(generated 13:47:16Z, private; the excerpt is in the project's CloudDocs
folder `Update-Neustart-Panic-2026-10-23`):

- The old firmware wrote and verified the image: the install record reads
  `v0.1.36 verified`, which is stored only after `flash_image` succeeded.
- The miner then came up with reset reason **panic** instead of `software`.
- **No core dump**: `coreDumpBytes` null, so none in flash at all, not an old
  one.
- No `update installed` event for 0.1.36.
- The boot event's time (13:06:34Z) is 8.6 minutes before the boot computed
  from the uptime (13:15:10Z).
- v0.1.36 ran healthy afterwards (3.53 TH/s, 0.24 % errors, 174/0 shares).

Our own miners: the updates 0.1.35 → 0.1.36 on 2026-10-23 (.40, .41, .42,
.44, .45, .46, .111) and 0.1.34 → 0.1.35 on 2026-10-22 all restarted with
`software`.

**The Gamma .42's `panic` of 2026-10-21 08:12:29Z is not a second case.**
The event log around it: boot `software` 08:09:39Z, `panic` 08:12:29Z,
boot `software` 08:20:15Z. That was the first device run of the self-test
package: dvb-WarpAxe #2 flashed the test image at 08:09, `POST
/api/v1/selftest` overflowed the self-test thread's 8 KB PSRAM stack 170 s
later, and the fix (a boxed window, 16 KB) was flashed at 08:20 (docs/10,
"The first device run"). The `v0.1.33 verified` entries next to both boots
are the stored install record, which the boot event repeats; they do not
mean an update happened then.

## What the evidence settles

### 1. The missing "update installed" event is the bootloader's, not the panic's

The firmware records that event at boot only when the bootloader reports the
image as `pending-verify` (`main.rs`, `update::ota_state`). John's boot log
says `update: image state at boot: new`: his bootloader does not touch the
state, so it predates rollback support (`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`,
a51ce13, v0.1.22; the bootloader is only ever written by a USB flash). Our
miners show the event (`install_rec v0.1.36 verified`) because their
bootloaders are current. From this branch the event no longer depends on the
bootloader (see the marker below).

### 2. The 8.6 minutes are RTC drift, not a hang

The system time survives a reset in the RTC slow clock, the internal RC
oscillator here (`CONFIG_RTC_CLK_SRC_INT_RC`), while the running system keeps
time with the crystal-driven high-resolution timer. `esp_restart` runs
`esp_sync_timekeeping_timers` (newlib `esp_time_impl.c`) as a shutdown
handler, which writes the accurate time back into the RTC before the reset.
A panic reset skips the shutdown handlers, so the next boot starts from the
RTC's own count, with the RC oscillator's drift since the last sync: 8.6 min
over the 46.6 h since his previous boot is 0.31 %, well inside what the RC
oscillator does over a day of temperature changes. Our seven miners, all
after a software reset on 2026-10-23, show the boot event's time equal to
the NTP-derived boot time within a second:

| Miner | Boot event `t` | `unixTime − uptime` | Difference |
|---|---|---|---|
| .40 Gaia | 1792758580 | 1792758581 | −1 s |
| .41 NerdQAxe++ | 1792759058 | 1792759058 | 0 s |
| .42 Gamma | 1792758798 | 1792758798 | 0 s |
| .44 Nano | 1792757365 | 1792757366 | −1 s |
| .45 Octaxe | 1792758093 | 1792758093 | 0 s |
| .46 NerdNOS | 1792758345 | 1792758346 | −1 s |
| .111 devkit | 1792759033 | 1792759033 | 0 s |

### 3. Why no core dump: the dumps are as large as the partition

The four core dumps stored on our miners (`GET /api/v1/coredump`, read on
2026-10-23) against the `coredump` partition of 65,536 bytes:

| Miner | Dump | Tasks | Stacks | Register notes | TCBs + headers |
|---|---|---|---|---|---|
| .41 NerdQAxe++ | 64,004 B | 18 | 45,264 B | 11,324 B | ~7.4 KB |
| .44 Nano | 60,644 B | 20 | 39,856 B | 12,540 B | ~8.2 KB |
| .46 NerdNOS | 64,292 B | 21 | 42,512 B | 13,148 B | ~8.6 KB |
| .111 devkit | 63,012 B | 24 | 38,192 B | 14,972 B | ~9.8 KB |

A dump is an ELF with every task's used stack (the largest single stack
seen: 11.7 KB), a register note per task, and the TCBs. ESP-IDF v5.3.3
computes the size first and, when it does not fit, writes nothing at all
(`espcoredump/src/core_dump_flash.c`, `esp_core_dump_flash_write_prepare`:
"Not enough space to save core dump!", `ESP_ERR_NO_MEM`). With dumps of
60.6–64.3 KB in a 64 KB partition, every crash is a coin toss: one more
task, or a deeper stack in one of them, and nothing is stored. That is the
state of every miner whose partition table was flashed up to v0.1.36, John's
included, and it fits all three crashes without a dump we have seen (the
.42's self-test overflow of 2026-10-21, the .44's `int-wdt` of the same
day, John's restart).

The other ways a dump is lost were checked in ESP-IDF v5.3.3 and do not
apply here or cannot be told apart without a console: an exception inside
the dump writer ("Re-entered core dump", for example after a stack overflow
corrupted a heap block or a TCB the writer walks); a panic while the flash
cache is off is handled (`panic_enable_cache`); `CONFIG_ESP_COREDUMP_FLASH_NO_OVERWRITE`
is off, so an old dump never blocks a new one; a dump damaged in flash fails
the CRC check at the next boot and reads as none (that check logs through
`esp_rom_printf`, which the firmware's log ring does not see).

docs/10 said the writer "cannot read a PSRAM stack with the cache off" for
the .42's self-test crash. That is wrong: the writer copies each stack
through an internal buffer with the cache on and only switches it off for
the raw flash write, and our dumps carry PSRAM segments (6.5 KB of them on
the .41). docs/10 now points here.

### 4. A panic prints to nobody after the first 30 s

The console is USB-Serial/JTAG (`CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG`). After
the boot's USB step the firmware hands the one USB PHY to USB OTG (TinyUSB:
NCM network and the CDC-ACM console, which needs the running system to
send). The panic handler writes to the USB-Serial/JTAG FIFO with interrupts
off (`panic.c`, `panic_print_char_usb_serial_jtag`, 50 ms until it gives up
on the first character and stops waiting): with the PHY on OTG nobody
drains that FIFO, and the panic text is lost. A cable on the port shows the
log up to the restart on the CDC console and then nothing of the crash. A
trace of a crash in this phase can only come from what the firmware stores.

## The restart path, read for panic sources

v0.1.34 and v0.1.35 are what users still update from; `update.rs` and
`diag.rs` are the same in all three versions, `board.rs` differs in the
thread's stack. The path, on the main task (`update::serve_writes`):

1. `flash_image` ok → log "written and verified, restarting".
2. `drop(image)` (a 2.9 MB vector in PSRAM).
3. `config::set_last_install(record)`: one NVS string write; its error is
   logged.
4. 500 ms.
5. `board::prepare_reset("update")`: sets `FANS_FULL` and `SHUTDOWN`; waits
   up to 2 s for the board thread to report the rail off, plus two sample
   periods; then the chain's reset line low, `asic_power(false)`,
   `rail_off` (the TPS546's OPERATION off over the shared I²C mutex, errors
   logged), `LDO_OFF_BEFORE_RESET_MS`.
   On the board thread meanwhile, the halt branch of its loop: `vr.off()`,
   `asic_power(false)`, `hold_reset_without_regulator`, `pf.disabled()`,
   `POWERED` false, one warning line; the fan output goes to full on the
   next pass.
6. `diag::restart()` → `esp_restart()`: the shutdown handlers in reverse
   order of registration — `esp_wifi_stop` (the Wi-Fi thread's loop then
   sees the link lost and logs; it has no `unwrap` on that path),
   `esp_sync_timekeeping_timers` — then `vTaskSuspendAll` and
   `esp_restart_noos` (interrupts off, the other core reset and stalled,
   both caches off, the reset).

Findings:

- No `unwrap`, `expect`, indexing or arithmetic that can panic on the main
  task's path. The mutex locks there cannot be poisoned: the firmware is
  built with `panic_abort`, so no thread ever unwinds while holding one.
- The I²C bus is one mutex shared by the main task's `rail_off` and the
  board thread; the NVS write happens on the main task, whose stack is
  internal; a flash operation stalls the other core and disables the
  non-IRAM interrupts for its duration, as it does for every config save.
- **The board thread's stack is the one thin margin.** v0.1.34 gave it
  6,144 bytes. The v0.1.35 candidate overflowed those on a NerdQAxe++ a
  minute after boot once the event details grew, which is why v0.1.35 has
  10 KB and a low-water log. John's own boot log of v0.1.36 shows that
  thread with 4,348 bytes never used of 10,240 at its first sample, i.e.
  5.9 KB in use on a Gamma Turbo (EMC2103, two fan outputs) before any
  event. v0.1.36's loop carries more locals than v0.1.34's did, so the
  v0.1.34 figure is lower, but a 6 KB stack with a few hundred bytes of
  margin overflows the moment an interrupt frame lands at the deepest point
  of a pass. The canary check at the next context switch then calls
  `esp_system_abort`, which is a panic: reset reason `panic`, and a dump
  only if it fits.
- The second-smallest stack on the path is the system event task
  (`CONFIG_ESP_SYSTEM_EVENT_TASK_STACK_SIZE` 2,304 bytes), which runs
  esp-idf-svc's Wi-Fi and IP event closures when `esp_wifi_stop` posts
  `STA_STOP`, `STA_DISCONNECTED` and `STA_LOST_IP`. The same closures run on
  every disconnect, which John's miner had four of after the boot without
  harm, so this is the less likely of the two.
- `esp_restart_noos` with `CONFIG_SPIRAM_ALLOW_STACK_EXTERNAL_MEMORY` moves a
  PSRAM stack pointer to internal memory before it switches the caches off;
  the main task's stack is internal anyway.

So the panic's origin is not provable from the code. The leading hypothesis
is a stack overflow of the 6 KB board thread of v0.1.34 on the Gamma Turbo
during the halt branch; v0.1.35 and v0.1.36 already carry the 10 KB stack
and its low-water log. The "no core dump" part is settled by the partition
size, which this branch changes. Both parts leave a trace from this branch
on, whatever the cause turns out to be.

## What this branch changes

1. **The `coredump` partition grows from 64 KB to 256 KB**
   (`partitions.csv`: 0x620000, 0x40000; the flash has 9.6 MiB free above
   its new end, and no other partition moves). The table is written by a USB flash
   only (the web flasher's `partition-table.bin`, `warpaxe-factory.bin`);
   an online update keeps the table in flash, so miners updated online
   keep 64 KB until they are flashed over USB once. The firmware logs the
   partition's size at boot and warns below 128 KB; `coreDumpPartition`
   is in `/api/system/info` and the diagnostics file.
   Note for the cut: `sdkconfig.defaults` names the table by an absolute
   path into the main checkout (`CONFIG_PARTITION_TABLE_CUSTOM_FILENAME`),
   so a build in a worktree still uses main's table; the release built from
   main after the merge gets the new one. Check the release's
   `partition-table.bin` with `gen_esp32part.py`: `coredump … 256K`.
2. **The restart marker** (`warpaxe_diag::restart`, NVS key `restart`,
   `why|step|from|uptime`). `update::restart_now` stores it before the
   chips go off and again, at its second step, right before the reset, for
   every intentional restart (`update`, `ota` for an upload through the web
   UI, `restart`, `download mode`; the word is the writer's origin and goes
   into the event untranslated). The next boot takes it first thing after
   the config is read, so a crash later in that boot is never blamed on the
   restart. After a crash reset (panic, interrupt or task watchdog, or the
   RTC watchdog) the event log gets `boot: <image>
   crashed while restarting (<why>), switching the chips off` or `… in the
   system reset` (codes `rs_crash_chips`, `rs_crash_reset`, in all eight UI
   languages), and the log says the same with the uptime. An update or
   upload records its `installed` event on any bootloader, after the plain
   software reset and after a crash in the old image's restart alike (the
   written image boots either way), not when the bootloader reports a
   rollback and not when the image that began the restart is the one that
   came back; the bootloader's `pending-verify` path records nothing twice.
   Two limits: a power cycle that ends a hang in the restart reads as a
   restart without a crash; and a marker left behind by a downgrade to an
   image without this code is read by the next image that has it, about a
   restart from back then. After the way into download mode the RTC
   watchdog is not a crash of that restart: it ends in the ROM, and the
   watchdog is how the web flasher, espflash and `esptool -a
   watchdog-reset` leave download mode (esptool's USB reset and a power
   cycle are clean reasons anyway). A panic is a crash of that restart
   all the same, and it was one: see *The way into download mode crashed
   in the torn-down console* below (v0.1.37 read it as "no crash reset";
   from v0.1.38 it is the `rs_crash_reset` event like any other).
3. **Last words** in RTC slow memory (`warpaxe_glue.c`: the last 256 bytes
   of it, at a fixed address rather than a linker-placed variable, because
   the image that crashes in the restart after an update is not the image
   that reads the words; a magic and a checksum tell them from the noise
   after a power-on, and the boot checks that the image's own RTC data
   stays below the block). The Rust panic hook stores the FreeRTOS task's
   name, file:line and the message before the default hook prints and the
   firmware aborts, in a frame of its own so the smallest stacks are not
   pushed over; the FreeRTOS stack overflow hook
   (`vApplicationStackOverflowHook`, weak in the port, overridden here, in
   IRAM like the port's) stores the task's name before it aborts as the
   port's own did. The next boot after a crash logs them (`last words of
   the crashed run: …`) and shows them, scrubbed like the log, as
   `lastWords` in `/api/system/info` and the diagnostics file. Hardware
   exceptions (a load from a bad address, say) leave no last words; for
   those there is the core dump, with room now. Two panics at once on both
   cores would interleave their words.
4. The restart path keeps its order (the install record, the chips off,
   the reset); `restart_now` is its one place, and it adds the marker's two
   NVS writes before and after the chips-off, each behind the NVS mutex
   the config saves and the event log use, a few milliseconds each. The
   log line "chips off before the reset" now names the writer's origin
   (`ota` for an upload, where it said `update`).

Not done: the dump itself cannot be made smaller from the config. The ELF
writer dumps every task (`CONFIG_ESP_COREDUMP_MAX_TASKS_NUM` is not used by
it), and the binary format would drop the register notes but also
`esp_core_dump_get_summary`, which the boot log and the crash banner use.

## Reproduced: the interrupt watchdog in a flash erase, and why it leaves no dump

2026-10-23, Gamma .42, candidate 8800aed up for 39 minutes, an image
uploaded through the web UI (`soak-test.py --upload`): the next boot said
`reset reason int-wdt`, the marker `restart (ota) begun by warpaxe 0.1.37
at uptime 2363 s: crashed in the system reset`, the event `crashed while
restarting (ota), in the system reset`, then `installed`; no core dump
(`coreDumpBytes` null) although the partition was 256 KB, no last words.
Uploads earlier that day, minutes after a boot, had restarted cleanly.

What the reset reason says, from ESP-IDF v5.3.3 (`esp_system/int_wdt.c`,
`spi_flash/cache_utils.c`, `spi_flash/spi_flash_chip_generic.c`):

- `int-wdt` is MWDT1's first stage (`CONFIG_ESP_INT_WDT_TIMEOUT_MS`,
  300 ms). With `CONFIG_ESP_INT_WDT_CHECK_CPU1` it is fed from CPU0's tick
  only after CPU1's tick has run as well.
- Every flash write or erase stalls CPU1 (`esp_ipc_isr_stall_other_cpu`)
  and holds CPU0's non-IRAM interrupts for the operation's length.
  Nothing feeds the watchdog meanwhile: a single flash operation longer
  than 300 ms is an `int-wdt` reset, on any dual-core ESP32-S3 with this
  option. ESP-IDF's own sector-erase timeout is 600 ms (GD25Q127 at
  125 °C plus 100 ms; typical is 45 ms, longer when the chip is warm
  and the sector worn), and the 64 KB block erase the update's slot is
  cleared with may take 4.1 s by the same table.
- The "system reset" step of the marker holds exactly one such
  operation: the marker's second NVS write, which erases a page when
  the one in use is full. `esp_restart` itself runs two shutdown
  handlers (`esp_wifi_stop`, `esp_sync_timekeeping_timers`; the only
  ones registered, `esp_register_shutdown_handler` callers in v5.3.3)
  and disables MWDT1 before it switches interrupts off, so it cannot be
  the one. The 39 minutes fit: a warm chip erases slower, and a page
  fills over time.
- No dump, with 256 KB to spare: the panic lands while the chip is in
  the middle of that erase. The dump's first flash operation waits for
  the chip to go idle only `SPI_FLASH_DEFAULT_IDLE_TIMEOUT_MS` (200 ms;
  `set_write_protect` and `erase_sector` both wait that long), then
  gives up with `ESP_ERR_TIMEOUT`: "Failed to erase flash", nothing
  stored. An `int-wdt` inside a flash operation therefore never leaves
  a dump, whatever the partition's size. The Nano .44's `int-wdt` of
  2026-10-21 without a dump, under WarpTune's PATCHes (each an NVS
  write), reads the same way.

Fix: `CONFIG_ESP_INT_WDT_TIMEOUT_MS` 300 → 1000 ms (`sdkconfig.defaults`;
an app setting, it reaches every miner with the update). That covers
every sector erase the chip is specified for and the usual block erase
of the update's slot, and a real interrupt hang still resets within a
second (the second stage resets at two). The restart path's NVS writes
log their duration when they took 100 ms or more, so a slow erase shows
up in the log before it bites. Left as they are: the block erase of the
update's slot (`esp_ota_begin` with `OTA_SIZE_UNKNOWN` erases the whole
slot first, 48 blocks of 64 KB, 150 ms typical each); the ESP-IDF
option `CONFIG_SPI_FLASH_BYPASS_BLOCK_ERASE` would make that 768 sector
erases, some 30 s longer per update, for a worst case that has not been
seen. Oliver decides whether that is wanted.

Reproduction plan, with Oliver's OK: on the .42 at an hour's uptime,
upload the build with the 1000 ms watchdog through the web UI several
times (each at a long uptime, so the chip is warm and NVS has filled),
and look for `config: storing the restart marker took N ms` lines and
for `int-wdt`. The way to force a long erase is many events or config
patches before the upload, which fill the NVS page the marker lands in.

## The way into download mode crashed in the torn-down console

Found by dvb-WarpAxe #2 on 2026-10-24 while giving the fleet the 256 KB
table over USB: after `POST /api/v1/bootloader` the .41 (NerdQaxe++) and
the .45 (NerdOctaxe Gamma), both on v0.1.37 `3e0188b`, sat in ROM download
mode with `RTC_CNTL_STORE6_REG` = `0x80040004`, the ESP-IDF panic hint
(`ESP_RST_PANIC`), on every entry (the .41 twice in a row, with an app
boot between that cleared the hint). No last words, so not a Rust panic;
no dump, because both still had the 64 KB partition. After the esptool
session the app came up with reset reason `panic` and the crash banner.

**The chain**, read from the code (all deterministic, no device needed):

1. When USB OTG comes up, `main` moves the console into the CDC port
   (`wa_usb_console_to_cdc` → `esp_tusb_init_console`: `freopen` of
   stdin, stdout and stderr onto `/dev/tusb_cdc`). From then on every
   `ESP_LOG` line and every Rust log line is a `write(2)` into
   esp_tinyusb's VFS (`tusb_write`): `tud_cdc_n_write_char` into the
   stream FIFO, then `tud_cdc_n_write_flush`.
2. `wa_reboot_to_bootloader` (the download-mode path and the `B` on the
   console port) called `tinyusb_driver_uninstall` first thing. That is
   `tusb_stop_task` (the TinyUSB task deleted), `tusb_teardown` =
   `tud_deinit` (the usbd mutex `_usbd_mutex` deleted and set to NULL,
   the CDC streams' FIFO mutexes deleted, `_usbd_dev` cleared, the
   interrupt freed), then `usb_del_phy`. Nothing moved the console back.
3. The next log line, from any task, goes `tusb_write` →
   `tud_cdc_n_write_flush` → `tu_edpt_stream_write_xfer` → `stream_claim`
   → `usbd_edpt_claim` → `tu_edpt_claim(ep_state, _usbd_mutex)` →
   `osal_mutex_lock(NULL)` → `xSemaphoreTake(NULL)` → FreeRTOS's
   `configASSERT(pxQueue)` → `abort()`: a panic, with the hint, without
   last words (C, not Rust). There is always such a line: `esp_restart`'s
   shutdown handler `esp_wifi_stop` alone prints half a dozen
   (`wifi:state: run -> init`, `wifi:flush txq` …), and during the 50 ms
   wait before it any task may log. The FIFO write before the flush goes
   through the deleted FIFO mutex too (freed memory, the handle not
   cleared), which with heap poisoning off happens to work.
4. The restart marker was already at its second step ("in the system
   reset"), so the next boot logged `restart (download mode) begun by
   warpaxe 0.1.37 at uptime 89 s: no crash reset` — the rule from
   v0.1.37 that read a download-mode marker as never a crash, written
   for exactly this symptom before its cause was known.

The update and restart paths never uninstall TinyUSB, which is why they
end in `software` and only this path panicked. The chain is the same on
every board; it needs a host on the USB port for nothing (the CDC port
does not have to be open).

**The fix** (`warpaxe_glue.c`): before TinyUSB goes down, the three
standard streams go back to `/dev/console` (the USB-Serial/JTAG device
the boot started on); the `FILE` lock lets a line in flight finish on the
CDC port first. lwIP's sends (`wa_usb_send`) are refused from that moment
and one in flight gets 20 ms, since `tinyusb_net_send_async` takes the
same mutex. `tinyusb_driver_uninstall`'s result is logged, not ignored.
The lines from the shutdown handlers now go to the USB-Serial/JTAG
device, whose VFS drops bytes rather than block while no host reads
(`TX_FLUSH_TIMEOUT_US`, 50 ms); since the host is still re-enumerating
the port when the reset comes 50 ms later, expect them, and the panic
text of any later crash on this path, to be lost on the wire most of the
time. The restart marker and the panic hint are the trace that stays.

**The marker rule** (`warpaxe_diag::restart::outcome`): after a
download-mode marker the RTC watchdog and the clean reasons complete the
restart, a panic or a task or interrupt watchdog crashes it. The hint in
`RTC_CNTL_STORE6_REG` is written by the panic handler alone and cleared
by nothing but an app boot, so with a download-mode marker it can only
come from the marker's own run: the way into the ROM crashed, after the
marker was written. The .41 and .45 would have reported `crashed while
restarting (download mode), in the system reset`. The banner was right as
it was: `crashed` for a panic stays, whatever marker is open, and dvb-WarpAxe
#2's wish that a panic ending at the download-mode entry should not show
it is met at the root, by not panicking there. A real crash on that path
must show.

**The reset cause next to the reason**: the app now logs
`esp_rom_get_reset_reason(0)` with the boot line (`reset reason panic (rtc
cause 0x03)`) and reports it as `resetCause` in `/api/system/info` and
the diagnostics file. It would have told the following apart at once. Both
miners came up as `panic` after the esptool session, i.e. a software
reset with the old hint, although the session was meant to end in
esptool's RTS reset (`CORE_USB_UART`, read as `usb`, the hint ignored —
what `tools/flash-backup.sh` ends with). The sequence was separate
esptool calls with `--before no_reset --after no_reset`, the last of them
`write_mem 0x6000812c 0 0x1` (clearing `FORCE_DOWNLOAD_BOOT`), then
`--after hard_reset read_mac` for the reset. esptool's `--after no_reset`
with the stub running is `soft_reset(True)`: `flash_finish(reboot)`, the
stub's software reset "back to the ROM loader" — which works while the
register still forces download mode, and boots the app the moment it
does not. So `write_mem … --after no_reset` itself booted the app with
`CORE_SW` and the hint, and the `read_mac` meant to reset found no
loader (exit 1). esptool 4.9's `hard_reset` for the ESP32-S3 clears
`RTC_CNTL_OPTION1_REG` itself before pulling RTS: end a session with
`--after hard_reset` and leave the `write_mem` out, and the app comes up
as `usb`.

**Reproduction** (needs Oliver's OK, a device with a 256 KB partition):
`POST /api/v1/bootloader` on v0.1.37, leave download mode with the RTS
reset, read `/api/system/info`: `resetReason` `panic`, a current
`coreDumpBytes`, and `espcoredump.py info_corefile` on it shows
`xQueueSemaphoreTake` ← `tu_edpt_claim` ← `usbd_edpt_claim` ←
`tu_edpt_stream_write_xfer` ← `tud_cdc_n_write_flush` ← `tusb_write`
under a `vprintf`. Then the same on v0.1.38: `software` or `usb`, the
boot event `restart (download mode) … no crash reset`, no new dump, and a
monitor on the USB-Serial/JTAG port shows `download mode: console back
on USB-Serial/JTAG` followed by the Wi-Fi stop lines.

## The Sv2 certificate against a clock NTP has not confirmed

A side finding of the same USB session (dvb-WarpAxe #2, the .45): the
app came back with a clock that read as set (past 2024, so
`clock::unix_now` is `Some`) but off far enough to fail the certificate
window of both Sv2 pools (`InvalidCertificate` at 46 s and 52 s against
the Umbrel pool, at 64 s against pool.warppool.org), three failures,
fallback at 64 s, verified at 89 s after the first NTP sync. The time
comes out of RTC memory across a reset — right after a software reset,
not to be trusted after minutes in the ROM and a tool's resets.

From v0.1.38 the Sv2 connect waits (the same bounded 20 s as for no
clock at all) while NTP is configured and has not set the clock this
boot (`clock::ntp_pending`), says so if it goes ahead anyway, and a
certificate that fails against such a clock in the boot's first five
minutes is not counted towards the fallback (`CLOCK_UNSURE`, taken by
`stratum::run`). The crate's error now tells a certificate failure from
the rest of the handshake and names the window and the clock
(`warpaxe_sv2::Error::Certificate`, host-tested).

## The crash banner after a flash over USB

The web flasher, espflash and `esptool -a watchdog-reset` leave download
mode by arming the RTC watchdog, so the app's first boot after a flash
reports `wdt`, which the firmware counted as a crash since v0.1.34: the
crash banner, the 30 s on USB-Serial/JTAG before USB OTG, and a core dump
left from an earlier crash taken for this reset. A restart marker tells
the case apart only for a reinstall that began at *System → Download
mode*; the first install over AxeOS, NerdOS or ForgeOS has none.

From v0.1.37 the firmware stores the first nine hex digits of the running
image's ELF SHA in NVS (`lastsha`, written once per new image, on the main
task right after the restart marker is taken) and reads the previous one
first: an RTC-watchdog reset on the first boot of an image with no restart
marker open is the flashing tool's reset, not a crash
(`warpaxe_diag::restart::crashed`, host-tested: the first boot without a
marker, a later boot of the same image, an update's marker, the way into
download mode, and panic, interrupt and task watchdog, which are always a
crash), and not after a rollback either (the bootloader's `aborted`: the
new image's own end must stay a crash, see below). `crashed` in the API,
the banner and the JTAG window follow, the boot event does not say "no
core dump written" for it, and the last words are kept only for a crash;
the boot event still names `wdt`, and the log says why it was not
counted. Two cautious misses stay: reflashing the identical image over
USB without the *Download mode* marker shows the banner, as does
USB-flashing back the image that last stored the SHA. The bootloader's own image state is no substitute:
a bootloader without rollback support (before v0.1.22) reports `new` for
ever, which would forgive every watchdog on such a miner.

Blind spot, accepted: a real RTC-watchdog hang in the very first boot of
a new image, before the SHA is stored (the first moments of `main`), is
forgiven at the second boot too. A hang in the restart after an update is
not affected: its marker is open, and an update's marker with a watchdog
reset is a crash of that restart. The rollback case is closed by the
bootloader's state: when a new image took the marker, stored its SHA and
then ended in the RTC watchdog (the panic handler arms it against a hung
dump write) before it proved itself, the bootloader boots the old image
as `aborted`, which forgives nothing.

## Reproduction (not run: needs Oliver's OK and a word to Warppool #2)

The Gamma .42 is on USB (`/dev/cu.usbmodem9888E00355C81`). Proposed:

1. Flash this branch's build over USB with the partition table made from
   this branch's `partitions.csv` (`gen_esp32part.py partitions.csv
   partition-table.bin`, flashed at 0x8000: a worktree build's own table
   is main's, see the note above); keep the ELF. The boot log must then say
   `core dump partition 256 KB`.
2. Upload v0.1.34's `warpaxe.bin` (the release asset; ELF in
   `~/.config/warppool/warpaxe-release-elf/v0.1.34`) through the web UI. It
   runs on the new table.
3. Press Update → v0.1.36. Capture the CDC console meanwhile: it shows the
   log up to "chips off before the reset" and nothing of a panic (see 4
   above).
4. Read `/api/v1/events` and `/api/system/info` (`resetReason`,
   `coreDumpBytes`, `coreDumpCurrent`).
5. Repeat 2–4 three times.

A plain Gamma (EMC2101, one fan output) may not reproduce what a Turbo
(EMC2103) did, and v0.1.34 has none of this branch's traces; a crash in its
restart would show only through the core dump, which the larger partition
now lets it write. A crash in this branch's own restart path would show
through the marker and the last words on any partition.

## Tests

- `warpaxe-diag`: the marker's round trip, damaged and foreign entries,
  field cutting against the format and the size limit, the outcome
  against the reset reason, the crash detail's English text and its ring
  round trip.
- `warpaxe-diag`: `crashed` (the watchdog on the first boot, on a later
  boot, with an update's marker, after download mode; the crashes that
  always count) and `outcome` after download mode for every reason (the
  watchdog and a clean reset complete it, a panic crashes it).
- `warpaxe-sv2`: a certificate signed by another authority and one
  checked against a clock a month ahead are `Error::Certificate`, the
  latter naming the window and the clock.
- The firmware builds (the generated `sdkconfig` carries
  `CONFIG_ESP_INT_WDT_TIMEOUT_MS=1000`); fmt, clippy, the workspace tests,
  the no_std build, the scenarios and `tools/i18n-apply.py` pass.
- No device was flashed for this branch.

## Audit

An independent read of the diff (2026-10-23) found nothing critical. Fixed
from it: a false crash event after every web-flasher install that began
with *Download mode* (the flasher leaves download mode with the RTC
watchdog, which read as a crash of that restart); uploads were not counted
as installs (their origin is `ota`); a crash in the old image's restart
left the new image without its `installed` event on an old bootloader (the
very case above); the marker was taken late enough for a crash in the
boot's first steps to be blamed on the restart; the last-words block moved
with the image's RTC layout, so the new image would not have found the old
image's words; the UI never showed the two new event texts (its code list
did not have them); the reproduction's first step flashed main's table.
Lows taken: the panic hook's task name from FreeRTOS instead of a std
handle (no allocation, every task has one), its buffer in a frame of its
own, the glue in IRAM, the marker key removed even when it does not read,
fields cut by bytes, `lastWords` scrubbed. Noted, not changed: a power
cycle after a hang reads as no crash; a marker survives a downgrade; two
panics at once garble the words; the restart's purpose stays an English
word in the translated event.

A second independent read (2026-10-24) of the download-mode fix and the
Sv2 clock change confirmed the panic chain end to end from the sources
(including that `tusb_teardown` is the real `tusb_deinit`, that nothing
logs inside `tud_deinit`, that the Rust logger writes through the same
stdout `FILE` and lock, and that `/dev/console` is registered in this
configuration). Fixed from it: the scenario test for a wrong authority
key accepted only `Handshake`/`BadKey` and would have failed on every run
in which the flipped key was still on the curve; the wait for NTP's
confirmation applied to every reconnect for the whole boot when NTP
never syncs, now to the first five minutes only; the "clock unsure"
verdict was re-read at failure time instead of with the clock it was
about, and could outlive a session that ended in a `continue`;
`freopen`'s result was ignored. Noted, not changed: `tusb_stop_task` is
a bare `vTaskDelete`, so a TinyUSB task deleted while printing would
keep the stdout lock (the 20 ms after the sends stop close that window
in practice); the `B` on the console port enters download mode without
the restart marker and without switching the chips off; the file
descriptors behind the three streams change numbers across the
`freopen`s, which is harmless here (every one of 0–3 ends on the
console).

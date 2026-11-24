# An update with a fragmented internal heap

Written 2026-10-20 (branch `fix/ota-fragmentation`, on `feat/v0132`),
after a NerdQAxe++ on the test stand refused three `POST
/api/system/OTA` in a row with "not enough internal memory to start the
update (45203–47415 bytes free), try again" and took the same image at
once after a restart. The click update takes the same path, so a miner
in that state could not update itself at all, and v0.1.31 ships with it.
The rollout of 2026-10-18 went through only because the miners had been
restarted shortly before.

## What happened

Writing an app image to flash needs a task whose stack sits in internal
RAM: the cache is off while flash is written, and a stack in PSRAM
cannot be reached then (the lesson of v0.1.5, which rebooted every
minute once a PSRAM thread touched NVS). Up to v0.1.31 the writer was a
thread spawned for the write, with a 16 KB stack taken from the internal
heap at that moment (`api.rs` for the upload, `update.rs` for the click
update). After hours of mining the internal heap is in pieces: 45 KB
free in all, no piece of 16 KB. `heap_caps_get_free_size` says 45 KB,
`heap_caps_get_largest_free_block` would have said less than 16, and the
spawn failed with ENOMEM.

The fleet on 2026-10-20 ~06:50Z, `freeHeapInternal` from
`/api/system/info`:

| Miner | Firmware | Up | Internal free |
|---|---|---|---|
| .40 | v0.1.31 | 152 000 s | 35 KB |
| .41 | test (e6835c9) | 2 800 s | 33 KB |
| .42 | test (e6835c9) | 2 600 s | 52 KB |
| .44 | v0.1.31 | 152 000 s | 48 KB |
| .45 Gaia | v0.1.31 | 147 000 s | 26 KB |
| .46 | v0.1.31 | 152 000 s | 38 KB |

Two settings of the build frame these figures:
`CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=16384` sends every allocation
below 16 KB to the internal heap first (so every JSON reply, every
format!, every small Vec of every thread churns it), and
`CONFIG_SPIRAM_MALLOC_RESERVE_INTERNAL=32768` keeps the last 32 KB of it
for allocations that must be internal (task stacks, DMA, Wi-Fi), which
is why the Gaia sits at 26 KB and still runs. A suspect for the holes
(Warppool #2): until b9b019d the swarm page's Avalon read started a
16 KB thread from internal RAM about every 30 s on every miner whose
list names a `cgminer:` host (all six); since b9b019d it runs on a PSRAM
stack behind a heap check. Not proven; the measurement below is meant to
show it.

## The fix: the main task is the writer

Three ways were on the table:

1. **Remember "install vX" in NVS, restart, install early in the next
   boot** before the memory-hungry services start. Works, but costs the
   user a second restart, a new boot path that must never loop (flag,
   download, failure, flag cleared), new UI texts in eight languages,
   and does nothing for the upload path, whose image is gone after a
   restart.
2. **Reserve the writer's stack at boot**: 16 KB of internal RAM for
   good, on a Gaia that has 26 KB left. Too dear.
3. **Use a stack that already exists.** The main task's
   (`CONFIG_ESP_MAIN_TASK_STACK_SIZE=16000`) is allocated by FreeRTOS
   before anything else, lies in internal RAM, is never freed, and after
   `set_step("RUN")` the task only slept. It is the writer now
   (`update::serve_writes`): the online update and the upload hand their
   image over (`update::submit`), the main task writes it, stores the
   install record, parks the chips and restarts. A failed write reports
   as before and the task waits for the next job. Cost: nothing. A
   second image while one is queued or being written is refused at
   once and the caller says so (409 on the upload, the error line of
   the click update); the network search also waits while a write is
   queued.

Option 3 was taken: it removes the failure instead of recovering from
it, for both paths, with no new state in flash and no new texts. The
hand-off's rules (one job at a time, a second one refused and named,
`done` after a failed write, a queued job never lost) are the host-tested
crate `warpaxe-ota`.

Unchanged: the signature check (before the hand-off, in `install`), the
SHA-256 against the release file, the image's first byte, the rollback
with `pending-verify` (`mark_app_valid` at chain ready or after three
minutes), `prepare_reset` before the restart.

## Measurable now

- `largestFreeInternal` in `/api/system/info`: the largest piece of
  internal heap in one go, bytes. A task stack or a DMA buffer must fit
  into it; `freeHeapInternal` says nothing about that.
- The log: `heap: N KB internal free, largest piece M KB` at the end of
  boot, the same line at every hand-off (`update: … bytes handed to the
  writer (…)`, `ota: …`), and after a write `… bytes of the writer's
  stack never used` (FreeRTOS' high-water mark of the main task).

## The test build

`cargo build --release --features fragtest` adds `POST
/api/v1/diag/fragment`: it carves the internal heap into 8 KB blocks
until none is left and gives every other one back, so what remains free
is half the heap in holes of 8 KB. No 12 or 16 KB stack can be had any
more (the writers of v0.1.31 would be refused), the 6 KB threads of a
pool reconnect still fit. `?free=1` releases the blocks. Not in a
release build.

## Measurements

_Filled in as they come; nothing here is from notes._

### Hardware test on .41 (NerdQAxe++, 2026-10-20 07:17–07:28Z)

The owner's OK through dvb-WarpAxe #2, Warppool #2 told; images from
commit 5a84125 only. `free` / `largest` = `freeHeapInternal` /
`largestFreeInternal`.

| Step | What | free / largest | Outcome |
|---|---|---|---|
| 1 | fragtest image uploaded over b9b019d (20 min up, 33 KB free) | boot end (`step: RUN`): 90 / 31 KB; 17 s: 54 / 22; 86 s: 55 / 20 | up, marked valid at chain ready |
| 2 | `POST /api/v1/diag/fragment` | 55 / 20 → 45 / 8 KB | only three 8 KB blocks were to be had in one piece: the rest was already smaller |
| 3 | click update to the published v0.1.31 under that heap | largest 8 KB | written, restarted, back as v0.1.31 (1b4322b) with the record "v0.1.31 verified" |
| 4a | fragtest image uploaded over v0.1.31 (fresh boot) | 90 / 31 at boot end | up, marked valid |
| 4b | `fragment` | 55 / 20 → 45 / 9 KB | |
| 4c | fix image uploaded under that heap | 36 / 8 KB at the hand-off | `ota: 2874960 bytes handed to the writer`, `writing … on the main task`, erase 13 s, write 81 s, verify 4.5 s, `written and verified, restarting (8584 bytes of the writer's stack never used)`, `board: update: chips off before the reset`; back as 5a84125, pending-verify → valid at chain ready |

So the writer needs about 7.4 KB of the main task's 16000; the 16 KB
thread of v0.1.31 could not have been spawned at any point of steps 3
and 4c. Even a fresh boot has its largest piece at a third of the free
total.

### The heap over hours (2026-10-20 07:05–09:34Z)

Fleet log once a minute from `/api/system/info` (`freeHeapInternal`,
`largestFreeInternal` where the image reports it). .41 ran the fix
image 5a84125 from 07:32Z (Sv2 at the pool, chain up, nobody else on
it). Phase A: the swarm page open in a browser for an hour (the Avalon
card refreshing, the network search clicked once at the start). Phase
B: the hour after, no page open.

| .41 | Samples | free, min–max (mean) | largest piece |
|---|---|---|---|
| A, 07:33–08:33Z, swarm page open | 59 | 25 631–34 087 B (31 261) | 20 480 B in every sample |
| B, 08:33–09:33Z, no page | 59 | 28 735–36 147 B (32 805) | 20 480 B in every sample |

- **The largest piece does not move.** 24 576 B at 36 s up, 20 480 B
  from the second minute on and in all 119 samples after, through both
  phases. The holes are cut while the services come up (the test table
  above shows the same: 31 KB at the end of boot, 22 KB at 17 s, 20 KB
  at 86 s), not by anything that runs periodically afterwards.
- **The swarm page changes nothing** that matters. The free total sits
  about 1.5 KB lower with the page open (its polls and the Avalon read
  churn the heap), inside the ±4 KB band both phases show, and the
  largest piece is the same number. The PSRAM read of b9b019d is in
  the clear.
- **The fleet on the release** (821e8d2, rebooted into it between
  07:51Z and 09:04Z) shows the same shape: a largest piece of 25–32 KB
  that is constant per miner (.42: 31 744 B in all 102 samples over
  100 min; .40, .44, .45, .46: one step of 1 KB at most in 30–40 min)
  while the free total wanders by 10–18 KB.
- **The old fleet** before the release (v0.1.31, up 150 000 s) did not
  lose free total over the morning either: .40 30 955 → 39 007 B,
  .45 28 863 → 30 143 B between 07:05Z and the reboot. Those images
  report no largest piece, so whether the 16 KB cgminer thread of
  v0.1.31 cut the holes over days stays unproven; two hours on an image
  that no longer has it cannot show it. What the morning shows is that
  the update refused in the handover (45 KB free, no 16 KB piece at
  2 800 s up) is the normal state of a NerdQAxe++ a minute after boot,
  no days needed.

Why a fresh boot already has a largest piece of a third of its free
total: the internal heap is several regions, and a free run cannot
cross one. The 90 KB at the end of boot are spread over them; the
biggest run sits in one.

What else allocates internal RAM repeatedly, from reading the code:

- **Stratum** spawns a 6 KB internal sender thread per session (V1 and
  V2, `stratum.rs`); it ends with the session. One hole per reconnect
  at most, and the next one usually takes the freed stack.
- **HTTP**: `esp_httpd` buffers per request and every JSON reply go
  internal first (below 16 KB, `ALWAYSINTERNAL`). Short-lived; that is
  the ±4 KB wander of the free total.
- **mbedTLS** allocates from PSRAM (`CONFIG_MBEDTLS_EXTERNAL_MEM_ALLOC`),
  so a TLS session does not touch the internal heap.
- **Swarm**: the Avalon read and the scan workers run on PSRAM stacks
  since b9b019d.
- Everything else (board, miner, Wi-Fi, mDNS, display, history, market,
  update, sv2) is one thread, created at boot.

Not done, a candidate if the free total ever gets tight: lowering
`CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL` (say to 4 KB) would send the
JSON replies and the larger buffers to PSRAM. Slower replies, less
churn; measure before, measure after.

## Audit

- **Restart and download mode** (`POST /api/system/restart`,
  `/bootloader`) spawned a thread with the default 8 KB internal stack;
  on the heap above `std::thread::spawn` would panic, and the panic
  resets the board without `prepare_reset` (chips powered through the
  reset, docs/04). Both go through the same hand-off now
  (`update::Work`); a restart asked while a write is queued or under
  way is dropped, since that write ends in a restart of its own. Read
  only, not exercised on hardware in the test above.
- **Boot loop**: no new boot path. The writer takes a job only after
  `set_step("RUN")`; a job that fails to write reports and is dropped.
- **Rollback**: a written image boots as `pending-verify` and is marked
  valid at chain ready or after three minutes, as before; the record
  `lastinstall` is written by the main task (internal stack), as the
  writer thread did.
- **Signature**: checked in `install` before the hand-off; an image
  whose signature does not verify never reaches the writer.
- **PSRAM threads and flash** (the v0.1.5 rule): the hand-off adds no
  flash access on a PSRAM stack. `submit` logs and takes a mutex; the
  write, the NVS record and the restart run on the main task. The
  Avalon read of b9b019d (`swarm.rs start_read`, PSRAM): the NVS read
  `swarm_has_cgminer` happens in `lookup` on the HTTP task before the
  thread starts; the thread opens a socket, parses JSON
  (`warpaxe_avalon`), writes the RAM cache and logs (UART and the RAM
  ring, `diag::RingLogger`); `config::get()` reads RAM. The search's
  workers the same. No NVS, no flash. Confirmed by reading, 2026-10-20.

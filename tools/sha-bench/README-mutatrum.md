# SHA bench for the ESP32-P4: how to run it

This is a small, self-contained test program. It measures the ESP32-P4's
SHA-256 hardware engine from the inside: what a register access costs, how
long the engine is busy per 64-byte block, whether the input registers can
be refilled while it runs, what the DMA path costs, and how many double
SHA-256 hashes per second a few loop variants reach on it. It does the
same measurements we published for the ESP32-S3.

It does **no mining and no networking**. There is no Wi-Fi, no Ethernet,
no pool. It hashes the Bitcoin genesis block's header over and over (2048
nonces per variant), checks every hardware result against a software
SHA-256, and must find the genesis block's real nonce, which sits inside
the range it scans. It prints its results on the serial console, once a
few seconds after boot and then every 30 seconds.

## Which image

| File | For |
|---|---|
| `sha-bench-esp32p4.bin` | ESP32-P4 with silicon revision **v1.x** (pre-v3), which the Waveshare ESP32-P4-ETH has (v1.3) |

The image is built with ESP-IDF v5.3.3, which supports P4 revisions v0.x
and v1.x. On a v3.x part the bootloader refuses to start it and prints a
message about the chip revision. If one of your other boards does that,
tell us the revision from the boot log and we build an image for it.

The image is a complete flash image for offset 0 (bootloader, partition
table, app). It declares 4 MB of flash and uses about 300 KB; a 32 MB chip
is fine, the rest is simply not touched.

Check the file before flashing:

```
shasum -a 256 sha-bench-esp32p4.bin
```

The expected value is in `sha-bench-esp32p4.bin.sha256` next to it.

## Before flashing: back up what is on the board

The image **overwrites the firmware** on the board (the first 4 MB; the
remaining flash stays as it is). To get the board back afterwards, read
the flash first. The first 4 MB is enough to undo this program; the whole
32 MB takes a few minutes longer and is the safer backup if you are not
sure what else lives on the chip.

```
esptool.py --chip esp32p4 -p PORT -b 921600 read_flash 0x0 0x400000 backup-first-4mb.bin
esptool.py --chip esp32p4 -p PORT -b 921600 read_flash 0x0 0x2000000 backup-full-32mb.bin
```

You need esptool 4.7 or newer for the P4 (`pip install -U esptool`). Any
ESP-IDF 5.3+ install has one.

## The serial port

On the Waveshare ESP32-P4-ETH the USB-C socket is the programming port; it
is wired to a USB-to-UART bridge. The device shows up as

- Linux: `/dev/ttyUSB0` or `/dev/ttyACM0`
- macOS: `/dev/cu.usbserial-*` or `/dev/cu.usbmodem*`
- Windows: `COMx`

Use that as `PORT` below. The 4-pin USB OTG header is not involved.

## Flash

```
esptool.py --chip esp32p4 -p PORT -b 460800 write_flash 0x0 sha-bench-esp32p4.bin
```

esptool normally resets the board into download mode by itself over the
bridge. If it keeps printing "Connecting........" and fails: hold **BOOT**,
press and release **RST**, release **BOOT**, and run the command again.
If the transfer shows errors at 460800, use `-b 115200`.

After flashing, press **RST** once (or unplug and replug) so the board
starts the program normally.

## Watch the output

Any serial terminal at **115200 baud** works:

```
idf.py monitor -p PORT                     # with ESP-IDF installed (Ctrl-] to quit)
screen PORT 115200                         # Ctrl-A then K to quit
python -m serial.tools.miniterm PORT 115200   # pyserial comes with esptool, Ctrl-] to quit
```

The program prints a header with chip, revision, CPU and bus clocks, a
line `software reference against the genesis hash: ok`, then every 30 s a
table (sections T, A, B, D, V) followed by one line beginning with

```
SHABENCH {"run":1,"chip":"esp32p4","rev":"v1.3", ... }
```

That line is the machine-readable result. In section V every row must
show `bad 0`, and the last line of the section must say the firmware loop
found the genesis block.

## What to send us

The whole console output from the boot messages through at least the
**second** `SHABENCH` line (the second run is the steady state, the first
one is fine too). Copy and paste as text; a screenshot works in a pinch
but the JSON line is what we parse. If you run it on more than one board,
one capture per board, with the board's name.

For comparison, the ESP32-S3 at 240 MHz comes out at about 140 CPU cycles
of engine time per block, 31 cycles per register load, and about 325 kH/s
for the best loop variant.

## Afterwards: restore the board

```
esptool.py --chip esp32p4 -p PORT -b 460800 write_flash 0x0 backup-first-4mb.bin
```

(or the full backup file at 0x0). Press RST and the board is back to what
it was.

## What the program does, in one paragraph

It takes the mbedTLS hardware lock (so nothing else touches the SHA
engine), sets SHA-256 mode and measures with the CPU cycle counter: 16
stores to the message registers with and without a FENCE, 8 stores and 8
loads of the digest registers, a lone BUSY read; how many cycles after
START/CONTINUE the engine reports idle, both polled and as the smallest
fixed delay, and again after the write buffer was drained first; after
which delay the message registers may be overwritten without changing the
digest (0 means the engine latches its input at the trigger); the DMA path
over one and two blocks and whether it goes through the message registers;
and finally six loop variants over 2048 nonces, each hash checked in
software. Source: `tools/sha-bench` and
`firmware/components/warpaxe_glue/warpaxe_shabench.c` in the WarpAxe
repository, documented in `docs/06-Devkit-S3-SHA-Peripheral.md`.

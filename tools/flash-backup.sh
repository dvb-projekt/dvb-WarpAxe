#!/usr/bin/env bash
# Full flash backup of an ESP32-S3 miner before WarpAxe touches it, and
# the one-line restore. Reads the whole flash (size from the chip),
# writes <out>/<name>-<date>.bin plus .sha256 and a RESTORE.txt.
#
#   tools/flash-backup.sh <name> [port] [out-dir]
#
#   name     label for the files, e.g. bitaxe-gamma-601
#   port     serial port; default: the single /dev/cu.usbmodem* present
#   out-dir  default: ~/Library/Mobile Documents/com~apple~CloudDocs/Bitcoin/Claude/WarpForge-Firmware/backups
#
# The board must be reachable by esptool: on a Bitaxe (native USB) the
# USB-Serial/JTAG port appears while the stock firmware runs; esptool
# resets it into the ROM loader itself. Mining stops during the read
# (about 2 min for 16 MB) and the board reboots into its old firmware
# afterwards. Nothing is written.
#
# Restore (also written to RESTORE.txt next to the image):
#   python3 -m esptool --chip esp32s3 -p <port> -b 921600 write_flash 0 <image>.bin
set -euo pipefail

name=${1:?usage: flash-backup.sh <name> [port] [out-dir]}
port=${2:-}
out=${3:-"$HOME/Library/Mobile Documents/com~apple~CloudDocs/Bitcoin/Claude/WarpForge-Firmware/backups"}
esptool=${ESPTOOL:-"python3 -m esptool"}

if [[ -z $port ]]; then
    ports=(/dev/cu.usbmodem*)
    if [[ ${#ports[@]} -ne 1 || ! -e ${ports[0]} ]]; then
        echo "expected exactly one /dev/cu.usbmodem* port, found: ${ports[*]}" >&2
        exit 1
    fi
    port=${ports[0]}
fi

mkdir -p "$out"
stamp=$(date +%Y-%m-%d)
img="$out/$name-full-$stamp.bin"
if [[ -e $img ]]; then
    echo "refusing to overwrite $img" >&2
    exit 1
fi

echo "== chip and flash size on $port"
id=$($esptool --chip esp32s3 -p "$port" flash_id)
echo "$id" | grep -E 'Chip is|Detected flash size|MAC' || true
size=$(echo "$id" | sed -n 's/.*Detected flash size: \([0-9]*\)MB.*/\1/p')
if [[ -z $size ]]; then
    echo "could not read the flash size from esptool" >&2
    exit 1
fi
bytes=$((size * 1024 * 1024))

echo "== reading $size MB to $img"
$esptool --chip esp32s3 -p "$port" -b 921600 read_flash 0 "$bytes" "$img"

echo "== checksum"
(cd "$out" && shasum -a 256 "$(basename "$img")" | tee "$(basename "$img").sha256")
actual=$(stat -f %z "$img")
if [[ $actual -ne $bytes ]]; then
    echo "image is $actual bytes, expected $bytes" >&2
    exit 1
fi

cat > "$out/$name-full-$stamp.RESTORE.txt" <<TXT
Full flash image of $name, read $stamp from $port ($size MB).
Verify:  shasum -a 256 -c $(basename "$img").sha256
Restore: python3 -m esptool --chip esp32s3 -p <port> -b 921600 write_flash 0 $(basename "$img")
The restore rewrites everything: bootloader, partition table, both app
slots, NVS (Wi-Fi, pool, tuning point) and any data partitions. Hold the
BOOT button while plugging in if the board no longer enters the loader
on its own.
TXT
echo "== done: $img"
echo "   restore notes in $(basename "$img" .bin).RESTORE.txt"

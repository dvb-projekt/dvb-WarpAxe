#!/usr/bin/env python3
"""Where the firmware image's bytes are (docs/16).

Sums the linker map of the last firmware build by section and by who put
the bytes there: the firmware crate, the workspace crates, other Rust
crates, core/alloc/std, and ESP-IDF's C libraries in groups. With an
image file it also prints the image's size and what is left below the
release limit 0x2F0000 (tools/release-firmware.sh).

  cd firmware/warpaxe && cargo build --release
  espflash save-image --chip esp32s3 --flash-size 16mb \\
      target/xtensa-esp32s3-espidf/release/warpaxe /tmp/warpaxe.bin
  python3 tools/image-breakdown.py --image /tmp/warpaxe.bin
  python3 tools/image-breakdown.py --items .flash.rodata 40   # the largest pieces

The map is the one the final link writes (esp-idf-sys passes
-Wl,--Map=…/out/build/libespidf.map); the newest one under
firmware/warpaxe/target is taken unless --map names another.

Two things to know when reading it:
- String sections of C objects (.rodata.str1.1 and the like) are merged
  by the linker and listed with their sizes before merging; they are
  shown as one line, the section's size minus everything else.
- Generic code is counted where it was instantiated: serde_json's
  parser for the firmware's Config is "firmware crate", not serde_json.
"""
import argparse
import collections
import re
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LIMIT = 0x2F0000
SECTIONS = (".iram0.vectors", ".iram0.text", ".dram0.data", ".dram0.bss",
            ".flash.text", ".flash.appdesc", ".flash.rodata")
IN_IMAGE = (".iram0.vectors", ".iram0.text", ".dram0.data",
            ".flash.text", ".flash.appdesc", ".flash.rodata")

WIFI = {"libnet80211", "libpp", "libphy", "libcore", "libcoexist", "libwpa_supplicant",
        "libesp_wifi", "libesp_phy", "libesp_coex", "libmesh", "libespnow",
        "libsmartconfig", "libwapi", "libbtbb"}
NET = {"liblwip", "libesp_netif", "libespressif__mdns", "libesp_eth"}
TLS = {"libmbedtls", "libmbedcrypto", "libmbedx509", "libesp-tls", "libeverest", "libp256m"}
LIBC = {"libc", "libc_nano", "libm", "libm_nano", "libgcc", "libstdc++", "libnewlib", "libcxx", "libpthread"}
USB = {"libespressif__tinyusb", "libesp_tinyusb", "libusb"}
HTTP = {"libesp_http_server", "libhttp_parser", "libesp_http_client", "libtcp_transport",
        "libesp_https_ota", "libesp_https_server"}
STD = {"core", "alloc", "std", "compiler_builtins", "rustc_demangle", "hashbrown",
       "panic_abort", "libc", "memchr", "unwind", "std_detect", "cfg_if"}
STRINGS = "C strings (log texts, asserts, names; merged)"


def group(src):
    m = re.search(r"([^/]+)\.(a|rlib)\(([^)]*)\)$", src)
    if m:
        lib = m.group(1)
        if m.group(2) == "rlib":
            crate = re.sub(r"^lib", "", re.sub(r"-[0-9a-f]{16}$", "", lib))
            if crate.startswith("warpaxe_"):
                return "Rust: workspace crates (warpaxe-*)"
            if crate in STD:
                return "Rust: core, alloc, std"
            if crate == "secp256k1_sys":
                return "Rust dependency in C: secp256k1"
            return "Rust: other crates"
        for libs, name in ((WIFI, "C: Wi-Fi, WPA, PHY"), (NET, "C: lwIP, netif, mDNS"),
                           (TLS, "C: mbedTLS, esp-tls"), (LIBC, "C: libc, libm, libgcc, pthread"),
                           (USB, "C: USB (TinyUSB, NCM, CDC)"), (HTTP, "C: HTTP server and client")):
            if lib in libs:
                return name
        return "C: ESP-IDF system, drivers, FreeRTOS"
    if re.search(r"/deps/warpaxe-[0-9a-f]{16}\.", src):
        return "Rust: firmware crate"
    return "C: ESP-IDF system, drivers, FreeRTOS"


def read_map(path):
    lines = path.read_text(errors="replace").split("\n")
    start = next(i for i, l in enumerate(lines) if l.startswith("Linker script and memory map"))
    end = next(i for i, l in enumerate(lines) if l.startswith("Cross Reference Table"))
    one_line = re.compile(r"^ (\S+)\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+(\S.*)$")
    second_line = re.compile(r"^\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+(\S.*)$")
    head = re.compile(r"^(\.\S+)\s+(0x[0-9a-f]+)\s+(0x[0-9a-f]+)")
    size_of, has_strings = {}, set()
    total = collections.defaultdict(lambda: collections.defaultdict(int))
    items = collections.defaultdict(list)
    cur = pending = None
    for l in lines[start:end]:
        if l and not l[0].isspace():
            name = l.split()[0]
            cur = name if name in SECTIONS else None
            m = head.match(l)
            if cur and m:
                size_of[cur] = int(m.group(3), 16)
            pending = None
            continue
        if cur is None:
            continue
        m = one_line.match(l)
        if m:
            sec, _, size, src = m.groups()
        else:
            m = second_line.match(l)
            if m and pending:
                sec = pending
                _, size, src = m.groups()
            else:
                s = l.strip()
                pending = s if s and " " not in s else None
                continue
        pending = None
        size = int(size, 16)
        if size == 0:
            continue
        if sec == "*fill*":
            total[cur]["(alignment)"] += size
        elif re.search(r"\.str\d+\.\d+$", sec):
            has_strings.add(cur)
        elif src and not src.startswith("0x"):
            total[cur][group(src)] += size
            items[cur].append((size, group(src), sec))
    for sec in has_strings:
        total[sec][STRINGS] = size_of[sec] - sum(total[sec].values())
    return size_of, total, items


def image(path):
    b = path.read_bytes()
    off = 24
    print(f"{path.name}: {len(b)} bytes, {LIMIT - len(b)} below 0x2F0000")
    for i in range(b[1]):
        addr, n = struct.unpack_from("<II", b, off)
        print(f"  segment {i}: {n:8d} bytes at {addr:#010x}")
        off += 8 + n
    print()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--map", type=Path, help="linker map (default: the newest firmware build's)")
    ap.add_argument("--image", type=Path, help="warpaxe.bin from espflash save-image")
    ap.add_argument("--items", nargs=2, metavar=("SECTION", "N"),
                    help="also list the N largest pieces of SECTION")
    a = ap.parse_args()
    path = a.map
    if path is None:
        maps = sorted((ROOT / "firmware/warpaxe/target/xtensa-esp32s3-espidf/release/build")
                      .glob("esp-idf-sys-*/out/build/libespidf.map"),
                      key=lambda p: p.stat().st_mtime)
        if not maps:
            sys.exit("no linker map: build the firmware first (cargo build --release)")
        path = maps[-1]
    if a.image:
        image(a.image)
    size_of, total, items = read_map(path)
    print(f"map: {path}")
    for sec in SECTIONS:
        if sec not in size_of:
            continue
        where = "in the image" if sec in IN_IMAGE else "RAM only, not in the image"
        print(f"\n{sec}  {size_of[sec]} bytes  ({where})")
        for g, s in sorted(total[sec].items(), key=lambda x: -x[1]):
            print(f"  {s:9d}  {g}")
    print(f"\nsections in the image: {sum(size_of.get(s, 0) for s in IN_IMAGE)} bytes")
    if a.items:
        sec, n = a.items[0], int(a.items[1])
        print(f"\nthe {n} largest pieces of {sec}:")
        for s, g, name in sorted(items[sec], reverse=True)[:n]:
            print(f"  {s:8d}  {g[:34]:34s} {name[:80]}")


if __name__ == "__main__":
    main()

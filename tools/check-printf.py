#!/usr/bin/env python3
"""Find printf formats in the firmware image that newlib nano would break.

The build uses newlib's nano printf (CONFIG_NEWLIB_NANO_FORMAT, docs/16).
Its parser knows the length modifiers h, l and L only; a conversion with
ll, hh, j, z, t, q or a position (%1$s) is printed as its letter and takes
no argument, so every argument after it shifts. A %s after such a spec
reads half of a 64-bit value as a pointer: a crash when that line prints.

    tools/check-printf.py target/release-firmware/warpaxe.bin [--all]

Reads every printable string of the file (the save-image output, the ELF,
anything), lists the strings with such a spec, and fails (exit 1) when one
of them has a %s after it and is not in KNOWN below (docs/16 "Where it
would print wrongly" says why each of those is harmless). --all also
prints the hits without a %s after them (wrong numbers in a log line,
nothing shifts into a pointer). Called by tools/release-firmware.sh: CI
builds no ESP image.
"""

import re
import sys

# Strings with a broken spec and a %s after it that are known and cannot
# print: esp_http_client's digest auth is linked but unreachable
# (CONFIG_ESP_HTTP_CLIENT_ENABLE_DIGEST_AUTH off; docs/16). A substring
# of the string is enough.
KNOWN = (
    "%s:%s:%016llx",
    ":%016llx:%s:%s",
)

MIN_LEN = 4
# One conversion: flags, width, precision, length, conversion letter.
SPEC = re.compile(
    rb"%(?P<pos>\d+\$)?[-+ #0']*(?:\d+|\*)?(?:\.(?:\d+|\*))?"
    rb"(?P<len>hh|ll|h|l|L|j|z|t|q)?(?P<conv>[diouxXcspnfFeEgGaA%])"
)
BROKEN_LEN = {b"hh", b"ll", b"j", b"z", b"t", b"q"}
PRINTABLE = re.compile(rb"[\x20-\x7e\t]{%d,}" % MIN_LEN)


def broken(spec):
    return spec.group("pos") is not None or spec.group("len") in BROKEN_LEN


def scan(data):
    """(string, has a %s after a broken spec) for every string with one."""
    for m in PRINTABLE.finditer(data):
        s = m.group(0)
        if b"%" not in s:
            continue
        specs = [x for x in SPEC.finditer(s) if x.group("conv") != b"%"]
        first = next((i for i, x in enumerate(specs) if broken(x)), None)
        if first is None:
            continue
        # Machine code that happens to read like a format: a broken spec
        # alone in a run of noise. Kept: the check would rather list one
        # too many than miss one.
        crash = any(x.group("conv") == b"s" for x in specs[first + 1 :])
        yield s.decode("ascii", "replace"), crash


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("--")]
    show_all = "--all" in argv
    if len(args) != 1:
        print(__doc__.strip().splitlines()[0], file=sys.stderr)
        print(f"usage: {argv[0]} <image.bin|elf> [--all]", file=sys.stderr)
        return 2
    data = open(args[0], "rb").read()
    bad = []
    seen = set()
    for s, crash in scan(data):
        if s in seen:
            continue
        seen.add(s)
        known = any(k in s for k in KNOWN)
        if crash and not known:
            bad.append(s)
        elif show_all or crash:
            tag = "known, unreachable" if crash else "wrong numbers only"
            print(f"  ({tag}) {s!r}")
    for s in bad:
        print(f"nano printf would crash on: {s!r}", file=sys.stderr)
    if bad:
        print(
            f"{len(bad)} format(s) with ll/hh/j/z/t/q or a position before a %s "
            "(docs/16, nano printf): use l/PRIu32 or format in Rust",
            file=sys.stderr,
        )
        return 1
    print(f"printf formats: none that nano printf would crash on ({len(seen)} with a spec it misreads)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

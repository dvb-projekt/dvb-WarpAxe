#!/usr/bin/env python3
"""Put a firmware image on a test miner and watch it the way a user runs it.

    tools/soak-test.py HOST --upload warpaxe.bin [--minutes 5]
    tools/soak-test.py HOST --click              [--minutes 5]
    tools/soak-test.py HOST --watch              [--minutes 5]

--upload flashes the image over /api/system/OTA, --click installs the
latest published release through the online update (the path most users
take; it writes the install record), --watch only observes.

Then, with the automatic update check switched on, the miner is watched
for --minutes: it must not restart, it must hash and find shares, and its
update watcher must have run a check. v0.1.5 passed every bench test and
still restarted once a minute in the field, because the watcher tripped
over the install record only a click install leaves behind; so --upload
refuses a miner without that record unless --no-record is given.

Exit 0 = pass, 1 = fail. Standard library only.
"""
import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def call(host, path, method="GET", body=None, timeout=10, ctype="application/json"):
    req = urllib.request.Request(f"http://{host}{path}", data=body, method=method)
    if body is not None:
        req.add_header("Content-Type", ctype)
    # A busy miner (right after a boot, or during an update check) can let
    # a single GET time out; try reads three times before giving up.
    tries = 3 if method == "GET" else 1
    for n in range(tries):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                raw = r.read()
            break
        except (urllib.error.URLError, OSError):
            if n == tries - 1:
                raise
            time.sleep(2)
    try:
        return json.loads(raw)
    except ValueError:
        return raw.decode("utf-8", "replace")


def info(host):
    return call(host, "/api/system/info")


def say(msg):
    print(time.strftime("%H:%M:%S"), msg, flush=True)


def fail(msg):
    say("FAIL: " + msg)
    sys.exit(1)


def wait_for_restart(host, before_uptime, limit=240):
    """Wait until the miner answers with a lower uptime than before."""
    t0 = time.time()
    while time.time() - t0 < limit:
        time.sleep(5)
        try:
            i = info(host)
        except (urllib.error.URLError, OSError, ValueError):
            continue
        if i["uptimeSeconds"] < before_uptime:
            return i
    fail(f"the miner did not come back within {limit} s")


def upload(host, image):
    data = Path(image).read_bytes()
    if len(data) < 4096 or data[0] != 0xE9:
        fail(f"{image} is not an ESP app image")
    up = info(host)["uptimeSeconds"]
    say(f"uploading {image} ({len(data)} bytes)")
    reply = call(host, "/api/system/OTA", "POST", data, timeout=180,
                 ctype="application/octet-stream")
    say(f"miner: {str(reply).strip()}")
    return wait_for_restart(host, up)


def click(host):
    say("checking for the latest release")
    call(host, "/api/v1/update/check", "POST", b"{}")
    for _ in range(30):
        time.sleep(2)
        u = call(host, "/api/v1/update")
        if u.get("latest") and u.get("phase") == "idle":
            break
    if not u.get("available"):
        fail(f"no newer release offered (installed {u.get('installed')}, "
             f"latest {(u.get('latest') or {}).get('tag')}): nothing to click")
    tag = u["latest"]["tag"]
    up = info(host)["uptimeSeconds"]
    say(f"installing {tag} by click")
    call(host, "/api/v1/update/install", "POST", b"{}")
    i = wait_for_restart(host, up, limit=420)
    rec = call(host, "/api/v1/update").get("last_install", "")
    if rec != f"{tag} verified":
        fail(f"install record says {rec!r}, expected '{tag} verified'")
    say(f"install record: {rec}")
    return i


def soak(host, minutes, expect):
    i = info(host)
    if expect and i.get("version") != f"warpaxe {expect}":
        fail(f"running {i.get('version')!r}, expected 'warpaxe {expect}'")
    if not i.get("updateCheck"):
        say("switching the automatic update check on")
        call(host, "/api/system", "PATCH", json.dumps({"updateCheck": True}).encode())
    boot_uptime = i["uptimeSeconds"]
    shares0 = i["sharesAccepted"]
    say(f"watching {i.get('version')} for {minutes} min "
        f"(uptime {boot_uptime} s, reset reason {i.get('resetReason')})")
    last_up, misses, checked = boot_uptime, 0, False
    end = time.time() + minutes * 60
    while time.time() < end:
        time.sleep(10)
        try:
            i = info(host)
            u = call(host, "/api/v1/update")
        except (urllib.error.URLError, OSError, ValueError):
            misses += 1
            if misses * 10 >= 60:
                fail("no answer for a minute")
            continue
        misses = 0
        if i["uptimeSeconds"] < last_up:
            fail(f"restarted (uptime {last_up} s -> {i['uptimeSeconds']} s, "
                 f"reset reason {i.get('resetReason')})")
        last_up = i["uptimeSeconds"]
        if u.get("checked_uptime_s", 0) > 0 and u.get("phase") == "idle" and not checked:
            checked = True
            say(f"update watcher checked at uptime {u['checked_uptime_s']} s: "
                f"latest {(u.get('latest') or {}).get('tag')}, error {u.get('error')!r}")
    if not checked:
        fail("the update watcher never ran a check")
    if i["hashRate_10m"] <= 0:
        fail("no hashrate")
    if i["sharesAccepted"] <= shares0:
        fail("no new shares")
    say(f"PASS: up {i['uptimeSeconds']} s, {i['hashRate_10m'] / 1000:.2f} TH/s, "
        f"{i['sharesAccepted'] - shares0} new shares, {i['sharesRejected']} rejected")


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("host")
    m = p.add_mutually_exclusive_group(required=True)
    m.add_argument("--upload", metavar="BIN")
    m.add_argument("--click", action="store_true")
    m.add_argument("--watch", action="store_true")
    p.add_argument("--minutes", type=float, default=5)
    p.add_argument("--expect", help="version the miner must run afterwards "
                   "(default for --upload: firmware/warpaxe/Cargo.toml)")
    p.add_argument("--no-record", action="store_true",
                   help="allow --upload on a miner without an install record")
    a = p.parse_args()

    u = call(a.host, "/api/v1/update")
    say(f"{a.host}: {info(a.host).get('version')}, "
        f"install record {u.get('install_record', u.get('last_install'))!r}")
    expect = a.expect
    if a.upload:
        # The record stays in flash across uploads (install_record; the
        # shown last_install only matches the image it was made for).
        # Without it the test misses what a click-installed miner goes
        # through. Firmware before 0.1.7 reports only last_install.
        if not (u.get("install_record") or u.get("last_install")) and not a.no_record:
            fail("no install record on this miner: click-install a release once "
                 "(--click), or pass --no-record to test without it")
        if not expect:
            toml = (ROOT / "firmware/warpaxe/Cargo.toml").read_text()
            expect = next(l.split('"')[1] for l in toml.splitlines() if l.startswith("version"))
        upload(a.host, a.upload)
    elif a.click:
        click(a.host)
    soak(a.host, a.minutes, expect)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Check the pinned CA bundle the firmware trusts (firmware/warpaxe/certificates).

Without arguments: the file matches its published checksum, holds 118 roots,
and none of the roots removed after the eMudhra incident. With --build-dir
(a finished firmware build, the esp-idf-sys `out` directory): the build used
only this file, and the trust store compiled into the app is exactly the one
ESP-IDF's generator makes from it.

Run from the repo root: python3 tools/check-ca-bundle.py [--build-dir DIR]
"""
import argparse
import hashlib
import re
import ssl
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FW = ROOT / "firmware/warpaxe"
BUNDLE = FW / "certificates/cacert-2026-10-20.pem"
ROOTS = 118
REMOVED = {"emSign ECC Root CA - G3", "AAA Certificate Services"}


def fail(why):
    print(f"check-ca-bundle: {why}", file=sys.stderr)
    sys.exit(1)


def check_file():
    want = BUNDLE.with_suffix(".pem.sha256").read_text().split()[0]
    if hashlib.sha256(BUNDLE.read_bytes()).hexdigest() != want:
        fail(f"{BUNDLE.name}: checksum does not match {BUNDLE.name}.sha256")
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.load_verify_locations(cafile=str(BUNDLE))
    n = ctx.cert_store_stats()["x509_ca"]
    if n != ROOTS:
        fail(f"{BUNDLE.name}: {n} roots, expected {ROOTS}")
    names = {v for c in ctx.get_ca_certs() for rdn in c["subject"] for k, v in rdn if k == "commonName"}
    if names & REMOVED:
        fail(f"{BUNDLE.name}: still holds {sorted(names & REMOVED)}")
    print(f"{BUNDLE.name}: checksum ok, {n} roots, removed roots absent")


def config(build):
    s = (build / "sdkconfig").read_text()
    on = set(re.findall(r"^(CONFIG_\w+)=y$", s, re.M))
    path = re.search(r'^CONFIG_MBEDTLS_CUSTOM_CERTIFICATE_BUNDLE_PATH="([^"]*)"$', s, re.M)
    return on, path.group(1) if path else None


def check_build(build):
    on, path = config(build)
    for k in ("CONFIG_MBEDTLS_CERTIFICATE_BUNDLE", "CONFIG_MBEDTLS_CERTIFICATE_BUNDLE_DEFAULT_NONE",
              "CONFIG_MBEDTLS_CUSTOM_CERTIFICATE_BUNDLE"):
        if k not in on:
            fail(f"build: {k} is not set (stale build directory? cargo clean -p esp-idf-sys)")
    for k in ("CONFIG_MBEDTLS_CERTIFICATE_BUNDLE_DEFAULT_FULL", "CONFIG_MBEDTLS_CERTIFICATE_BUNDLE_DEFAULT_CMN",
              "CONFIG_MBEDTLS_CERTIFICATE_BUNDLE_DEPRECATED_LIST"):
        if k in on:
            fail(f"build: {k} is set")
    if path is None or (build / path).resolve() != BUNDLE.resolve():
        fail(f"build: custom bundle path {path!r} does not lead to {BUNDLE.relative_to(ROOT)}")
    actual = build / "build/esp-idf/mbedtls/x509_crt_bundle"
    if not actual.is_file():
        fail(f"build: no {actual.relative_to(build)}")
    idf = next(Path.home().glob(".espressif/esp-idf/v5.3.*/components/mbedtls/esp_crt_bundle/gen_crt_bundle.py"), None)
    if idf is None:
        fail("build: ESP-IDF's gen_crt_bundle.py not found under ~/.espressif")
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run([sys.executable, str(idf), "--input", str(BUNDLE), "-q"], cwd=tmp, check=True)
        expected = (Path(tmp) / "x509_crt_bundle").read_bytes()
    if actual.read_bytes() != expected:
        fail("build: the compiled trust store is not the pinned bundle alone")
    print(f"build: trust store is the pinned bundle alone ({len(expected)} bytes)")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--build-dir", type=Path, help="esp-idf-sys out directory of a firmware build")
    a = ap.parse_args()
    check_file()
    if a.build_dir:
        check_build(a.build_dir)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Check the pinned CA bundle the firmware trusts (firmware/warpaxe/certificates).

Without arguments: curl's extract matches its published checksum, the bundle
the firmware trusts (ca-bundle.pem) is that extract minus the roots in DROPPED,
it holds 117 roots and none of the eMudhra roots. With --write: regenerate
ca-bundle.pem from the extract. With --build-dir
(a finished firmware build, the esp-idf-sys `out` directory): the build used
only this file, and the trust store compiled into the app is exactly the one
ESP-IDF's generator makes from it.

Run from the repo root: python3 tools/check-ca-bundle.py [--write] [--build-dir DIR]
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
UPSTREAM = FW / "certificates/cacert-2026-10-20.pem"
BUNDLE = FW / "certificates/ca-bundle.pem"
ROOTS = 117
# Not in curl's extract of 2026-10-20 (Mozilla removed them).
REMOVED = {"emSign ECC Root CA - G3", "AAA Certificate Services"}
# Still in the extract, left out by WarpAxe: the last eMudhra root, as
# ForgeOS v1.8 and ESP-Miner do (certificates/README.md).
DROPPED = {"emSign Root CA - G1"}
EMUDHRA = re.compile(r"emSign|eMudhra", re.I)


def derive():
    """curl's extract without the blocks named in DROPPED, byte for byte otherwise."""
    text = UPSTREAM.read_text()
    blocks = text.split("\n\n")
    kept = [b for b in blocks if b.split("\n", 1)[0] not in DROPPED]
    if len(blocks) - len(kept) != len(DROPPED):
        fail(f"{UPSTREAM.name}: expected to drop {sorted(DROPPED)}, dropped {len(blocks) - len(kept)} blocks")
    return "\n\n".join(kept)


def fail(why):
    print(f"check-ca-bundle: {why}", file=sys.stderr)
    sys.exit(1)


def check_file():
    want = UPSTREAM.with_suffix(".pem.sha256").read_text().split()[0]
    if hashlib.sha256(UPSTREAM.read_bytes()).hexdigest() != want:
        fail(f"{UPSTREAM.name}: checksum does not match {UPSTREAM.name}.sha256")
    if BUNDLE.read_text() != derive():
        fail(f"{BUNDLE.name} is not {UPSTREAM.name} minus {sorted(DROPPED)}: run with --write")
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.load_verify_locations(cafile=str(BUNDLE))
    n = ctx.cert_store_stats()["x509_ca"]
    if n != ROOTS:
        fail(f"{BUNDLE.name}: {n} roots, expected {ROOTS}")
    names = {v for c in ctx.get_ca_certs() for rdn in c["subject"] for k, v in rdn if k == "commonName"}
    bad = {c for c in names if c in REMOVED | DROPPED or EMUDHRA.search(c)}
    orgs = {v for c in ctx.get_ca_certs() for rdn in c["subject"] for k, v in rdn if k == "organizationName"}
    bad |= {o for o in orgs if EMUDHRA.search(o)}
    if bad:
        fail(f"{BUNDLE.name}: still holds {sorted(bad)}")
    print(f"{BUNDLE.name}: {UPSTREAM.name} checksum ok, {n} roots, no eMudhra root")


def config(build):
    s = (build / "sdkconfig").read_text()
    on = set(re.findall(r"^(CONFIG_\w+)=y$", s, re.M))
    path = re.search(r'^CONFIG_MBEDTLS_CUSTOM_CERTIFICATE_BUNDLE_PATH="([^"]*)"$', s, re.M)
    return on, path.group(1) if path else None


def check_build(build):
    on, path = config(build)
    for k in ("CONFIG_MBEDTLS_CERTIFICATE_BUNDLE", "CONFIG_MBEDTLS_CERTIFICATE_BUNDLE_DEFAULT_NONE",
              "CONFIG_MBEDTLS_CUSTOM_CERTIFICATE_BUNDLE", "CONFIG_MBEDTLS_HAVE_TIME_DATE"):
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
    fixture = ROOT / "crates/trust/tests/data/x509_crt_bundle"
    if fixture.read_bytes() != expected:
        fail(f"{fixture.relative_to(ROOT)} is not the bundle of {BUNDLE.name}: regenerate it (certificates/README.md)")
    print(f"build: trust store is the pinned bundle alone ({len(expected)} bytes)")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--write", action="store_true", help=f"regenerate {BUNDLE.name} from {UPSTREAM.name}")
    ap.add_argument("--build-dir", type=Path, help="esp-idf-sys out directory of a firmware build")
    a = ap.parse_args()
    if a.write:
        BUNDLE.write_text(derive())
    check_file()
    if a.build_dir:
        check_build(a.build_dir)


if __name__ == "__main__":
    main()

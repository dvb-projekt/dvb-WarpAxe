# Pinned CA bundle

The firmware verifies TLS servers (Stratum over TLS, the online update from
git.warppool.org, the price fetch from mempool.space) against one bundle
only, not against the lists ESP-IDF ships.

- `cacert-2026-10-20.pem`: curl's extract of the Mozilla root store of
  20 October 2026, 118 roots, unchanged.
- Source: https://curl.se/ca/cacert-2026-10-20.pem, checksum from
  https://curl.se/ca/cacert-2026-10-20.pem.sha256 (kept next to it).
- Licence: Mozilla Public License 2.0, as stated by curl
  (https://curl.se/docs/caextract.html).
- `ca-bundle.pem`: what the firmware trusts, the extract minus
  `emSign Root CA - G1` (117 roots), written by
  `python3 tools/check-ca-bundle.py --write`; the check fails when it is
  anything else.

Why: after the eMudhra incident (intermediate CA keys compromised, October
2026) this extract no longer carries `emSign ECC Root CA - G3` and leaves out
`AAA Certificate Services`. ESP-IDF v5.3.3's full list still has both
(`Comodo AAA Services root` there). A server whose only chain runs to one of
the removed roots can no longer be reached; its operator has to serve a chain
to a root that remains. The same step was taken in ESP-Miner (bitaxeorg PR
#2040) and in the NerdQAxe+ firmware v1.1.1.

Since v0.1.41 WarpAxe also leaves out `emSign Root CA - G1`, the one
eMudhra root Mozilla kept, as ForgeOS v1.8 removes every eMudhra root. No
pool, update or price server the firmware talks to chains to it.

Dates: `CONFIG_MBEDTLS_HAVE_TIME_DATE` is on since v0.1.41, so mbedTLS
checks each certificate's validity window. The verify callbacks drop
those faults again while NTP has not set the clock this boot
(`warpaxe_trust::clock_checked`, `firmware/warpaxe/src/tls_trust.rs`);
`check-ca-bundle.py --build-dir` checks that the option is on.

`sdkconfig.defaults` turns the ESP-IDF lists off (`DEFAULT_NONE`, no
deprecated list) and the custom bundle on. A custom bundle is added to the
default lists, not put in their place, so both settings are needed.

`tools/check-ca-bundle.py` checks the files (curl's checksum,
`ca-bundle.pem` derived from it, root count, no eMudhra root) in CI; with `--build-dir` it also checks a finished
firmware build, as `tools/release-firmware.sh` does before every release.

The compact form of this file, as ESP-IDF's `gen_crt_bundle.py` writes
it, is also a test fixture: `crates/trust/tests/data/x509_crt_bundle`
(the firmware trusts a root of the bundle that a server sends along,
cross-signed by a root the bundle no longer has, see `crates/trust`).
`check-ca-bundle.py --build-dir` checks that it matches the build.

On an ESP-IDF upgrade: check that `esp_crt_verify_callback` in
`components/mbedtls/esp_crt_bundle/esp_crt_bundle.c` keeps its signature
(`firmware/warpaxe/src/tls_trust.rs` declares and calls it).

To move to a newer extract: put the new `.pem` and `.pem.sha256` here, change
the file name in the check, run it with `--write`, regenerate the
fixture (`python3 $IDF_PATH/components/mbedtls/esp_crt_bundle/gen_crt_bundle.py
--input ../../../../firmware/warpaxe/certificates/ca-bundle.pem -q` in
`crates/trust/tests/data`), and build clean
(`cargo clean -p esp-idf-sys`): ESP-IDF does not pick up a changed sdkconfig
default in an existing build directory.

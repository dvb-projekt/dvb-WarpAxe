# Pinned CA bundle

The firmware verifies TLS servers (Stratum over TLS, the online update from
git.warppool.org, the price fetch from mempool.space) against this file only,
not against the lists ESP-IDF ships.

- File: `cacert-2026-10-20.pem`, curl's extract of the Mozilla root store
  of 20 October 2026, 118 roots.
- Source: https://curl.se/ca/cacert-2026-10-20.pem, checksum from
  https://curl.se/ca/cacert-2026-10-20.pem.sha256 (kept next to it).
- Licence: Mozilla Public License 2.0, as stated by curl
  (https://curl.se/docs/caextract.html).

Why: after the eMudhra incident (intermediate CA keys compromised, October
2026) this extract no longer carries `emSign ECC Root CA - G3` and leaves out
`AAA Certificate Services`. ESP-IDF v5.3.3's full list still has both
(`Comodo AAA Services root` there). A server whose only chain runs to one of
the removed roots can no longer be reached; its operator has to serve a chain
to a root that remains. The same step was taken in ESP-Miner (bitaxeorg PR
#2040) and in the NerdQAxe+ firmware v1.1.1.

`sdkconfig.defaults` turns the ESP-IDF lists off (`DEFAULT_NONE`, no
deprecated list) and the custom bundle on. A custom bundle is added to the
default lists, not put in their place, so both settings are needed.

`tools/check-ca-bundle.py` checks the file (checksum, root count, the two
removed roots absent) in CI; with `--build-dir` it also checks a finished
firmware build, as `tools/release-firmware.sh` does before every release.

To move to a newer extract: put the new `.pem` and `.pem.sha256` here, change
the file name in `sdkconfig.defaults` and in the check, and build clean
(`cargo clean -p esp-idf-sys`): ESP-IDF does not pick up a changed sdkconfig
default in an existing build directory.

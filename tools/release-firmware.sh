#!/usr/bin/env bash
# Publish a WarpAxe firmware release on Forgejo: the app image every board
# runs (warpaxe.bin), its SHA-256 (warpaxe.bin.sha256) and its signature
# (warpaxe.bin.sig, BIP-340 Schnorr over the SHA-256), which the miners'
# online update fetches and checks against the key built into the
# firmware (RELEASE_KEY in firmware/warpaxe/src/update.rs).
#
#   tools/release-firmware.sh v0.2.0 [notes-file]
#
# The version must match firmware/warpaxe/Cargo.toml; the tag goes on the
# current commit, which must be pushed (main or a release branch) and green.
# Token: ~/.config/warppool/forgejo-admin-token; signing key:
# ~/.config/warppool/warpaxe-release.key (made once with
# `cargo run -p warpaxe-sv2 --example release-key -- keygen <file>`).
# Neither is ever printed. Before publishing, the image is uploaded to the
# test miner SOAK_HOST (default .41) and watched for 5 min
# (tools/soak-test.py); announce that to anyone else using the miner.
#
# A fourth asset, tested-on.txt, lists the board profiles the image ran
# on (one id per line). Only the T-Display-S3 reads it: the devkit is
# offered a release only when its own id is in the file (firmware
# update.rs). TESTED_ON="nerdqaxe-plus-plus bitaxe-gamma" names the boards
# of your own soak; t-display-s3 must not be written by hand, it is added
# only when SOAK_DEVKIT=<host> is set and the soak on that devkit passed
# in this run. Without the file no devkit installs the release.
set -euo pipefail

TAG="${1:?usage: $0 vX.Y.Z [notes-file]}"
NOTES_FILE="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW="$ROOT/firmware/warpaxe"
API="https://git.warppool.org/api/v1/repos/dvb-projekt/dvb-WarpAxe"
TOKEN="$(cat ~/.config/warppool/forgejo-admin-token)"
KEY="$HOME/.config/warppool/warpaxe-release.key"
[ -r "$KEY" ] || { echo "no signing key at $KEY" >&2; exit 1; }
OUT="$ROOT/target/release-firmware"

VERSION="$(grep -m1 '^version' "$FW/Cargo.toml" | cut -d'"' -f2)"
[ "v$VERSION" = "$TAG" ] || { echo "tag $TAG does not match firmware version $VERSION" >&2; exit 1; }
[ -z "$(git -C "$ROOT" status --porcelain)" ] || { echo "working tree not clean" >&2; exit 1; }
COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
git -C "$ROOT" fetch -q origin
# The tag goes on a pushed commit: main, or a release branch when main
# already carries work that must not ship yet.
[ -n "$(git -C "$ROOT" branch -r --contains "$COMMIT")" ] || { echo "HEAD is not pushed to any branch" >&2; exit 1; }

(cd "$FW" && cargo build --release)
mkdir -p "$OUT"
espflash save-image --chip esp32s3 --flash-size 16mb \
    "$FW/target/xtensa-esp32s3-espidf/release/warpaxe" "$OUT/warpaxe.bin" >/dev/null
# Room for the NerdMiner import: a T-Display-S3 that ran NerdMiner keeps
# its config.json (wallet, pool) in NerdMiner's SPIFFS partition, flash
# 0x310000..0x3F0000: the first 64 KB in the tail of WarpAxe's ota_0
# (0x20000 + 0x300000), the rest in the head of ota_1. The first install
# writes only ota_0, so the file survives it as long as the app ends
# below 0x310000, i.e. is at most 0x2F0000 bytes; the first start reads it
# from there (src/cpuminer.rs import_nerdminer). A later online update
# writes ota_1 over the rest of it, which is fine: the import is done.
# The gate binds every board's release to that window. Should it bite one
# day, the plan is to give up the import (the devkit's first start then
# asks for Wi-Fi and wallet like any board) rather than hold a release
# for the boards with chips; Oliver decides then. The headroom is printed
# so the day does not come as a surprise.
APP_BYTES=$(wc -c < "$OUT/warpaxe.bin")
HEADROOM=$((0x2F0000 - APP_BYTES))
echo "import window headroom: $HEADROOM bytes below 0x2F0000 (app $APP_BYTES bytes)"
if [ "$HEADROOM" -lt 0 ]; then
    echo "warpaxe.bin is $APP_BYTES bytes, more than 0x2F0000: it would overwrite a NerdMiner's config.json on the first install (see the comment above)" >&2; exit 1
elif [ "$HEADROOM" -lt $((256 * 1024)) ]; then
    echo "warning: less than 256 KB left below the import window" >&2
fi
# The first install from a stock firmware, over USB: the parts for the web
# flasher (bootloader, partition table, empty OTA data so the new table
# boots ota_0, the app) and one merged image for esptool users. The web
# flasher skips the NVS partition, so AxeOS/NerdOS settings survive for the
# first-start import; the merged image covers it and starts clean.
IDF_OUT="$(ls -d "$FW"/target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*/out | head -1)"
cp "$IDF_OUT/build/bootloader/bootloader.bin" "$OUT/bootloader.bin"
cp "$IDF_OUT/build/partition_table/partition-table.bin" "$OUT/partition-table.bin"
cp "$IDF_OUT/build/ota_data_initial.bin" "$OUT/ota-data-initial.bin"
python3 -m esptool --chip esp32s3 merge_bin -o "$OUT/warpaxe-factory.bin" --flash_size 16MB \
    0x0 "$OUT/bootloader.bin" 0x8000 "$OUT/partition-table.bin" \
    0xf000 "$OUT/ota-data-initial.bin" 0x20000 "$OUT/warpaxe.bin" >/dev/null
python3 - "$TAG" "$OUT/manifest.json" <<'PY'
import json, sys
tag, out = sys.argv[1], sys.argv[2]
json.dump({
    "name": "WarpAxe",
    "version": tag.lstrip("v"),
    "new_install_prompt_erase": True,
    "builds": [{"chipFamily": "ESP32-S3", "parts": [
        {"path": "bootloader.bin", "offset": 0x0},
        {"path": "partition-table.bin", "offset": 0x8000},
        {"path": "ota-data-initial.bin", "offset": 0xF000},
        {"path": "warpaxe.bin", "offset": 0x20000},
    ]}],
}, open(out, "w"), indent=2)
PY
(cd "$OUT" && shasum -a 256 warpaxe.bin > warpaxe.bin.sha256)
(cd "$ROOT" && cargo run -q -p warpaxe-sv2 --example release-key -- sign "$KEY" "$OUT/warpaxe.bin" > "$OUT/warpaxe.bin.sig")
# The key in the firmware must be the one that signed, or every miner
# refuses the release.
PUB="$(cd "$ROOT" && cargo run -q -p warpaxe-sv2 --example release-key -- pubkey "$KEY")"
grep -A 5 'const RELEASE_KEY' "$FW/src/update.rs" | tr -d ' ,\n' | sed 's/0x//g' | grep -q "$PUB" \
    || { echo "RELEASE_KEY in update.rs is not the public key of $KEY" >&2; exit 1; }
(cd "$ROOT" && cargo run -q -p warpaxe-sv2 --example release-key -- verify "$PUB" "$OUT/warpaxe.bin" "$OUT/warpaxe.bin.sig" >/dev/null)
echo "image: $(wc -c < "$OUT/warpaxe.bin") bytes, sha256 $(cut -d' ' -f1 "$OUT/warpaxe.bin.sha256")"

# The web flasher as one static directory (page, esptool-js, manifest,
# parts, the signed image). warppool.org/warpaxe/ unpacks it on deploy
# after checking warpaxe.bin with tools/verify-release.py against the
# pinned key.
FL="$OUT/web-flasher"
rm -rf "$FL" && mkdir -p "$FL"
cp -R "$ROOT/web/flasher/." "$FL/"
cp "$FW/src/ui/assets/wordmark.webp" "$FW/src/ui/assets/favicon.svg" "$FL/"
cp "$OUT"/{manifest.json,bootloader.bin,partition-table.bin,ota-data-initial.bin,warpaxe.bin,warpaxe.bin.sha256,warpaxe.bin.sig} "$FL/"
tar -czf "$OUT/web-flasher.tar.gz" -C "$FL" .
(cd "$OUT" && shasum -a 256 warpaxe-factory.bin web-flasher.tar.gz > install.sha256)

# Nothing goes out before the exact image has run on a test miner the way
# users run it: with an install record in flash and the update watcher on
# (0.1.5 passed every bench test and restarted once a minute in the field).
# SOAK_HOST picks the miner; SKIP_SOAK=1 only for a release that cannot
# run on it, and say so in the notes.
if [ "${SKIP_SOAK:-0}" != 1 ]; then
    "$ROOT/tools/soak-test.py" "${SOAK_HOST:-192.168.178.41}" --upload "$OUT/warpaxe.bin" \
        --minutes "${SOAK_MINUTES:-5}" || { echo "soak test failed: not published" >&2; exit 1; }
fi
# The boards the release ran on (see the header). The devkit's id comes
# only from a passed soak on a devkit in this run, never from TESTED_ON.
TESTED="${TESTED_ON:-}"
case " $TESTED " in *" t-display-s3 "*)
    echo "TESTED_ON must not name t-display-s3 by hand: set SOAK_DEVKIT=<host> and let the soak add it" >&2; exit 1;;
esac
if [ -n "${SOAK_DEVKIT:-}" ]; then
    # A devkit flashed over USB has no install record (that record is
    # what the soak on SOAK_HOST covers), so --no-record is set here.
    "$ROOT/tools/soak-test.py" "$SOAK_DEVKIT" --upload "$OUT/warpaxe.bin" --no-record \
        --minutes "${SOAK_MINUTES:-5}" || { echo "devkit soak failed: not published" >&2; exit 1; }
    TESTED="$TESTED t-display-s3"
fi
ASSETS="warpaxe.bin warpaxe.bin.sha256 warpaxe.bin.sig warpaxe-factory.bin web-flasher.tar.gz install.sha256"
if [ -n "${TESTED// /}" ]; then
    printf '%s\n' $TESTED > "$OUT/tested-on.txt"
    ASSETS="$ASSETS tested-on.txt"
    echo "tested-on.txt: $TESTED"
else
    echo "no TESTED_ON and no SOAK_DEVKIT: the release carries no tested-on.txt, no devkit will install it" >&2
fi

# PUBLISH_GATE=<file>: hold here, after the soaks, until the release is
# cleared by hand: the run publishes once <file> exists and stops without
# publishing once <file>.abort does (Oliver, 2026-10-20: the go-ahead to
# publish comes after the soaks, separately).
if [ -n "${PUBLISH_GATE:-}" ]; then
    echo "soaks passed: waiting for $PUBLISH_GATE to publish ($PUBLISH_GATE.abort stops)"
    until [ -e "$PUBLISH_GATE" ]; do
        [ -e "$PUBLISH_GATE.abort" ] && { echo "stopped before publishing: not published" >&2; exit 1; }
        sleep 5
    done
fi

BODY="WarpAxe $TAG"
[ -n "$NOTES_FILE" ] && BODY="$(cat "$NOTES_FILE")"
REL="$(python3 -c 'import json,sys; print(json.dumps({"tag_name":sys.argv[1],"target_commitish":sys.argv[2],"name":"WarpAxe "+sys.argv[1],"body":sys.argv[3],"draft":False,"prerelease":False}))' "$TAG" "$COMMIT" "$BODY")"
ID="$(curl -sf -X POST -H "Authorization: token $TOKEN" -H 'Content-Type: application/json' \
    -d "$REL" "$API/releases" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
for f in $ASSETS; do
    curl -sf -X POST -H "Authorization: token $TOKEN" \
        -F "attachment=@$OUT/$f" "$API/releases/$ID/assets?name=$f" >/dev/null
done
echo "release $TAG (id $ID) published: update (warpaxe.bin + .sha256 + .sig), first-install parts, tested-on.txt (${TESTED:-none})"

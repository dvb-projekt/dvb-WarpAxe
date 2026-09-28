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

BODY="WarpAxe $TAG"
[ -n "$NOTES_FILE" ] && BODY="$(cat "$NOTES_FILE")"
REL="$(python3 -c 'import json,sys; print(json.dumps({"tag_name":sys.argv[1],"target_commitish":sys.argv[2],"name":"WarpAxe "+sys.argv[1],"body":sys.argv[3],"draft":False,"prerelease":False}))' "$TAG" "$COMMIT" "$BODY")"
ID="$(curl -sf -X POST -H "Authorization: token $TOKEN" -H 'Content-Type: application/json' \
    -d "$REL" "$API/releases" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
for f in warpaxe.bin warpaxe.bin.sha256 warpaxe.bin.sig warpaxe-factory.bin \
    web-flasher.tar.gz install.sha256; do
    curl -sf -X POST -H "Authorization: token $TOKEN" \
        -F "attachment=@$OUT/$f" "$API/releases/$ID/assets?name=$f" >/dev/null
done
echo "release $TAG (id $ID) published: update (warpaxe.bin + .sha256 + .sig) and first-install parts"

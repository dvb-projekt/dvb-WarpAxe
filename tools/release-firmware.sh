#!/usr/bin/env bash
# Publish a WarpAxe firmware release on Forgejo: the app image every board
# runs (warpaxe.bin) and its SHA-256 (warpaxe.bin.sha256), which the
# miners' online update fetches and checks.
#
#   tools/release-firmware.sh v0.2.0 [notes-file]
#
# The version must match firmware/warpaxe/Cargo.toml; the tag goes on the
# current commit, which must be pushed and green. Token:
# ~/.config/warppool/forgejo-admin-token (never printed).
set -euo pipefail

TAG="${1:?usage: $0 vX.Y.Z [notes-file]}"
NOTES_FILE="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FW="$ROOT/firmware/warpaxe"
API="https://git.warppool.org/api/v1/repos/dvb-projekt/dvb-WarpAxe"
TOKEN="$(cat ~/.config/warppool/forgejo-admin-token)"
OUT="$ROOT/target/release-firmware"

VERSION="$(grep -m1 '^version' "$FW/Cargo.toml" | cut -d'"' -f2)"
[ "v$VERSION" = "$TAG" ] || { echo "tag $TAG does not match firmware version $VERSION" >&2; exit 1; }
[ -z "$(git -C "$ROOT" status --porcelain)" ] || { echo "working tree not clean" >&2; exit 1; }
COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
git -C "$ROOT" fetch -q origin
[ "$(git -C "$ROOT" rev-parse origin/main)" = "$COMMIT" ] || { echo "HEAD is not origin/main" >&2; exit 1; }

(cd "$FW" && cargo build --release)
mkdir -p "$OUT"
espflash save-image --chip esp32s3 --flash-size 16mb \
    "$FW/target/xtensa-esp32s3-espidf/release/warpaxe" "$OUT/warpaxe.bin" >/dev/null
(cd "$OUT" && shasum -a 256 warpaxe.bin > warpaxe.bin.sha256)
echo "image: $(wc -c < "$OUT/warpaxe.bin") bytes, sha256 $(cut -d' ' -f1 "$OUT/warpaxe.bin.sha256")"

BODY="WarpAxe $TAG"
[ -n "$NOTES_FILE" ] && BODY="$(cat "$NOTES_FILE")"
REL="$(python3 -c 'import json,sys; print(json.dumps({"tag_name":sys.argv[1],"target_commitish":sys.argv[2],"name":"WarpAxe "+sys.argv[1],"body":sys.argv[3],"draft":False,"prerelease":False}))' "$TAG" "$COMMIT" "$BODY")"
ID="$(curl -sf -X POST -H "Authorization: token $TOKEN" -H 'Content-Type: application/json' \
    -d "$REL" "$API/releases" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
for f in warpaxe.bin warpaxe.bin.sha256; do
    curl -sf -X POST -H "Authorization: token $TOKEN" \
        -F "attachment=@$OUT/$f" "$API/releases/$ID/assets?name=$f" >/dev/null
done
echo "release $TAG (id $ID) published with warpaxe.bin + warpaxe.bin.sha256"

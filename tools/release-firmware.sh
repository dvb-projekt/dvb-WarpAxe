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
# Neither is ever printed.
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
(cd "$OUT" && shasum -a 256 warpaxe.bin > warpaxe.bin.sha256)
(cd "$ROOT" && cargo run -q -p warpaxe-sv2 --example release-key -- sign "$KEY" "$OUT/warpaxe.bin" > "$OUT/warpaxe.bin.sig")
# The key in the firmware must be the one that signed, or every miner
# refuses the release.
PUB="$(cd "$ROOT" && cargo run -q -p warpaxe-sv2 --example release-key -- pubkey "$KEY")"
grep -A 5 'const RELEASE_KEY' "$FW/src/update.rs" | tr -d ' ,\n' | sed 's/0x//g' | grep -q "$PUB" \
    || { echo "RELEASE_KEY in update.rs is not the public key of $KEY" >&2; exit 1; }
(cd "$ROOT" && cargo run -q -p warpaxe-sv2 --example release-key -- verify "$PUB" "$OUT/warpaxe.bin" "$OUT/warpaxe.bin.sig" >/dev/null)
echo "image: $(wc -c < "$OUT/warpaxe.bin") bytes, sha256 $(cut -d' ' -f1 "$OUT/warpaxe.bin.sha256")"

BODY="WarpAxe $TAG"
[ -n "$NOTES_FILE" ] && BODY="$(cat "$NOTES_FILE")"
REL="$(python3 -c 'import json,sys; print(json.dumps({"tag_name":sys.argv[1],"target_commitish":sys.argv[2],"name":"WarpAxe "+sys.argv[1],"body":sys.argv[3],"draft":False,"prerelease":False}))' "$TAG" "$COMMIT" "$BODY")"
ID="$(curl -sf -X POST -H "Authorization: token $TOKEN" -H 'Content-Type: application/json' \
    -d "$REL" "$API/releases" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
for f in warpaxe.bin warpaxe.bin.sha256 warpaxe.bin.sig; do
    curl -sf -X POST -H "Authorization: token $TOKEN" \
        -F "attachment=@$OUT/$f" "$API/releases/$ID/assets?name=$f" >/dev/null
done
echo "release $TAG (id $ID) published with warpaxe.bin + .sha256 + .sig"

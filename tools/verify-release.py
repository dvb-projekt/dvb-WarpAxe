#!/usr/bin/env python3
"""Check a WarpAxe release image against its signature, without Rust.

  tools/verify-release.py <pubkey-hex> warpaxe.bin warpaxe.bin.sig

The signature is BIP-340 Schnorr over the image's SHA-256, made by
tools/release-firmware.sh; the public key is RELEASE_KEY in
firmware/warpaxe/src/update.rs. Exit 0 and "ok: sha256 <hex>" when it
holds, exit 1 otherwise. For deploy steps that publish the image (the web
flasher): pin the key in the step, never take it from the download.

The verification follows the BIP-340 reference code (pure Python, slow but
a single check takes milliseconds).
"""
import hashlib
import sys

P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


def tagged_hash(tag, msg):
    t = hashlib.sha256(tag.encode()).digest()
    return hashlib.sha256(t + t + msg).digest()


def add(p1, p2):
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    if p1[0] == p2[0] and p1[1] != p2[1]:
        return None
    if p1 == p2:
        lam = 3 * p1[0] * p1[0] * pow(2 * p1[1], P - 2, P) % P
    else:
        lam = (p2[1] - p1[1]) * pow(p2[0] - p1[0], P - 2, P) % P
    x = (lam * lam - p1[0] - p2[0]) % P
    return (x, (lam * (p1[0] - x) - p1[1]) % P)


def mul(p, n):
    r = None
    for i in range(256):
        if (n >> i) & 1:
            r = add(r, p)
        p = add(p, p)
    return r


def lift_x(x):
    if x >= P:
        return None
    y_sq = (pow(x, 3, P) + 7) % P
    y = pow(y_sq, (P + 1) // 4, P)
    if pow(y, 2, P) != y_sq:
        return None
    return (x, y if y % 2 == 0 else P - y)


def verify(pubkey, msg, sig):
    pt = lift_x(int.from_bytes(pubkey, "big"))
    r = int.from_bytes(sig[:32], "big")
    s = int.from_bytes(sig[32:], "big")
    if pt is None or r >= P or s >= N:
        return False
    e = int.from_bytes(tagged_hash("BIP0340/challenge", sig[:32] + pubkey + msg), "big") % N
    rr = add(mul(G, s), mul(pt, N - e))
    return rr is not None and rr[1] % 2 == 0 and rr[0] == r


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__.strip().splitlines()[2].strip())
    pubkey = bytes.fromhex(sys.argv[1].strip())
    digest = hashlib.sha256(open(sys.argv[2], "rb").read()).digest()
    sig = bytes.fromhex(open(sys.argv[3]).read().strip())
    if len(pubkey) != 32 or len(sig) != 64 or not verify(pubkey, digest, sig):
        print("signature does not match", file=sys.stderr)
        sys.exit(1)
    print(f"ok: sha256 {digest.hex()}")


if __name__ == "__main__":
    main()

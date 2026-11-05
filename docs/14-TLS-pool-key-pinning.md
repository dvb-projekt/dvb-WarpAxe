# A pool on its own certificate: pinning the pool's key, with a click

Oliver set the NerdAxe Gaia to Stratum V1 over TLS against WarpPool on
his Umbrel (port 3334). Every connection failed and the Gaia fell back to
the fallback pool. WarpPool's certificate there is self-signed (rcgen, CN
"rcgen self signed cert", SAN dvb-warppool, dvb-warppool.local,
umbrel.local, localhost, no IP), and `link.rs` checked only against the
pinned CA bundle and the host name.

Oliver's decision (2026-11-05): pin the fingerprint of the pool's key,
confirmed by a click; not a custom-CA upload as the first answer.

## The rule

1. **Only as the fallback of the check.** Every TLS pool connection is
   first checked as before: the chain against the CA bundle (plus the
   sent-root rule of v0.1.40), the name against the host. Only when
   mbedTLS leaves the *leaf* certificate "not trusted" is the stored pin
   consulted. A public pool with a real certificate, served with its
   chain, never meets the pin, also not when its certificate rotates
   every 90 days. (A pool that leaves its intermediate out of the chain
   is refused like a self-signed one and its key offered; the texts say
   "not verifiable against a public authority" in the log, and the Pool
   page asks the owner to compare the fingerprint before the click.)
2. **No silent take-over.** A refused leaf's key is remembered as
   "pending", in RAM only, per pool slot (main, fallback). The Pool page
   shows "Certificate not from a public authority", the fingerprint, and
   a button. Only the click stores the pin, as a config field of that
   slot (`stratumTLSPin`, `fallbackStratumTLSPin`); a saved pool (up to
   eight) carries the pin along with its entry.
3. **The pin is the public key**, SHA-256 over the DER
   SubjectPublicKeyInfo of the leaf, 32 bytes. With a matching pin the
   host name is not checked, so the pool's IP address works. A renewed
   certificate with the same key keeps the pin (test in `crates/trust`).
   A "Forget" button drops it.
4. **A changed key of a pool on its own certificate never connects.**
   The session fails with a clear
   message ("the pool's key changed: … is not the trusted key"), nothing
   is taken over, and the new key appears as pending with its own
   confirmation ("Trust the new key"), the old one shown beside it.
5. Texts in eight languages, reviewed by a native-speaker agent.

## Format (agreed with WarpPool, fixed)

- Hex, upper case, colons: `AB:CD:…:EF`, 95 characters. Equal to
  `openssl x509 -pubkey -noout | openssl pkey -pubin -outform der |
  openssl dgst -sha256 -c` (which prints lower case). Compared without
  regard to case; the API accepts the written form in any case, with or
  without colons, and stores the canonical one.
- UI: bold short form of the first and last four bytes, `AB:CD:EF:01 …
  23:45:67:89`; below, the full value in monospace, four lines of eight.
- API: the full string, no line breaks.
- WarpPool shows the same value in its Connect window from its next
  release (`/api/overview` → `connect.tls_spki_sha256`).
- Oliver's Umbrel (192.168.178.3:3334), measured from the Mac on
  2026-11-05 and kept as a test vector:
  `15:1A:63:B8:21:17:9C:A4:60:E8:E9:C5:BC:66:9F:F4:2F:00:C8:7B:EE:E3:0F:06:35:06:26:BD:09:9E:E0:47`.

## Where it lives

- `crates/trust/src/pin.rs`: the pin, its written form, the parse, and
  `verdict(depth, flags, pin, spki)` → Untouched / Pinned / Unconfirmed /
  Changed. No I/O, no unsafe, host-tested (`crates/trust/tests/pin.rs`:
  two openssl certificates from one key, one from another key, the
  Umbrel's real certificate; 12 tests).
- `crates/trust/src/x509.rs`: a minimal DER walk to the subject and the
  SubjectPublicKeyInfo, for the tests (the firmware gets both from
  mbedTLS as `subject_raw` and `pk_raw`).
- `firmware/warpaxe/src/tls_trust.rs`: `attach_pool` installs
  `verify_pool` (the order is under "Order in the callback" below).
  `pool_begin(pin)` before the handshake,
  `pool_outcome()` after it. `attach` (update, price fetch) is unchanged
  and never sees a pin.
- `firmware/warpaxe/src/link.rs`: `Link::connect(host, port, tls, pin)`;
  a refusal for the key alone is the error `KeyRefused(seen)`.
- `firmware/warpaxe/src/stratum.rs`: the pin per slot from the config,
  `TLS_PENDING` per slot (RAM), `tls_pending(fallback)` and
  `clear_tls_pending(fallback)`; `StratumState::tls_pinned` for the
  running session.
- `firmware/warpaxe/src/config.rs`: the two config fields, `PoolProfile::pin`
  (stored only when set; `as_patch` clears the main pool's pin when a
  saved pool without one is used).
- `firmware/warpaxe/src/api.rs`: `pool_refuse` validates and normalises a
  pin in a PATCH; `forget_pending_keys` drops a slot's pending key when
  its URL, port, TLS switch, protocol or pin is patched.
- Web UI: `pinHtml(fb, info)` under each TLS switch on the Pool page;
  actions `pintrust:<main|fallback>` and `pinforget:<main|fallback>`.

## Per-connection context

The mbedTLS verify callback has no per-connection parameter in esp-tls
(the attach function is a bare pointer, `p_vrfy` is null). The pool has
its own attach function (`attach_pool`) and a static slot (`POOL`) that
the stratum thread fills before the handshake and reads after it. One
pool handshake runs at a time (the stratum thread's; the primary probe
is plain TCP, Sv2 is Noise), so one slot is enough. The update and the
price fetch keep `attach`, so the pin cannot reach them. No change to
the vendored esp-idf-svc was needed.

## The flags decision

The pin covers a leaf whose flags are "not trusted" plus any of
CN_MISMATCH, EXPIRED, FUTURE, BAD_MD:

- CN_MISMATCH: the point of the pin (an IP address as the pool address;
  rcgen's CN is not the host).
- EXPIRED / FUTURE: a self-signed certificate's dates were written by
  the holder of the pinned key, so they say nothing a public PKI would.
  Refusing them would make the connection depend on the miner's clock
  (a boot before NTP starts in 1970, before any `notBefore`) with no gain
  in safety: whoever holds the key can issue a certificate with any
  dates. The same reasoning as for a trust anchor in `is_sent_root`.
- BAD_MD: the hash in the self-signature, signed with the pinned key
  itself; meaningless once the key is pinned.

Never overridden, pin or no pin: REVOKED, KEY_USAGE, EXT_KEY_USAGE,
NS_CERT_TYPE, BAD_PK, BAD_KEY (a weak or unsupported key stays refused).

Order in the callback: the sent-root rule; then a leaf whose issuer is
a bundle root is left to ESP-IDF; then a matching pin short-circuits before ESP-IDF's
lookup (which has no issuer to find for a self-signed leaf and would end
the handshake). An unconfirmed or changed key is only reported after
ESP-IDF's own verdict refused the leaf, so a leaf a bundle root issued
directly is never logged or offered as refused. A leaf whose issuer name
is a root of the bundle (`Bundle::has_subject`) is left to the public PKI
altogether: expired or on the wrong name, it is never offered for a pin.

Not covered: a chain from a private CA (leaf + CA sent). mbedTLS marks
the CA, not the leaf, as "not trusted"; the leaf's flags are 0 and the
pin rule does nothing, so nothing is offered for pinning and the chain
fails as before. The pin is a fallback after the bundle, not strict
pinning: with a pin stored, a certificate the public PKI trusts for the
host name connects through the bundle whatever its key (device test 7).

The stored pin survives a change of the pool's address or port (the
same pool under its IP address keeps working); only the pending key is
dropped. Pointing the entry at another self-signed pool therefore shows
"The pool's key has changed", and the text says so.

## API

`GET /api/system/info` (also in `/api/v1/state` → `config`, `stratum`):

| Field | Meaning |
|---|---|
| `stratumTLSPin`, `fallbackStratumTLSPin` | the confirmed key, `AB:CD:…:EF`, "" = none |
| `stratumTLSPending`, `fallbackStratumTLSPending` | a key seen but refused, for confirmation; null = none; only while the slot's host:port is the one it was seen at |
| `stratumTLSKeyChanged`, `fallbackStratumTLSKeyChanged` | the pending key differs from a confirmed one |
| `stratumTLSPinned` | the running session's certificate was accepted by the pin, not the bundle |

`PATCH /api/system {"stratumTLSPin": "<fingerprint>"}` stores it (any
case, with or without colons; 400 when not 32 bytes of hex);
`{"stratumTLSPin": ""}` forgets it. Both reconnect the pool at once, as
every pool field does, and clear the slot's pending key.

NVS: no new keys. The pins are two fields of the config document
(`config` in namespace `warpaxe`); a saved pool's pin is a field of its
entry in `pools`, written only when set.

Rules kept: the TLS callback writes RAM only (the stratum thread's);
the pin is written to NVS by the HTTP handler. Every PATCH wakes the
display, as before, which is fine for a click.

## Device test (Gaia against the Umbrel, by WarpAxe #2)

1. Gaia on V1 + TLS, port 3334, no pin: the session fails with "pool
   certificate not from a public authority: key 15:1A:63:B8 … 09:9E:E0:47
   (trust it on the Pool page)"; the Pool page shows the yellow box with
   that fingerprint; the fallback takes over after three failures as
   before.
2. Click "Trust this pool": PATCH with the fingerprint, reconnect, the
   log says "TLS up, certificate accepted by the pinned key"; the box
   turns to "Trusted key" with "The current connection was accepted with
   this key."; `stratumTLSPinned` true. Shares accepted.
3. Set the pool address to the IP (192.168.178.3): still connects (no
   host-name check with the pin).
4. Save the pool under a name, switch to another saved pool and back:
   the pin travels with the entry.
5. "Forget": the next connect fails again with the yellow box.
6. Key change: restart WarpPool with a new certificate key (or point the
   miner at another self-signed server): red box "The pool's key has
   changed", no connection, the old short form shown; "Trust the new
   key" connects.
7. A public pool over TLS (e.g. a public WarpPool port) with a pin
   stored for it: connects through the bundle, `stratumTLSPinned` false.
8. Online update check and price fetch still work (they do not use the
   pool attach).

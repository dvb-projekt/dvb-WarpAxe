# An unencrypted fallback pool

Status: built on `feat/plaintext-fallback`, not in a release yet.

On 25 November 2026 Braiins reported BGP hijacks against plain Stratum
V1. The attacker announces a more specific route to the pool's address,
so the miner's connection ends at the attacker's server while DNS stays
untouched. That server accepts any worker name and hands out jobs from
another pool, so the shares pay the attacker. TLS with a checked
certificate, or Stratum V2 with the pool's authority key, stops this. A
plain V1 fallback pool behind such a main pool opens the gap again.

On TLS, WarpAxe already checks the certificate's name and authority
(`link.rs`, `tls_trust`) or the pinned key (docs/14); on Sv2 it checks
the pool's certificate against the authority key. What it did not check
was the fallback: after three failed attempts on the main pool, or a main pool
that stops sending jobs, it switched to the fallback whatever its protocol
and host. A refused certificate counted as a failed attempt like any other.
So an attacker on the route who presented a false certificate three
times could move the miner to a plain fallback.

## What changes

`warpaxe_stratum::guard` (host-tested) holds the rules:

- `is_local_host`: a private, shared (CGNAT), loopback or link-local address
  (10/8, 127/8, 172.16/12, 192.168/16, 169.254/16, 100.64/10, ::1,
  fc00::/7, fe80::/10, the IPv4-mapped forms), or a name that only
  resolves on the local network (a single label, `.local`, `.lan`,
  `.home.arpa`, `.internal`, `localhost`). It uses the same list as
  WarpPool's connect dialog (`ui/src/lib/localHost.ts`).
- `plain_public`: plain V1 (neither TLS nor Sv2) on a host that is not
  local.
- `hold_main`: the main pool is protected (TLS, or Sv2 with an
  authority key), the fallback is `plain_public`, and the main pool's
  certificate was just refused.

In the firmware:

- `tls_trust::verify_pool` records when the pool's certificate is
  refused, for any reason (another name, expired, an unknown
  authority, a key that is not the pin). An Sv2 certificate not signed by
  the authority key counts the same. A refusal while NTP has not set the
  clock yet is still treated as a clock problem, as before, and is not
  counted.
- When `hold_main` is true, `stratum::run` does not switch: no failure
  is counted towards the fallback, and the outage retries and the
  main/fallback alternation try only the main pool. The chips idle, as
  with any pool that sends no work. The event log gets `pool_held_cert` once. The hold
  ends when the main pool is accepted and mines again, when the settings
  are saved, or when the settings no longer call for it (the fallback gets TLS,
  Sv2 or a local host).
- A main pool that is simply unreachable (no answer, connection reset)
  still switches to the fallback after three attempts. The firmware
  cannot tell an outage from an attacker who drops the connection, or
  who sends a malformed certificate or a TLS alert that never reaches
  the certificate check. The hold covers only an attacker who presents a
  certificate that is then refused; the warnings below cover the rest.
- The miner still switches as before to a fallback with TLS or Sv2, or
  on the local network.

## What the owner sees

- API (`/api/system/info`): `stratumPlainPublic` (the current pool, or the
  pool being tried, is plain V1 on a public host) and `stratumHeldForCert` (the hold
  above).
- The pool line on the dashboard and on the Pool page shows
  "unencrypted" after the protocol for such a pool.
- The dashboard shows a notice while the miner is on an unencrypted
  public fallback pool, and while the hold is in force.
- Pool settings: a warning under the fallback pool while it is plain V1
  on a public host, updated as the fields are edited.
- The serial log line of a switch says when the fallback is unencrypted on a
  public host.

A plain main pool on a public host is shown as "unencrypted" too, but
nothing else changes for it: that is the owner's choice and works as
before.

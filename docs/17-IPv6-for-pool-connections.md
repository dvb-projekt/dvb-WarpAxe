# IPv6 for pool connections

Until v0.1.45 WarpAxe mined over IPv4 only: lwIP was built with IPv6, but
the station never asked the router for an IPv6 address, and the pool
connection took the first address `getaddrinfo` returned. With
`AF_UNSPEC`, lwIP returns only the A records of a name that has both.

The trigger was ESP-Miner PR #2105 ("Prefer IPv6 for mining with IPv4
fallback", Vortex Radar), which we tested on our native dual-stack
line on 2026-11-17 and commented on
(bitaxeorg/ESP-Miner#2105, issuecomment-6943665366). What we learned
there went into this design: the PR tries each address one after the
other with a 5 s connect timeout each, asks for AAAA even on IPv4-only
networks, does not accept `[addr]` literals, and counts a dropped long
session as a failed attempt. (WarpAxe's own fallback rule does not have
that last bug: a session that ran a minute and brought work resets the
counter, `stratum.rs`.)

Decision (2026-11-17): IPv6 in v0.1.46, with a switch in the web
UI, on by default.

## The rule

1. **SLAAC on association.** With the `ipv6` setting on (default), the
   station creates its link-local address as soon as it is associated,
   before DHCP (`net/wifi.rs` `start_ip6`; `CONFIG_LWIP_IPV6_AUTOCONFIG`).
   The router's advertisement adds the global and unique-local
   addresses: on a FRITZ!Box about 3 s after the link-local address
   (logged at 2 s granularity), about 2 s after the IPv4 address. Retried while the netif is not up yet;
   also started when the switch is turned on later, without a restart.
2. **IPv4 first, AAAA only when it can help.** For a pool name, the A
   records are looked up first. AAAA is asked for only when the setting is
   on and the station holds a global or unique-local address. A name whose
   AAAA lookup is empty or takes longer than 2 s is not asked again for 30
   minutes, so a resolver that drops AAAA queries costs one wait, not one
   per reconnect. A station with only a unique-local address (a router
   without IPv6 upstream) tries IPv6 for unique-local pools only.
3. **Happy Eyeballs (RFC 8305).** IPv6 and IPv4 addresses alternate,
   IPv6 first. The next address starts 250 ms after the previous one, or
   at once when the previous one was refused; the first connection that
   completes wins, the others are closed (`net/dial.rs`, timing in
   `warpaxe_stratum::dial::Race`, host-tested). A route that drops packets
   silently costs a quarter second, not a connect timeout per address.
   lwIP keeps one address per name (`CONFIG_LWIP_DNS_MAX_HOST_IP=1`), so
   in practice it is one IPv6 against one IPv4 address.
4. **The first connection waits briefly for the prefix.** Right after
   joining, the first pool connection follows the IPv4 address within a
   fraction of a second, while the prefix is still on its way. Connections
   within the first 4 s of SLAAC wait for a global or unique-local
   address; later ones never wait. On an IPv4-only network this costs up
   to 4 s once per association.
5. **TLS.** esp-tls resolves names itself with `AF_UNSPEC` (A records
   only), so WarpAxe picks the address and hands esp-tls the literal, with
   the pool's name as `common_name`: it still goes out as SNI and the
   certificate is still checked against it. With one address (every
   IPv4-only network, every literal) no extra connection is made; with
   several, the race decides and its connection is closed again before
   esp-tls opens its own (one extra TCP handshake, no data). Key pinning
   (docs/14) never used the host name and is unchanged.
6. **Literals.** A pool address may be an IPv6 literal, with or without
   brackets (`[2001:db8::1]` or `2001:db8::1`). It is used as typed, also
   with the switch off. Displays and logs write IPv6 literals in brackets
   so the port stays readable.
7. **The web UI stays local.** The web UI and the API have no login. With
   IPv4 the router's NAT keeps the internet out; a global IPv6 address has
   no NAT in front, only the router's firewall (most block incoming IPv6
   by default, not all). So the HTTP server's open function refuses IPv6
   peers outside the link (fe80::/10), unique local (fc00::/7), loopback
   and the miner's own /64; IPv4 is always served (also as `::ffff:a.b.c.d` on the
   dual-stack socket). Refusals are counted (`ipv6Refused`); the first ten
   and every hundredth are logged. This needed `open_fn` in esp-idf-svc's server configuration
   (vendor patch 3, `vendor/esp-idf-svc/WARPAXE-PATCH.md`).

## The switch

Network page, "IPv6", on by default, with the addresses the miner holds
under it. Off: pool names go over IPv4 at once (the session is rebuilt);
the addresses themselves stay until the next start. On: SLAAC starts
within two seconds and the session is rebuilt. `PATCH /api/system`
`{"ipv6": false}`.

## API

`/api/system/info`: `ipv6` (the setting), `ipv6Addresses` (global and
unique-local addresses of the station), `poolAddress` and
`poolAddressFamily` (`"IPv4"`/`"IPv6"`, null while no session is up),
`ipv6Refused` (web connections refused since boot). The pool cards show
the family next to V1/V2 and TLS.

## What it costs

- Image: +12,352 B (2,847,264 B at a25db09 against 2,834,912 B at
  04643cf).
- Internal RAM, Gamma .42: `freeHeapInternal` 33,975 B and
  `largestFreeInternal` 25,600 B after 28 minutes on a25db09, against
  38,755 / 26,624 B after 92 minutes on 04643cf: about 5 KB less free and
  the largest block 1 KB smaller (the IPv6 addresses, neighbour and
  router state, the second address family in DNS). Not measured at the
  same uptime, so a rough figure. The tightest boards are the Naja Duo
  (.47: 24,799 / 14,848 B on 04643cf) and the Nerd boards (14,336 B
  largest block); check them after the fleet flash.
- An IPv4-only network: one A lookup and one connect as before, plus up
  to 4 s once per association (rule 4).

## Limits

- An IPv6-only network (no IPv4 at all) is not supported: the DNS
  servers come from DHCPv4 (`CONFIG_LWIP_IPV6_RDNSS_MAX_DNS_SERVERS` is 0).
- Only pool connections use IPv6. The online update, market data, NTP and
  the swarm search stay on IPv4.

## Privacy

lwIP has no privacy extensions (RFC 4941). The global address is the
EUI-64 one, built from the MAC, so its last 64 bits are the same in every
network and every IPv6 pool sees the miner's MAC. Pools see the public IPv4
address under NAT anyway, but not the MAC. Users who mind turn the switch
off.

## Tested (Gamma .42, 2026-11-17, b63c163 to a25db09)

| Case | Result |
|---|---|
| Dual-stack pool (solo.atlaspool.io:3333) | IPv6, shares accepted |
| IPv4-only pool (public-pool.io:21496) | IPv4, subscribed and authorized |
| AAAA present, IPv6 refused (solo.ckpool.org:3333) | IPv6 reset, IPv4 won after 355 ms |
| `[ULA literal]` of a relay that accepts IPv6 only | IPv6 |
| Local name with A + AAAA, same IPv6-only relay | IPv6 (global address), shares |
| Switch off, dual-stack pool | IPv4 at once, shares |
| Switch on again | IPv6, shares |
| TLS, dual-stack (solo.atlaspool.io:4333) | IPv6, certificate verified for the name |
| TLS, IPv4-only (public-pool.io:4333) | IPv4, no probe, certificate verified |
| First connection after boot, dual-stack pool | IPv6 (prefix about 3 s after link-local) |
| Stratum V2 to warppool through the IPv6-only relay (`[ULA literal]`) | IPv6, Noise handshake and certificate verified, shares accepted |
| Web UI over the global address from the same /64, and over the ULA | served |

Not tested on hardware: an IPv6 route that drops packets silently (the
timing is host-tested), a refused web connection from outside the /64
(the rule is host-tested).

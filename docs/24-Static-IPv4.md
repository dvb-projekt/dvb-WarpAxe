# Static IPv4

Status: test build on its own branch, not in a release.

Some routers forget DHCP reservations, or hand out a reserved address
late, and a miner that moves around the LAN is lost to dashboards and
tuners that know it by address. The stock firmwares have no answer:
ESP-Miner (AxeOS) has no static address, and NerdOS closed its request
(#138) with "use the router". WarpAxe offers one as an option, off by
default, with a guard so that a typo cannot cut the miner off.

## Settings

| Field | Meaning |
|---|---|
| `staticIp` | the switch; off by default |
| `staticIpAddr` | the address, dotted |
| `staticNetmask` | dotted (`255.255.255.0`) or a prefix length (`24`, `/24`) |
| `staticGateway` | the router |
| `staticDns` | first DNS server; empty = the gateway |
| `staticDns2` | second DNS server; empty = none |

They apply at boot. A `PATCH /api/system` with the switch on (or turning
it on) is checked as a whole by `warpaxe_board::static_ip::validate`
(host-tested) and refused with a 400 naming the field when the settings
cannot work:

- every address is dotted IPv4 unicast (no 0.x, 127.x, multicast, class E
  or 255.255.255.255; no leading zeros);
- the mask is contiguous and between /8 and /30;
- the gateway lies in the address's subnet and is not the address;
- neither the address nor the gateway is the subnet's network or
  broadcast address.

With the switch off the fields are stored trimmed and unchecked, so a
half-filled form can be kept. Existing configs read the new fields as
their defaults (off, empty): nothing changes for them.

## Which interface

One address, one interface: the Ethernet HAT (docs/13) when a W5500
answered at boot, otherwise the Wi-Fi station. Giving both the same
address would put one IP behind two MACs on the same LAN. The other
interface keeps DHCP; the setup hotspot (192.168.4.1), the USB link and
IPv6 (SLAAC, docs/17) are untouched. The Wi-Fi thread waits for the HAT's
probe before its first association when the switch is on (a fraction of
a second, at most ten).

## How it is applied

The netifs stay DHCP clients as before (`net/wifi.rs`, `net/eth.rs`).
`net/static_ip.rs` replaces DHCP on the chosen one while its link is
down: `esp_netif_dhcpc_stop`, `esp_netif_set_ip_info`, then
`esp_netif_set_dns_info` (main, backup), since setting the address clears
the DNS servers. On the link-up event ESP-IDF raises "got IP" for the
static address itself (`esp_netif_action_connected` with the client
stopped). An `esp_netif_stop` (the hotspot opening or closing, the
Ethernet driver restarted after `eth::NO_ADDRESS`) resets the netif to
DHCP, so the address is set again before every association and every
driver start.

## Lock-out guard

`warpaxe_board::static_ip::Watch` (host-tested), driven by the
interface's thread:

- With the link up and nothing confirmed, the gateway is probed every
  10 s with a TCP connect to port 80, 2 s timeout. An open or a refused
  connection is an answer (the host is there); a timeout or "host
  unreachable" (no ARP reply) is not. A pool session whose local address
  is the static one counts as an answer too, also one to a pool on the
  LAN itself without the gateway (seen on the devkit: with a dead
  gateway the main pool's name did not resolve, the miner moved to its
  fallback pool on the LAN, and that session confirmed the address). The
  guard is about reaching the miner, and the address does work on the
  LAN then; the dead gateway still shows in the pool's failures.
- The first answer confirms the address for the boot; no more probes.
- 180 s of link without an answer (`GIVE_UP_S`; time without link does not
  count, so a router that is still booting after a power cut gets its
  time): the interface goes back to DHCP until the next restart. Wi-Fi
  disconnects, starts its DHCP client and associates again; Ethernet stops
  the driver, starts its DHCP client and starts the driver again (the stop
  alone left the static address on the netif: .41, 2026-11-25). A pool session still on the static address is
  moved. Event `static_ip_fallback` (kind `net`: address, interface,
  gateway, seconds), `ipMode` "static-fallback", and a yellow dashboard
  banner linking to the Network tab.

If DHCP fails too, the existing paths take over: the setup hotspot opens
after its usual three minutes without a station link (Wi-Fi), and the
next restart tries the static address again.

Not covered: another device already holding the address (the gateway
answers, so the guard confirms). The router's ARP table then flips
between the two.

## API

System info (and so the diagnostics file):

| Field | |
|---|---|
| `ipMode` | `dhcp`, `static`, `static-fallback` |
| `staticIpInterface` | `wifi`, `ethernet`, or null |
| `staticIpConfirmed` | the gateway or the pool answered on the static address |
| `ipNetmask`, `ipGateway`, `ipDns` | what the LAN interface uses now (DHCP or static): the hint under the static fields |

## UI

Network tab: a switch "Static IPv4" under the IPv6 switch; the five
fields show only while it is on, each with the value in use now. The
Connection block gets a line: in use (green), waiting for the gateway, or
given up for DHCP (yellow). Texts in all eight UI languages; the six
translated ones still need the native-speaker check before a release.

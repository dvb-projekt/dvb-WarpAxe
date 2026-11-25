# Static IPv4

Status: test build on its own branch, not in a release.

Some routers forget DHCP reservations, or hand out a reserved address
late, and a miner that moves around the LAN is lost to dashboards and
tuners that know it by address. The stock firmwares have no answer:
ESP-Miner (AxeOS) has no static address, and NerdOS closed its request
(#138) with "use the router". WarpAxe offers one as an option, off by
default, with a guard so that a typo cannot cut the miner off.

## Settings

One config field, `staticIpv4`, holds the switch and the five fields:

```
192.168.1.50,24,192.168.1.1,,9.9.9.9      on
-192.168.1.50,24,192.168.1.1,,9.9.9.9     off, the fields kept
                                          (empty) off
```

In order: the address; the mask, dotted (`255.255.255.0`) or a prefix
length (`24`, `/24`); the gateway (the router); the first DNS server
(empty = the gateway); the second (empty = none). Missing trailing
fields count as empty, spaces are taken off. One field instead of six
because every field of `Config` costs 0.5 to 0.9 KB of image in its
serde code (see "Image size" below).

The setting applies at boot. A `PATCH /api/system` that sets it on is
checked as a whole by `warpaxe_board::static_ip::validate_packed`
(host-tested) and refused with a 400 naming the field when the settings
cannot work:

- every address is dotted IPv4 a host can have (no 0.x, 127.x,
  multicast, 240 and up, 255.255.255.255; no leading zeros);
- the mask is contiguous and between /8 and /30;
- the gateway lies in the address's subnet and is not the address;
- neither the address nor the gateway is the subnet's network or
  broadcast address.

Switched off, the fields are stored unchecked, so a half-filled form can
be kept. Existing configs read the field as empty (off): nothing changes
for them.

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
`esp_netif_set_dns_info` (main, backup; an empty second server repeats
the first). On the link-up event ESP-IDF raises "got IP" for the
static address itself (`esp_netif_action_connected` with the client
stopped). An `esp_netif_stop` (the hotspot opening or closing, the
Ethernet driver restarted after `eth::NO_ADDRESS`) resets the netif to
DHCP, so the address is set again before every association and every
driver start.

### DNS

lwIP keeps one list of DNS servers for all interfaces
(`CONFIG_ESP_NETIF_SET_DNS_PER_DEFAULT_NETIF` is off, and stays off: it
would change DNS for every setup). That list is cleared by
`esp_netif_set_ip_info` on a DHCP client netif and by every
`esp_netif_dhcpc_start` (`dns_clear_servers`), and written by every
DHCP lease, on either interface. With the address on the HAT and the
station on DHCP, the station's lease would replace the typed servers.
So the guard looks at the list on every tick (every one or two
seconds) and sets the typed servers again when they were replaced,
for as long as the static address is in use: confirmed and with its
link up (a HAT with its cable out leaves the station's DNS alone, the
station may be in another network), not before the first answer and
not after the fallback. The log says `static ip: DNS … set again`. A
lookup in the second or so before that, or before the address is
confirmed, can still go to the lease's server. `ipv4Now` shows the server in the list.

## Lock-out guard

`warpaxe_board::static_ip::Watch` (host-tested), driven by the
interface's thread:

- With the link up and nothing confirmed, the gateway is probed every
  10 s with a TCP connect to port 80, 2 s timeout, from a socket bound to
  the static address (so an answer over the other interface on the same
  subnet does not count). An open or a refused
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
  time). Only the time between two ticks that both had the link counts:
  the Wi-Fi thread ticks without the link before every association, so
  the scan and the join (up to 20 s) are not link time: the interface goes back to DHCP until the next restart. Wi-Fi
  disconnects, starts its DHCP client and associates again; Ethernet stops
  the driver, starts its DHCP client and starts the driver again (the stop
  alone left the static address on the netif: .41, 2026-11-25). A pool session still on the static address is
  moved. Event `static_ip_fallback` (kind `net`: address, interface,
  gateway, seconds), `ipMode` "static-fallback", and a yellow dashboard
  banner linking to the Network tab.

If DHCP fails too, the existing paths take over: the setup hotspot opens
after its usual three minutes without a station link (Wi-Fi), and the
next restart tries the static address again.

No duplicate detection: an address another device already holds is
confirmed all the same (the gateway answers). The router's ARP table
then flips between the two.

With the HAT fitted and no cable in, the address waits for the cable:
`ipMode` stays `static-wait` and the guard counts nothing, while the
station keeps DHCP. That is the HAT's own state, not a fault.

## API

System info (and so the diagnostics file):

| Field | |
|---|---|
| `ipMode` | `dhcp`; `static-wait` (static, nothing has answered yet); `static` (the gateway or the pool answered); `static-fallback` (no answer, DHCP until a restart) |
| `ipv4Now` | what the LAN interface uses now (DHCP or static) as `"mask,gateway,dns"`, the hint under the static fields; `dns` is the first server in lwIP's list (the one lookups go to); null without a LAN address |

Which interface carries the static address is in the log
(`static ip: wifi …` / `static ip: ethernet …`) and in the event.

## UI

Network tab: a switch "Static IPv4" under the IPv6 switch; the five
fields show only while it is on, each with the value in use now. The
Connection block gets a line: in use (green), waiting for the gateway, or
given up for DHCP (yellow). Texts in all eight UI languages, the six
translations native-speaker checked (2026-11-25). The page splits
`staticIpv4` into the five fields and joins them back on save.

## Stack

The Wi-Fi thread has 8 KB of internal RAM and now also runs the probe
(a raw lwIP connect), the event record and the address texts. The least
stack it left untouched, logged at every Wi-Fi connect and at the
guard's confirm and fallback (`stack … B never used`), on the devkit
with 966dccb (2026-11-25):

| Run | Never used |
|---|---|
| DHCP, connect | 3,128 B |
| static, confirmed by the gateway probe | 3,008 B |
| static, 182 s of failed probes, fallback, DHCP association | 3,128 B |
| switched off again, connect | 3,312 B |

About 5.1 KB used at most, 3 KB to spare: the stack stays at 8 KB.

## Image size

The feature added 25,376 B to the image at 62da248 (against main
c7e1976 built in a worktree of the same path length); db96424 adds
12,304 B, 13,072 B less. The review fixes (DNS kept, bound probe,
link-to-link counting, stack log lines) add 800 B: 966dccb is
2,980,176 B, +13,104 B against c7e1976, 100,016 B below 0x2F0000. Taken out without giving up function, each step
measured on its own (the steps were measured on uncommitted trees, so
they do not add up to the byte):

| Step | Bytes |
|---|---|
| Ip4 hint as plain bytes instead of three Strings | −304 |
| Ip4 hint out of `NetState` (every reader copies the state) | −1,344 |
| five String fields of `Config` packed into one | −4,592 |
| `system_info`: three fields instead of six | −960 |
| shorter error texts and log lines, no clone of the stratum state per tick | −352 |
| the switch into the same field (`-` = off) | −624 |
| `ipMode` carries "waiting" instead of its own field | −352 |
| one validation function instead of two, a byte for the mode instead of enum + mutex | −4,560 |

The last step gained more than its own functions weigh: with less code
in the network threads, LLVM inlined the surrounding code as it did
before the feature. Two tries did not count and were dropped: the raw
`esp_wifi_disconnect` instead of `BlockingWifi::disconnect` (+16), and a
short dashboard note instead of the long one (−528, kept the long one).
The TCP probe costs nothing: `connect_timeout` and `lwip_select` were in
the image already, as was core's IPv4 parser.


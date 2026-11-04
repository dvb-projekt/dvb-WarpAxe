# 13 · Ethernet over the W5500 HAT (NerdHat)

Written 2026-11-04 (branch `claude/warpaxe-gaia-ethernet-cd405d` on
`main` e04bfb1), after Oliver ordered a NerdHat for the Gaia. Built and
host-tested without the hardware; the device test below is still to be
run once the HAT is here. Nothing in this round changes the Gaia's
clock and voltage limits.

## What it is

The NerdHat (Bitronics NERDETH rev 1.1) is a WIZnet W5500 on a small
board that plugs onto the header of the NerdQAxe family, and onto the
NerdAxe Gaia over an interposer. The W5500 is a MAC and PHY with its own
TCP/IP offload engine, talked to over SPI; ESP-IDF drives it as a plain
MAC (`esp_eth_mac_new_w5500`, `CONFIG_ETH_SPI_ETHERNET_W5500`) with lwIP
on the ESP, the same stack Wi-Fi uses, so every socket in the firmware
works over it unchanged.

NerdOS supports the HAT from v1.1.1 (shufps/ESP-Miner-NerdQAxePlus PRs
#724 for the Gaia and #731 for the QAxe family; `main/network/w5500.cpp`,
`main/network/network_manager.cpp`, `EthPins` in `main/boards/board.h`).
AxeOS has no Ethernet. WarpAxe follows NerdOS's pins, its detection and
its reset; the routing is lwIP's own.

## Boards and pins

| Board | SCLK | MOSI | MISO | CS | RST | INT | Profile |
|---|---|---|---|---|---|---|---|
| NerdQAxe++ (every revision, Rev 7 included) | 2 | 12 | 16 | 21 | – | – | `NERDQAXE_PLUSPLUS`, `NERDQAXE_PLUSPLUS_REV7` |
| NerdOctaxe-γ up to rev 3.3 | 2 | 12 | 16 | 21 | – | – | `NERDOCTAXE_GAMMA` |
| NerdAxe Gaia (over the interposer) | 10 | 3 | 16 | 21 | – | – | `NERDAXE_GAIA` |

The pins live in `crates/board/src/profile.rs` (`EthPins`, the constants
`NERDHAT_QAXE` and `NERDHAT_GAIA`, the field `Profile::ethernet`), with
tests that only these profiles carry them, that none of the HAT's GPIOs
is one the profile drives itself, and that the Gaia's are not GPIO2, 12,
13 or 11.

Two things to know about the table:

- **No reset and no interrupt line on any board.** The handover text
  gave 13/11 for the QAxe family; those are the defaults of NerdOS's
  `W5500` class, which every board overrides with `GPIO_NUM_NC`
  (`kHatPins` in `nerdqaxeplus.cpp`, `kEthPins` in `nerdaxegaia.cpp`).
  The HAT resets itself by an RC network at power-on; the firmware
  resets the chip over SPI at every start (below); the driver polls the
  chip instead of waiting for an interrupt. The profile keeps `rst` and
  `int` as options and the driver honours them, so a board that wires
  them needs only a profile change.
- **The Gaia's pins are others on purpose.** On the Gaia the QAxe pins
  are /VDD (GPIO2), the LDO enable (GPIO12), PMB_ALRT (GPIO13) and PGOOD
  (GPIO11): SPI traffic there switches power rails. The interposer brings
  SCLK out on GPIO10 (JP2 2-3) and MOSI on GPIO3 (JP3 2-3) instead. The
  firmware never uses one board's pins on another: the pins come from the
  profile of the board that was detected or verified, and only then.

The NerdQAxe+ (BM1368) also carries the header in NerdOS, but WarpAxe has
no profile for that board, so it is not in the table. The NerdQX (Q1370 /
Q1373) uses the same pins in NerdOS (`nerdqx.h`); its WarpAxe profiles
say `ethernet: None` for this round, which covers the boards that have
run here.

## Not possible

- **NerdOctaxe-γ from rev 3.4** (and the NerdQX with the same part): two
  TMP451 temperature multiplexers read the chips' diodes, and their
  select lines A0/A1 are GPIO2 and GPIO12, the HAT's SCLK and MOSI. SPI
  traffic would switch the mux. NerdOS refuses the HAT when either TMP451
  answers; WarpAxe does the same: `board::ethernet_pins` asks the board's
  I²C bus for 0x4C and 0x4E before anything is driven, and an answer
  there means no Ethernet, with a line in the log. WarpAxe reads no
  TMP451 itself (the Octaxe's temperature comes from the TMP1075 as under
  NerdOS's fallback path), so this is about not disturbing the board, not
  about WarpAxe's readings.
- **Boards with a CAN HAT**: the CAN transceiver sits on GPIO16/21 (NerdOS
  `NerdQaxePlus::getEthPins` returns nothing when CAN is fitted). WarpAxe
  has no CAN support and does not know whether a CAN HAT is plugged in;
  a CAN HAT would make the W5500 probe fail (no VERSIONR), and the pins
  would see the probe's few SPI clocks once. Nothing more is driven.

## How it starts

In `main.rs`, after the USB netif and before Wi-Fi (`set_step("ETHERNET")`):

1. `board::ethernet_pins()` — the profile's pins, or nothing: a board
   without the header, the undetected profile of a first start, a board
   chosen by hand whose chain has not confirmed the choice yet (the pins
   would be the chosen board's; Ethernet starts from the next boot, which
   runs verified), or a TMP451 answering on the QAxe pins (above).
2. `net::eth::start` on a thread of its own, 8 KB internal stack:
   - the SPI bus on SPI2 with DMA (frames up to 1.5 KB go in one transfer),
     2 MHz as NerdOS runs the HAT (long wires on a header);
   - **the probe**: VERSIONR (0x0039) read over a device with the HAT's
     chip select; a W5500 reads 0x04, a board without the HAT reads 0xFF
     or 0x00. Not 0x04 means one log line and the thread ends, the bus is
     freed, and the firmware runs as before on Wi-Fi and USB
     (`ethStatus: "Not fitted"` in the API);
   - **the software reset**: MR (0x0000) written with RST (0x80), 2 ms. A
     software restart of the ESP alone leaves the W5500 as it was, and the
     HAT's RC reset only works at power-on. ESP-IDF's own driver resets and
     verifies the chip once more when it is installed;
   - the driver: `EthDriver::new_spi_with_event_source` with
     `SpiEthChipset::W5500`, polling every millisecond where there is no
     interrupt pin (NerdOS `poll_period_ms = 1`; the chip buffers 16 KB),
     and the ESP's efuse Ethernet MAC (`ESP_MAC_ETH`, distinct from the
     Wi-Fi station's and the USB link's) written to the chip. ESP-IDF
     sets a MAC on its own only for the internal EMAC; a SPI MAC keeps
     00:00:00:00:00:00 unless the driver is told one, and the W5500's MAC
     filter would then drop every unicast frame (the audit's first find);
   - the netif: `ETH_DEF`, DHCP client with the miner's hostname, route
     priority 110 (the Wi-Fi station's is 100, the USB netif's 30);
   - then a loop every second: link, address, speed and duplex into
     `net::state()`, log lines on every change. A link that holds no
     address for 60 s (a port without DHCP, an outage) gets the driver
     stopped and started again: esp_netif keeps the default route on a
     netif that is up, address or not, so after one Wi-Fi flap such a
     link would hold the route away from a working Wi-Fi; stopping takes
     the netif down and the route moves, starting retries DHCP.

The failure paths are visible: a bus, driver or netif that does not come
up sets `ethStatus: "Failed"` (the log says which step), distinct from
"Not fitted".

`esp-idf-svc` 0.51 (the vendored copy) carries the whole W5500 path; the
only change to the build is `CONFIG_ETH_SPI_ETHERNET_W5500=y` in
`sdkconfig.defaults`, which makes esp-idf-sys build `esp_eth`'s SPI
drivers and sets the `esp_idf_eth_spi_ethernet_w5500` cfg esp-idf-svc
compiles its W5500 code under. A build after that change needs the
generated sdkconfig redone (`cargo clean -p esp-idf-sys`, or a fresh
target directory).

## Routing, fallback and the hotspot

- **Preference is lwIP's.** Every interface with an address is up, and
  esp_netif picks the default route by the highest route priority among
  them: Ethernet when it has an address, else Wi-Fi, else USB. The pool
  connection, the update check, the price fetch and NTP leave over the
  default route; the HTTP server, mDNS and the API answer on every
  interface all the time. Wi-Fi is not stopped when the cable is in (NerdOS
  stops it); it stays connected as the fallback.
- **Cable gone**: esp_netif takes the Ethernet netif down and clears its
  address; lwIP aborts every TCP connection bound to that address
  (`tcp_netif_ip_addr_changed`). A pool session that was on the cable
  ends; `net::eth` sees the address go and, if the session's local address
  was the cable's (`stratum::state().local_ip`, from `Link::local_ip`),
  calls `stratum::reconnect()` so the next session starts now over Wi-Fi
  instead of after the backoff.
- **Cable back**: the address returns, the default route is Ethernet
  again, and a pool session that runs over Wi-Fi is moved once the cable
  has held its address for 30 s (`SETTLE`): one `stratum::reconnect()`,
  never one per flap. A cable that comes and goes within 30 s causes no
  reconnect of its own beyond the one its loss forced. This applies to
  Stratum V1 (plain and TLS) and V2 alike; both sessions record their
  local address on connect.
- **The setup hotspot** opens only without any connection: the Wi-Fi
  thread feeds `Hotspot::tick` with Ethernet as a connection
  (`connected || eth`) and a fitted HAT as a configured network
  (`has_ssid || eth_fitted`), and holds its first ticks while the HAT's
  probe or a fresh link's DHCP is still pending. So with a cable the
  hotspot never opens, with or without a network in the settings; a HAT
  without a cable and no network in the settings opens it after the
  usual three minutes, not at once; a cable pulled later with no network
  in the settings opens it at once; a hotspot that is open closes a
  minute after the cable has an address. Wi-Fi keeps trying its
  configured network meanwhile.
- **mDNS** starts once Wi-Fi or Ethernet has an address and announces on
  both; the mDNS component handles the `ETH_DEF` netif on its own
  (`CONFIG_MDNS_PREDEF_NETIF_ETH`), to be confirmed on the device.

## What the firmware shows

- `/api/system/info`: `ethStatus` (null on a board without the header,
  "Not fitted", "No link", "No address", "Connected!"), `ethIP`, `ethMAC`,
  `ethSpeed` (10 or 100), `ethFullDuplex`, `netInterface` ("ethernet",
  "wifi", "usb" or null: the default route), `wifiIP`. `wifiStatus` and
  `wifiRSSI` stay what they were. NerdOS reports "LAN Connected!" in
  `wifiStatus`; WarpAxe keeps that field for Wi-Fi so a tuner's reading of
  it does not change.
- `/api/v1/state`: `net.eth` (fitted, link, connected, ip, mac,
  speed_mbit, full_duplex) and `net.active`.
- Web UI: the dashboard's network cell and the header show "Ethernet ·
  100 Mbit/s" with a plug while the default route is the cable, the Wi-Fi
  level otherwise; the Network tab has a "Connection" block with every
  interface, its address and which one is active, plus a hint on the
  fallback, on boards with the header only. Eight languages
  (`firmware/warpaxe/i18n/ui`).
- Display: the preferred address everywhere an address is shown (cable,
  Wi-Fi, USB); the pool page's right-hand corner says "Ethernet ·
  100 Mbit/s" instead of SSID · dBm; the device page of a board without a
  regulator shows the same; the OLED's Wi-Fi slide says "ETHERNET 100
  MBIT/S". No new display strings to translate ("Ethernet" is the same in
  the six display languages).
- Swarm scan: the miner scans the /24 of its LAN address, the cable's
  first (`net::state().lan_ip()`); the browser side reads `net.eth.ip`
  before `net.wifi.ip`.

## Image

Release image 2,983,728 bytes, against 2,905,280 for `main` e04bfb1 built
the same way: +78,448 bytes, most of it `esp_eth` and the W5500 driver.
The release gate of `tools/release-firmware.sh` is 0x2F0000 (3,080,192
bytes, the NerdMiner config window on a first install): 96,464 bytes
left under it, 174,912 before this change.

## Device test (once the HAT is here)

Device actions only after Oliver's OK, announced to Warppool #2 first,
with an image from a committed state. The Gaia is the first board; the
NerdQAxe++ and the Octaxe rev 3.1 follow with the same list.

1. **Without the HAT, before anything else** (the regression test): flash
   the image on the Gaia without the interposer and HAT. Expect in the
   log `eth: no W5500 on SCLK 10 / MOSI 3 / MISO 16 / CS 21 (VERSIONR 0xff
   …)` (0x00 is fine too), `ethStatus: "Not fitted"` in the API, the
   Network tab's Connection block with "no Ethernet HAT found", Wi-Fi and
   the pool as before, the hotspot behaviour unchanged (unplug the Wi-Fi
   network: the hotspot opens after three minutes as before). Mine for an
   hour and compare the hashrate and error rate with the previous image.
2. **HAT fitted, no cable**: `eth: W5500 found on SCLK 10 / MOSI 3 / MISO
   16 / CS 21, reset over SPI; polled every 1 ms`, `eth: started, MAC
   xx:…` (the efuse Ethernet MAC, not the Wi-Fi one), `ethStatus: "No
   link"`. The LDO (GPIO12) and the core rail behave as without the HAT:
   watch the regulator's input voltage and `power_fault` on the dashboard
   for the first minutes.
3. **Cable in**: `eth: link up, 100 Mbit/s full duplex`, then `eth:
   address 192.168.x.y, the default route is the cable`; the router's
   DHCP list shows the miner's hostname on a new MAC; `ethStatus:
   "Connected!"`, `netInterface: "ethernet"`; the dashboard cell shows the
   plug and "100 Mbit/s", the pool page's corner "Ethernet · 100 Mbit/s".
   Open the web UI on the Ethernet address and on the Wi-Fi address: both
   work. `<hostname>.local` resolves (from a computer on the LAN; check
   which address it gives).
4. **Pool over the cable**: 30 s after the address, `stratum: the pool
   session runs over 192.168.x.w and the Ethernet cable has held
   192.168.x.y for 30 s: moving it, reconnecting`, one new session in the
   log. `stratum.reconnects` does not count this (it counts sessions that
   ended with an error); the log and `/api/v1/state` → `stratum.local_ip`
   do: the local address is now the Ethernet one. Test with the Stratum
   V1 pool (TLS as configured) and once more with a V2 pool (`sv2_proto`);
   the local address must be the cable's in both cases.
5. **Cable out while mining**: `eth: link down`, `eth: address gone…`;
   the session's socket is aborted by lwIP and the next session connects
   within seconds (one `stratum:` error line, `stratum.reconnects` +1);
   `stratum.local_ip` is the Wi-Fi address; shares keep coming; the
   display's corner goes back to SSID · dBm. One new session, not a
   series; `stratum: the pool session was on the Ethernet cable…` appears
   only if the session was still up when the eth thread noticed.
6. **Cable back in**: address within a few seconds, one move after 30 s,
   `local_ip` the cable's again. Then pull and re-insert the cable three
   times within 30 s: no new session from the re-insertions beyond the
   one each removal forces (`stratum.reconnects` grows by one per
   removal, no "moving it" line until the cable has held 30 s).
6a. **Port without DHCP**: cable into a switch port with no DHCP server
   (or the router's DHCP paused): `eth: link up`, no address; after 60 s
   `eth: link up for 60 s without an address: restarting the driver…`,
   then link up again, and so on every minute; meanwhile the web UI over
   Wi-Fi and the pool over Wi-Fi keep working (this is the case where the
   default route would otherwise be stuck on the cable).
7. **Ethernet only**: clear the Wi-Fi name in the settings (or set a
   wrong one) and restart with the cable in. No hotspot opens, not even
   briefly in the first seconds (watch the display for three minutes and
   the log for `setup hotspot`); the pool connects over the cable; the
   web UI works on the Ethernet address. Then pull the cable: with no
   Wi-Fi network the hotspot opens at once and closes a minute after the
   cable is back. Then restart with the HAT fitted, no cable and no
   network in the settings: the hotspot opens after three minutes, as on
   a board without the HAT.
8. **Update and price**: run an online update check (System tab) and
   watch the price tile; both over the cable (`netInterface: "ethernet"`
   at the time). Install an update over the cable once.
9. **Soak**: a day with the cable in, then a day on Wi-Fi with the HAT
   fitted; hashrate, error rate and `freeHeapInternal` compared with the
   image before. The 1 ms poll task costs some CPU; watch for any change
   in the chain's job pace (`jobInterval` is unaffected by design, but the
   figures will show).
10. **The Octaxe rev 3.1**: steps 1–3 and 5; the log must not say
    anything about a TMP451 (rev 3.1 has none). The mux check is a probe
    of 0x4C and 0x4E on the Nerd bus: on the Octaxe rev 3.1 and the
    NerdQAxe++ nothing answers there (their sensors are TMP1075 at 0x48…
    and the EMC2302 at 0x2E).

When the Gaia passes 1–9 it goes into `detect::TESTED`'s note and the
README's board table with the date; it is in `TESTED` already for the
board itself.

## Open points

- **A board chosen by hand** runs its first boot without Ethernet (the
  choice is unverified until the chain has answered); the second boot
  has it. One more line in the log, nothing to do.
- **TMP451 at boot.** The mux probe runs with the Octaxe's rails off (the
  LDO is low from `board::quiesce`). NerdOS probes the TMP451s after its
  power-up. If the TMP451s hang on the chips' I/O rail rather than the
  always-on 3.3 V, they do not answer at boot and a rev 3.4 board would
  get the HAT probed (a few SPI clocks on the mux selects, then the
  driver if a HAT is fitted). The rev 3.4 schematic, or a rev 3.4 board
  with the rails off, settles this; until then rev 3.4 owners should not
  fit a HAT, which NerdOS also refuses there.
- **mDNS on the cable** is the component's doing (predefined `ETH_DEF`
  netif); confirmed only on the device.
- **DNS servers** are global in lwIP: the DHCP answer that came last sets
  them. With Ethernet and Wi-Fi on the same router that is the same
  server; two different networks are not covered by this round.
- **Link speed in the API** comes from `ETH_CMD_G_SPEED`; the W5500
  negotiates 10/100 on its own.

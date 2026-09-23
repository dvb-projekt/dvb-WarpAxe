# warpaxe-proof-usb

Stage-B hardware proof on a **LilyGO T-Display-S3** — the controller of every
Nerd miner (NerdAxe Gamma, NerdQAxe+/++, NerdOctaxe-γ, Gaia). Throwaway by
design; what it proves goes into the design document (§14).

It brings up, with **no Wi-Fi at all**:

1. the ST7789 (170 × 320, 8-bit i80 bus) with a status screen;
2. the USB-C socket as a **CDC-NCM network adapter** plus a CDC-ACM console
   — the USB-Serial/JTAG console is gone on purpose, that is the point;
3. a DHCP server on `192.168.7.1/24` and an HTTP server:
   `GET /` (status JSON), `GET /api/system/info` (a few AxeOS fields),
   `GET /bootloader` (reboot into ROM download mode so the board can be
   re-flashed without touching BOOT).

## Build

Detached from the workspace: Xtensa target, `std` on ESP-IDF v5.3.

```bash
espup install --targets esp32s3          # once; then source ~/export-esp.sh
cargo build --release
espflash flash --monitor target/xtensa-esp32s3-espidf/release/warpaxe-proof-usb
```

The first build downloads ESP-IDF and the `esp_tinyusb` component.

## The proof

| Step | Expected |
|---|---|
| flash, boot | screen: `USB: WAITING` |
| host plugs USB-A-to-C (CC pins are open on LilyGO's board, C-to-C is unreliable) | new network interface on the host, DHCP lease from 192.168.7.1; screen: `USB: MOUNTED` |
| `curl http://192.168.7.1/` | status JSON, `HTTP HITS` on screen counts up |
| `curl http://192.168.7.1/bootloader` | board reboots into ROM download mode; `espflash` / `esptool` see `USB JTAG/serial debug unit` again |
| flash the backup / NerdMiner | the NerdMiner is back — the flash-back path is proven |

## Status 2026-09-23: proven

On the NerdMiner V2's T-Display-S3, `host-proof.sh` passes all six steps
(macOS 26, USB-A-to-C cable): NCM interface with a DHCP lease from the
board, `GET /` and `GET /api/system/info` over USB, hit counter climbing,
`GET /bootloader` putting the chip into ROM download mode, esptool talking
to it. No Wi-Fi involved at any point.

What it took (all recorded in the design document, §14.8):

- `esp_netif_driver_ifconfig_t.transmit_wrap` must be set as well as
  `transmit`: the borrowed Wi-Fi-AP netstack calls the wrap variant, and a
  NULL there crashes the tcpip thread on the first gratuitous ARP.
- TinyUSB's NCM driver never calls `tud_network_init_cb`; link readiness
  is the first received frame.
- Before rebooting into the ROM bootloader, `tinyusb_driver_uninstall()`
  hands the USB PHY back to USB-Serial/JTAG (the selection lives in the
  RTC domain and survives a software reset). `RTC_CNTL_FORCE_DOWNLOAD_BOOT`
  then does the rest. Without this, a software reset leaves the board
  unreachable until a power cycle.
- On this board every DTR/RTS reset through USB-Serial/JTAG lands in ROM
  download mode; `espflash flash -a watchdog-reset` starts the app.
- `RTC_CNTL_FORCE_DOWNLOAD_BOOT` (RTC domain) is not cleared by the ROM:
  after `/bootloader` the host must write 0 to `RTC_CNTL_OPTION1_REG`
  (`esptool write_mem 0x6000812C 0 0xFFFFFFFF`) before the reset that
  should start the app. `host-proof.sh` step 7 does that; the round trip
  passed three times in a row.
- esp-idf-sys does not rebuild a local C component on its own: touch
  `sdkconfig.defaults` (tracked) to force it. espflash skips segments it
  believes unchanged: `--no-skip` for a real write.
- Core dump to flash plus `esp_core_dump_get_summary` rendered on the
  display is a working crash diagnosis without a console.

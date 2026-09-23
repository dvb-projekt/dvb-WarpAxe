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

## Status 2026-09-23 (work in progress)

Done on the NerdMiner V2's T-Display-S3: display up, board enumerates as a
composite CDC-ACM + CDC-NCM device, macOS attaches `AppleUSBNCMData`, brings
the interface up and selects alternate setting 1. Open: the host's OUT
transfers fail (`netstat -I enX` shows `Oerrs`, `Ipkts 0`), the board's
receive callback is never called, so no DHCP, no HTTP yet. TinyUSB in
slave mode did not change that. Build 17 routes TinyUSB's driver log into
the on-display ring buffer to see why NTBs are dropped.

Board quirks learned the hard way, all recorded in the design document:

- **Every DTR/RTS reset through USB-Serial/JTAG lands in ROM download mode
  on this board** (`boot:0x3`), including espflash's default post-flash
  reset. Use `espflash flash -a watchdog-reset` to start the app.
- Changes to `sdkconfig.defaults` only take effect after deleting the
  generated `target/**/esp-idf-sys-*/out/sdkconfig`; esp-idf-sys may then
  create a second `out` directory — take bootloader and partition table
  from the one whose `sdkconfig` has the option.
- A `B` byte on the CDC console port reboots into the ROM download mode.

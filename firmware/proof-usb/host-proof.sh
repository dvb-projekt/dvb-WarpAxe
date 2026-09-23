#!/bin/zsh
# Host side of the proof, macOS. Run after flashing warpaxe-proof-usb and
# plugging the board in with a USB-A-to-C cable. Read-only except for the
# GET /bootloader at the end, which the board answers by rebooting into
# ROM download mode. Each step prints PASS or FAIL; the script's exit code
# is the number of failures.
set -u
fails=0
step() { print -- "== $1"; }
pass() { print -- "   PASS: $1"; }
fail() { print -- "   FAIL: $1"; fails=$((fails+1)); }

step "1. USB network interface with a 192.168.7.x lease"
iface=""
for i in {1..30}; do
  iface=$(ifconfig 2>/dev/null | awk '/^en[0-9]+:/{i=$1} /inet 192\.168\.7\./{print i; exit}' | tr -d ':')
  [[ -n "$iface" ]] && break
  sleep 1
done
if [[ -n "$iface" ]]; then
  pass "interface $iface: $(ifconfig "$iface" | awk '/inet 192.168.7/{print $2}')"
  networksetup -listallhardwareports 2>/dev/null | grep -B1 -A1 "Device: $iface" | sed 's/^/   /'
else
  fail "no interface with a 192.168.7.x address within 30 s"
fi

step "2. HTTP status from 192.168.7.1"
body=$(curl -s -m 5 http://192.168.7.1/)
if print -- "$body" | grep -q '"device":"warpaxe-proof-usb"'; then
  pass "$body"
else
  fail "no valid answer: '$body'"
fi

step "3. AxeOS-style info endpoint"
info=$(curl -s -m 5 http://192.168.7.1/api/system/info)
if print -- "$info" | grep -q '"hostname":"warpaxe-proof"'; then pass "$info"; else fail "'$info'"; fi

step "4. Ten more requests, hit counter must climb"
for i in {1..10}; do curl -s -m 3 -o /dev/null http://192.168.7.1/; done
hits=$(curl -s -m 5 http://192.168.7.1/ | sed -n 's/.*"http_hits":\([0-9]*\).*/\1/p')
if [[ "${hits:-0}" -ge 12 ]]; then pass "http_hits=$hits"; else fail "http_hits=$hits"; fi

step "5. Flash-back path: GET /bootloader, then the ROM's USB-Serial/JTAG must appear"
before=$(ls /dev/cu.usbmodem* 2>/dev/null | wc -l | tr -d ' ')
curl -s -m 5 http://192.168.7.1/bootloader
port=""
for i in {1..20}; do
  sleep 1
  port=$(ls -t /dev/cu.usbmodem* 2>/dev/null | head -1)
  if [[ -n "$port" ]] && ioreg -p IOUSB -l -w0 2>/dev/null | grep -q 'USB JTAG_serial debug unit'; then break; fi
  port=""
done
if [[ -n "$port" ]]; then
  pass "ROM download mode on $port"
  step "6. esptool talks to the ROM (read-only chip id)"
  if python3 -m esptool --port "$port" --chip esp32s3 --before no_reset --after no_reset chip_id 2>&1 | grep -q 'Chip is ESP32-S3'; then
    pass "esptool chip_id ok — the board can be re-flashed without a finger on BOOT"
  else
    fail "esptool could not talk to the ROM"
  fi
else
  fail "no USB-Serial/JTAG device after /bootloader (usbmodem count before: $before). Manual path: hold BOOT, press RST."
fi

step "7. Back to the app: clear the force-download flag (RTC domain, survives resets), watchdog reset"
if [[ -n "$port" ]]; then
  python3 -m esptool --port "$port" --chip esp32s3 --before no_reset --after no_reset --no-stub write_mem 0x6000812C 0 0xFFFFFFFF >/dev/null 2>&1
  python3 -m esptool --port "$port" --chip esp32s3 --before no_reset --after watchdog_reset --no-stub chip_id >/dev/null 2>&1
  back=""
  for i in {1..45}; do sleep 1; back=$(ifconfig 2>/dev/null | awk '/inet 192\.168\.7\./{print $2}'); [[ -n "$back" ]] && break; done
  if [[ -n "$back" ]] && curl -s -m 5 http://192.168.7.1/ | grep -q warpaxe-proof-usb; then
    pass "app is back on USB with lease $back after ${i}s — the round trip is closed"
  else
    fail "app did not come back within 45 s"
  fi
else
  fail "skipped: no ROM port from step 5"
fi

print -- "== failures: $fails"
exit $fails

# esp-idf-svc 0.51.0, patched for WarpAxe

Unchanged copy of the crates.io release `esp-idf-svc 0.51.0`
(MIT OR Apache-2.0, https://github.com/esp-rs/esp-idf-svc) with one change
in `src/wifi.rs`:

- `WifiEvent` has a new variant `Unknown(u32)`, and the event deserializer
  returns it for event IDs it does not know instead of `panic!`.

Why: ESP-IDF 5.3 posts `WIFI_EVENT_AP_WRONG_PASSWORD` when a station tries
to join the soft AP with a wrong password. 0.51.0 does not know that event;
its deserializer runs for every Wi-Fi event in the `sys_evt` task, so a
phone typing the setup-hotspot password wrong aborted the firmware
(found on a NerdQAxe++ with 0.1.4-rc2, 2026-09-28). esp-idf-svc 0.52+ knows
the event and logs unknown ones instead of panicking; drop this copy when
the firmware moves to it.

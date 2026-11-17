# esp-idf-svc 0.51.0, patched for WarpAxe

Unchanged copy of the crates.io release `esp-idf-svc 0.51.0`
(MIT OR Apache-2.0, https://github.com/esp-rs/esp-idf-svc) with three changes.

## 1. `src/wifi.rs`

- `WifiEvent` has a new variant `Unknown(u32)`, and the event deserializer
  returns it for event IDs it does not know instead of `panic!`.

Why: ESP-IDF 5.3 posts `WIFI_EVENT_AP_WRONG_PASSWORD` when a station tries
to join the soft AP with a wrong password. 0.51.0 does not know that event;
its deserializer runs for every Wi-Fi event in the `sys_evt` task, so a
phone typing the setup-hotspot password wrong aborted the firmware
(found on a NerdQAxe++ with 0.1.4-rc2, 2026-09-28). esp-idf-svc 0.52+ knows
the event and logs unknown ones instead of panicking; drop this copy when
the firmware moves to it.

## 2. `src/tls.rs`

`tls::Config` has a new field `crt_bundle_attach`: an attach function used
in place of `esp_crt_bundle_attach` when `use_crt_bundle_attach` is on
(`None`, the default, keeps ESP-IDF's). The HTTP client already takes one.

Why: the firmware's pool connection attaches `tls_trust::attach`, which
installs a verify callback that also trusts a root of the pinned bundle the
server sends along, cross-signed by a root the bundle no longer has
(crates/trust). Added 2026-11-04 for v0.1.40.

## 3. `src/http/server.rs`

`http::server::Configuration` has a new field `open_fn`, passed through to
`httpd_config_t::open_fn` (`None`, the default, as before).

Why: with IPv6 (docs/17) the miner holds a global address, and its web UI
and API, which have no login, would answer anyone the router's IPv6
firewall lets through. NAT used to hide them. The firmware's open
function refuses connections from global IPv6 addresses outside the
miner's own /64 (api.rs `local_only`). Added 2026-11-17 for v0.1.46.

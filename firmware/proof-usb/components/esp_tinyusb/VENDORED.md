Vendored copy of `espressif/esp_tinyusb` 1.7.6~2 (esp-usb commit
c0be948c1acc6ee1f7ef00ab183c571971fccefd, `device/esp_tinyusb`), unmodified.

Why a copy and not the managed component: esp-idf-sys's default bindings
header includes `tinyusb.h` whenever a component named
`espressif__esp_tinyusb` is in the build, and bindgen then generates a
packed struct containing an aligned union from TinyUSB's `cdc.h`, which
rustc rejects (E0588). Under the local name `esp_tinyusb` that include
block is skipped; the proof's C glue (`warpaxe_glue`) is the only code
that includes TinyUSB headers. The core `espressif/tinyusb` component is
still fetched by the component manager via this copy's idf_component.yml.

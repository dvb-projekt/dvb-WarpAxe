# warpaxe-drivers

Small `no_std` drivers for the parts around a BM13xx miner board, from
[dvb-WarpAxe](https://git.warppool.org/dvb-projekt/dvb-WarpAxe)
(mirror: [GitHub](https://github.com/dvb-projekt/dvb-WarpAxe)):

| Module | Part |
|---|---|
| `i2c` | the `I2c` trait the drivers use (write, write_read, read), with a scripted test bus behind the `mock` feature |
| `ina260` | INA260 power monitor: bus voltage, current, power |
| `tmp1075` | TMP1075 temperature sensor, with detection of the NerdQAxe boards' sensor layout |
| `pca9544` | PCA9544 four-channel I²C multiplexer |
| `ssd1306` | SSD1306 128×32 OLED with its own frame buffer |
| `font` | a 5×7 font for the OLED |
| `pmbus` | PMBus command codes and the LINEAR11 / ULINEAR16 number formats |
| `ntc` | NTC thermistor on a voltage divider (β equation) |

The `I2c` trait is close to embedded-hal 1.0's `I2c` (`write`,
`write_read`, `read`).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. The rest of WarpAxe is GPL-3.0-or-later. The drivers
were written from the datasheets and published board schematics; the
font is a widely circulated 5×7 table.

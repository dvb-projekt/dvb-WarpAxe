# Bitaxe wordmark

The boot screen of the Bitaxe boards with a colour display (Naja Duo,
Gamma Hex) shows the Bitaxe wordmark under the WarpAxe logo, as AxeOS
does on these boards. `tools/make-splash.py` turns the file into a
display sprite:

| File | Boards | Taken from |
|---|---|---|
| `bitaxe-wordmark.png` | Naja Duo, Gamma Hex | AxeOS `main/images/BitaxeLogoLarge.png` |

Source, GPLv3: [bitaxeorg/ESP-Miner](https://github.com/bitaxeorg/ESP-Miner),
`main/images/BitaxeLogoLarge.png` (added with the Naja Duo and Gamma Hex
display support, #1916), unchanged.

The Bitaxe boards with the small monochrome display (Gamma, Gamma Duo,
Gamma Turbo) keep the model name in text: the 128×32 screen has no room
for a second mark.

skot (Bitaxe) agreed in a private message on the OSMU forum on
2026-09-30 that the Bitaxe name and logo can be used in open source
Bitcoin mining projects, and so on WarpAxe's boot screen.

#!/bin/sh
# Build the bench for one target and merge it into a single image:
#   tools/sha-bench/build.sh esp32p4
#   -> build-esp32p4/sha-bench-esp32p4.bin (flash at 0x0) and its .sha256
# Each target builds in its own directory with its own sdkconfig, so the
# two can live side by side.
#
# The environment: ESP-IDF v5.3.3 as esp-idf-sys installs it for the
# firmware (~/.espressif, IDF_PATH may point elsewhere), with the
# xtensa and riscv32 toolchains, cmake and ninja from ~/.espressif/tools.
# ESP-IDF's own export.sh is not used: it insists on gdb and openocd,
# which that install has not got.
set -e
target="${1:?usage: build.sh esp32p4|esp32s3}"
cd "$(dirname "$0")"
esp="$HOME/.espressif"
IDF_PATH="${IDF_PATH:-$esp/esp-idf/v5.3.3}"
export IDF_PATH
IDF_PYTHON_ENV_PATH="${IDF_PYTHON_ENV_PATH:-$(ls -d "$esp"/python_env/idf5.3_py3.12_env 2>/dev/null | head -1)}"
export IDF_PYTHON_ENV_PATH
latest() { ls -d "$@" 2>/dev/null | sort | tail -1; }
PATH="$IDF_PATH/tools:$IDF_PYTHON_ENV_PATH/bin:$(latest "$esp"/tools/riscv32-esp-elf/*/riscv32-esp-elf/bin):$(latest "$esp"/tools/xtensa-esp-elf/*/xtensa-esp-elf/bin):$(latest "$esp"/tools/cmake/*/CMake.app/Contents/bin):$(latest "$esp"/tools/ninja/*):$PATH"
export PATH
# The ROM symbol files, for the gdbinit the build writes (export.sh sets this too).
ESP_ROM_ELF_DIR="$(latest "$esp"/tools/esp-rom-elfs/*)/"
export ESP_ROM_ELF_DIR
build="build-$target"
image="sha-bench-$target.bin"
idf.py -B "$build" -D "SDKCONFIG=$build/sdkconfig" set-target "$target" build
idf.py -B "$build" -D "SDKCONFIG=$build/sdkconfig" merge-bin -o "$image"
(cd "$build" && shasum -a 256 "$image" | tee "$image.sha256")

# The SHA peripheral bench as a project of its own (README.md): the
# firmware's bench code compiled for an ESP32-S3 or an ESP32-P4 without
# the rest of the firmware. `./build.sh esp32p4` or `./build.sh esp32s3`.
cmake_minimum_required(VERSION 3.16)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(sha-bench)

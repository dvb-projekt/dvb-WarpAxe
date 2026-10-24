# Glue component: the only place that includes TinyUSB headers (see
# warpaxe_glue.h for why), plus the esp_tinyusb dependency via
# idf_component.yml.
# warpaxe_cpuhash.c is the software hash of the T-Display-S3 without chips
# (src/cpuminer.rs), 1.1 KB of IRAM; warpaxe_shabench.c measures its SHA
# peripheral.
idf_component_register(SRCS "warpaxe_glue.c" "warpaxe_cpuhash.c" "warpaxe_shabench.c"
                       INCLUDE_DIRS "include"
                       REQUIRES esp_tinyusb esp_lcd esp_netif esp_event esp_system esp_hw_support espcoredump spi_flash esp_vfs_console)
# The hash loop wants speed; the rest of the image keeps the project's
# size-optimised flags.
set_source_files_properties(warpaxe_cpuhash.c warpaxe_shabench.c PROPERTIES COMPILE_OPTIONS "-O2")

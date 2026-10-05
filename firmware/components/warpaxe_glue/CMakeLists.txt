# Glue component: the only place that includes TinyUSB headers (see
# warpaxe_glue.h for why), plus the esp_tinyusb dependency via
# idf_component.yml.
# warpaxe_cpuhash.c is the devkit toy's software hash (firmware feature
# cpu-miner); nothing references it in a release build, so its object is
# never linked there.
idf_component_register(SRCS "warpaxe_glue.c" "warpaxe_cpuhash.c"
                       INCLUDE_DIRS "include"
                       REQUIRES esp_tinyusb esp_lcd esp_netif esp_event esp_system espcoredump spi_flash)

# Glue component: the only place that includes TinyUSB headers (see
# warpaxe_glue.h for why), plus the esp_tinyusb dependency via
# idf_component.yml.
idf_component_register(SRCS "warpaxe_glue.c"
                       INCLUDE_DIRS "include"
                       REQUIRES esp_tinyusb esp_lcd esp_netif esp_event esp_system)

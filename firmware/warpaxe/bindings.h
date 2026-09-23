// Headers esp-idf-sys does not bind by default: the proof's own C glue
// (which hides TinyUSB) and the LCD panel API for the ST7789. esp_netif
// and friends come from the default bindings — keep them out of here so
// their types are not generated twice.
#include "warpaxe_glue.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_vendor.h"
#include "esp_lcd_panel_ops.h"

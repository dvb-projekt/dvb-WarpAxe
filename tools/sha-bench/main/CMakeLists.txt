# The hash code is the firmware's own (firmware/components/warpaxe_glue):
# the same files, not a copy, so the standalone bench and the firmware's
# bench endpoint can never drift apart.
set(glue "${CMAKE_CURRENT_LIST_DIR}/../../../firmware/components/warpaxe_glue")
idf_component_register(SRCS "main.c" "${glue}/warpaxe_cpuhash.c" "${glue}/warpaxe_shabench.c"
                       INCLUDE_DIRS "." "${glue}/include" "${glue}"
                       PRIV_REQUIRES mbedtls esp_hw_support esp_mm esp_system)
# The hash loops want speed whatever the project's optimisation level
# (the firmware builds them with -O2 too).
set_source_files_properties("${glue}/warpaxe_cpuhash.c" "${glue}/warpaxe_shabench.c" "main.c"
                            PROPERTIES COMPILE_OPTIONS "-O2")

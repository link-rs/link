# ESP-IDF component wrapper for libquicr
#
# This component builds libquicr and its dependencies (picoquic, picotls, mbedtls)
# as static libraries for ESP32-S3.
#
# Based on: https://github.com/Quicr/hactar/blob/main/firmware/net/components/quicr/CMakeLists.txt

# Required IDF components
set(priv_requires
    pthread
    lwip
    mbedtls
)

# Register as an ESP-IDF component
idf_component_register(
    PRIV_INCLUDE_DIRS
    PRIV_REQUIRES ${priv_requires}
    REQUIRED_IDF_TARGETS esp32s3
)

# Set libquicr build options
set(PLATFORM_ESP_IDF ON CACHE BOOL "Building for ESP-IDF" FORCE)
set(USE_MBEDTLS ON CACHE BOOL "Use mbedTLS" FORCE)
set(QUICR_BUILD_TESTS OFF CACHE BOOL "Disable tests" FORCE)
set(quicr_BUILD_BENCHMARKS OFF CACHE BOOL "Disable benchmarks" FORCE)
set(QUICR_BUILD_SHARED OFF CACHE BOOL "No shared libs" FORCE)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "No shared libs" FORCE)
set(BUILD_STATIC_LIBS ON CACHE BOOL "Build static libs" FORCE)

# Point to ESP-IDF's mbedtls
set(MBEDTLS_PREFIX "${IDF_PATH}/components/mbedtls/mbedtls" CACHE PATH "MbedTLS path")

# Add libquicr subdirectory
add_subdirectory(/build/libquicr libquicr_build)

# Link quicr to this component
target_link_libraries(${COMPONENT_LIB} INTERFACE quicr)

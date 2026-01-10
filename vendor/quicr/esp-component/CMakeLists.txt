# ESP-IDF Component for libquicr
# Based on hactar project: https://github.com/Quicr/hactar
#
# Usage: Add this directory as an ESP-IDF component in your project.
# In your main CMakeLists.txt:
#   set(EXTRA_COMPONENT_DIRS "/path/to/quicr-rs/esp-component")
#
# Or symlink/copy to your project's components/ directory.

# Required ESP-IDF components
set(priv_requires
    pthread
    lwip
    mbedtls
)

idf_component_register(
    PRIV_INCLUDE_DIRS
    PRIV_REQUIRES ${priv_requires}
    REQUIRED_IDF_TARGETS esp32s3 esp32 esp32s2 esp32c3 esp32c6
)

# Path to libquicr source
set(LIBQUICR_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../libquicr")

# Path to FFI wrapper source
set(FFI_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../ffi")

# BSD socket compatibility headers (netinet/udp.h, sys/prctl.h)
# Based on hactar project approach - these stubs satisfy picoquic includes
set(COMPAT_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/include")
include_directories(BEFORE ${COMPAT_INCLUDE_DIR})

# Platform and TLS configuration
set(PLATFORM_ESP_IDF ON CACHE BOOL "" FORCE)
set(USE_MBEDTLS ON CACHE BOOL "" FORCE)
add_definitions(-DPLATFORM_ESP_IDF)

# Compiler flags for ESP-IDF compatibility
set(CMAKE_C_FLAGS "${CMAKE_C_FLAGS} -Wno-error=format -Wno-error")
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -Wno-error=format -Wno-error=pessimizing-move -Wno-error")
set(CMAKE_CXX_STANDARD 20)

# libquicr build options
set(QUICR_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(quicr_BUILD_BENCHMARKS OFF CACHE BOOL "" FORCE)
set(QUICR_BUILD_C_BRIDGE OFF CACHE BOOL "" FORCE)
set(QUICR_BUILD_SHARED OFF CACHE BOOL "" FORCE)

# picoquic options for ESP-IDF
set(BUILD_SOCKLOOP OFF CACHE BOOL "" FORCE)
set(BUILD_DEMO OFF CACHE BOOL "" FORCE)
set(BUILD_HTTP ON CACHE BOOL "" FORCE)
set(BUILD_LOGREADER OFF CACHE BOOL "" FORCE)
set(BUILD_TESTS OFF CACHE BOOL "" FORCE)

# picotls options
set(WITH_FUSION OFF CACHE BOOL "" FORCE)
set(WITH_BROTLI OFF CACHE BOOL "" FORCE)

# spdlog options for embedded
# NOTE: SPDLOG_NO_EXCEPTIONS must be OFF because libquicr uses exceptions
# NOTE: SPDLOG_DISABLE_DEFAULT_LOGGER must be OFF because libquicr uses the default logger
set(SPDLOG_NO_EXCEPTIONS OFF CACHE BOOL "" FORCE)
set(SPDLOG_NO_THREAD_ID ON CACHE BOOL "" FORCE)
set(SPDLOG_NO_TLS ON CACHE BOOL "" FORCE)
set(SPDLOG_DISABLE_DEFAULT_LOGGER OFF CACHE BOOL "" FORCE)

# mbedtls - use ESP-IDF's mbedtls, don't build bundled
set(ENABLE_TESTING OFF CACHE BOOL "" FORCE)
set(ENABLE_PROGRAMS OFF CACHE BOOL "" FORCE)

# Cross-compile settings
set(HAVE_FWRITE_UNLOCKED 0 CACHE STRING "" FORCE)

# POSIX and wide-character compatibility stubs for ESP-IDF
# These provide missing functions that libstdc++ and picoquic expect
add_library(compat_stubs STATIC
    ${CMAKE_CURRENT_SOURCE_DIR}/compat_stubs.c
)
# Ensure whole archive is linked to provide symbols to libstdc++
set_target_properties(compat_stubs PROPERTIES
    COMPILE_FLAGS "-fvisibility=default"
)

# Add libquicr as subdirectory
add_subdirectory(${LIBQUICR_DIR} libquicr)

# Link compat stubs to picoquic-core to provide missing POSIX functions
target_link_libraries(picoquic-core PRIVATE compat_stubs)

# Enable C++ exceptions and RTTI for libquicr and dependencies
# ESP-IDF defaults to -fno-exceptions -fno-rtti but libquicr needs both:
# - exceptions: libquicr uses throw statements
# - rtti: libquicr uses dynamic_cast
target_compile_options(quicr PRIVATE -fexceptions -frtti)
target_compile_options(spdlog PRIVATE -fexceptions)

# Link against the component library
# Note: ESP-IDF provides libstdc++ - don't link it explicitly to avoid conflicts
target_link_libraries(${COMPONENT_LIB} INTERFACE quicr)

# Also build and link the FFI wrapper if present
if(EXISTS "${FFI_DIR}/src/quicr_ffi.cpp")
    add_library(quicr_ffi STATIC
        ${FFI_DIR}/src/quicr_ffi.cpp
    )
    target_include_directories(quicr_ffi PUBLIC
        ${FFI_DIR}/include
        ${LIBQUICR_DIR}/include
    )
    target_link_libraries(quicr_ffi PUBLIC quicr)
    target_compile_definitions(quicr_ffi PRIVATE PLATFORM_ESP_IDF)
    target_compile_options(quicr_ffi PRIVATE -fexceptions -frtti)

    target_link_libraries(${COMPONENT_LIB} INTERFACE quicr_ffi)
endif()

# Export include directories for dependents
target_include_directories(${COMPONENT_LIB} INTERFACE
    ${LIBQUICR_DIR}/include
    ${FFI_DIR}/include
)

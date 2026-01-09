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

# Platform and TLS configuration
set(PLATFORM_ESP_IDF ON)
set(USE_MBEDTLS ON)
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
set(SPDLOG_NO_EXCEPTIONS ON CACHE BOOL "" FORCE)
set(SPDLOG_NO_THREAD_ID ON CACHE BOOL "" FORCE)
set(SPDLOG_NO_TLS ON CACHE BOOL "" FORCE)
set(SPDLOG_DISABLE_DEFAULT_LOGGER ON CACHE BOOL "" FORCE)

# mbedtls - use ESP-IDF's mbedtls, don't build bundled
set(ENABLE_TESTING OFF CACHE BOOL "" FORCE)
set(ENABLE_PROGRAMS OFF CACHE BOOL "" FORCE)

# Cross-compile settings
set(HAVE_FWRITE_UNLOCKED 0)

# Add libquicr as subdirectory
add_subdirectory(${LIBQUICR_DIR} libquicr)

# Link against the component
target_link_libraries(${COMPONENT_LIB} INTERFACE quicr)

# Export include directories for dependents
target_include_directories(${COMPONENT_LIB} INTERFACE
    ${LIBQUICR_DIR}/include
    ${LIBQUICR_DIR}/src
)

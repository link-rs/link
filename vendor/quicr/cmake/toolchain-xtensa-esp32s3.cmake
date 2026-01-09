# CMake toolchain file for cross-compiling to ESP32-S3 (Xtensa)
#
# This toolchain file configures CMake to use the Espressif xtensa-esp-elf
# toolchain for building libquicr and its dependencies for ESP32-S3.
#
# Usage:
#   cmake -DCMAKE_TOOLCHAIN_FILE=cmake/toolchain-xtensa-esp32s3.cmake ..

set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR xtensa)

# Find the toolchain
# First check environment variable, then common installation paths
if(DEFINED ENV{XTENSA_TOOLCHAIN})
    set(TOOLCHAIN_PREFIX "$ENV{XTENSA_TOOLCHAIN}")
elseif(EXISTS "$ENV{HOME}/.espressif/tools/xtensa-esp-elf")
    # Find the latest version
    file(GLOB TOOLCHAIN_VERSIONS "$ENV{HOME}/.espressif/tools/xtensa-esp-elf/esp-*")
    if(TOOLCHAIN_VERSIONS)
        list(SORT TOOLCHAIN_VERSIONS)
        list(GET TOOLCHAIN_VERSIONS -1 TOOLCHAIN_VERSION_DIR)
        set(TOOLCHAIN_PREFIX "${TOOLCHAIN_VERSION_DIR}/xtensa-esp-elf")
    endif()
else()
    message(FATAL_ERROR "Could not find Xtensa toolchain. Set XTENSA_TOOLCHAIN environment variable.")
endif()

message(STATUS "Using Xtensa toolchain: ${TOOLCHAIN_PREFIX}")

# Toolchain binaries
set(CMAKE_C_COMPILER "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-gcc")
set(CMAKE_CXX_COMPILER "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-g++")
set(CMAKE_ASM_COMPILER "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-gcc")
set(CMAKE_AR "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-ar")
set(CMAKE_RANLIB "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-ranlib")
set(CMAKE_OBJCOPY "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-objcopy")
set(CMAKE_OBJDUMP "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-objdump")
set(CMAKE_SIZE "${TOOLCHAIN_PREFIX}/bin/xtensa-esp-elf-size")

# ESP32-S3 specific flags
# -mlongcalls: Required for code larger than 256KB
# -mtext-section-literals: Place literals in text section
set(ESP32S3_FLAGS "-mlongcalls -mtext-section-literals")

# Compile flags for embedded target
# Include -Wno-error to allow warnings that occur due to header redefinitions
# Note: C++ exceptions and RTTI are enabled for libquicr which requires them
set(CMAKE_C_FLAGS_INIT "${ESP32S3_FLAGS} -ffunction-sections -fdata-sections -Wno-error")
set(CMAKE_CXX_FLAGS_INIT "${ESP32S3_FLAGS} -ffunction-sections -fdata-sections -std=c++17 -Wno-error")
set(CMAKE_EXE_LINKER_FLAGS_INIT "-Wl,--gc-sections")

# Don't try to run test executables
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

# Search paths
set(CMAKE_FIND_ROOT_PATH ${TOOLCHAIN_PREFIX})
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

# ESP32 platform defines
add_definitions(-DESP_PLATFORM=1)
add_definitions(-DESP32S3=1)
add_definitions(-DQUICR_BAREMETAL=1)

# Disable features that don't work on embedded
# Note: Exceptions are enabled for libquicr C++ code
add_definitions(-DSPDLOG_NO_EXCEPTIONS=1)
add_definitions(-DSPDLOG_NO_TLS=1)

# mbedTLS bare-metal configuration
# Use a custom mbedtls config that disables NET_C, THREADING, etc.
set(MBEDTLS_USER_CONFIG_FILE "${CMAKE_CURRENT_LIST_DIR}/../libquicr/platform/baremetal/mbedtls_config.h" CACHE STRING "")
add_definitions(-DMBEDTLS_USER_CONFIG_FILE="${MBEDTLS_USER_CONFIG_FILE}")

# Also define these directly to ensure they're set before config is processed
add_definitions(-DMBEDTLS_PLATFORM_MS_TIME_ALT=1)

# Use mbedtls
set(USE_MBEDTLS ON CACHE BOOL "Use MbedTLS" FORCE)
set(USE_OPENSSL OFF CACHE BOOL "Use OpenSSL" FORCE)

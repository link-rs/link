#!/bin/bash
# Build libquicr and dependencies for ESP32-S3 using ESP-IDF
#
# This script runs inside the Docker container and:
# 1. Creates a minimal ESP-IDF project
# 2. Builds libquicr as a component
# 3. Extracts static libraries to /output
#
# Environment variables:
#   STD_MODE      - "true" for ESP-IDF std mode, "false" for bare-metal
#   OUTPUT_SUBDIR - Output subdirectory name (esp32s3 or esp32s3-std)

set -e

# Default values if not set
STD_MODE="${STD_MODE:-false}"
OUTPUT_SUBDIR="${OUTPUT_SUBDIR:-esp32s3}"

if [ "$STD_MODE" = "true" ]; then
    BUILD_MODE="ESP-IDF std"
else
    BUILD_MODE="bare-metal"
fi

echo "=== Building libquicr for ESP32-S3 ($BUILD_MODE) ==="
echo "ESP-IDF version: $(idf.py --version)"
echo "Output directory: /output/$OUTPUT_SUBDIR"

# Initialize libquicr submodules if needed
if [ ! -f "/build/libquicr/dependencies/picoquic/CMakeLists.txt" ]; then
    echo "Initializing libquicr submodules..."
    cd /build/libquicr
    git submodule update --init --recursive
fi

# Create a minimal ESP-IDF project structure
PROJECT_DIR=/build/project
mkdir -p "$PROJECT_DIR/main"
mkdir -p "$PROJECT_DIR/components/quicr"

# Copy the quicr component
cp /build/component/CMakeLists.txt "$PROJECT_DIR/components/quicr/"

# Create minimal main component
cat > "$PROJECT_DIR/main/CMakeLists.txt" << 'EOF'
idf_component_register(
    SRCS "main.c"
    INCLUDE_DIRS "."
    REQUIRES quicr
)
EOF

cat > "$PROJECT_DIR/main/main.c" << 'EOF'
// Minimal main for building quicr component
void app_main(void) {}
EOF

# Create project CMakeLists.txt
cat > "$PROJECT_DIR/CMakeLists.txt" << 'EOF'
cmake_minimum_required(VERSION 3.16)

# Disable warnings that break the build
set(CMAKE_C_FLAGS "${CMAKE_C_FLAGS} -Wno-format -Wno-unused-but-set-variable")
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -Wno-format -Wno-pessimizing-move -Wno-unused-but-set-variable")

# C++20 for libquicr
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Build static libs
set(BUILD_SHARED_LIBS OFF)
set(BUILD_STATIC_LIBS ON)

# libquicr options
set(PLATFORM_ESP_IDF ON)
set(USE_MBEDTLS ON)
set(QUICR_BUILD_TESTS OFF)
set(quicr_BUILD_BENCHMARKS OFF)
set(QUICR_BUILD_SHARED OFF)

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(quicr_builder)
EOF

# Create sdkconfig.defaults based on mode
if [ "$STD_MODE" = "true" ]; then
    cat > "$PROJECT_DIR/sdkconfig.defaults" << 'EOF'
# ESP-IDF std mode - full C++ support

# Enable C++ exceptions (required by libquicr in std mode)
CONFIG_COMPILER_CXX_EXCEPTIONS=y
CONFIG_COMPILER_CXX_EXCEPTIONS_EMG_POOL_SIZE=1024

# Enable C++ RTTI
CONFIG_COMPILER_CXX_RTTI=y

# Increase stack sizes for QUIC
CONFIG_ESP_MAIN_TASK_STACK_SIZE=8192
CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=4096

# Enable PSRAM if available
CONFIG_SPIRAM=y
CONFIG_SPIRAM_MODE_OCT=y
CONFIG_SPIRAM_SPEED_80M=y

# mbedTLS configuration for QUIC
CONFIG_MBEDTLS_SSL_PROTO_TLS1_3=y
CONFIG_MBEDTLS_SSL_TLS1_3_COMPATIBILITY_MODE=y

# Enable pthread support (for std threading)
CONFIG_PTHREAD_TASK_PRIO_DEFAULT=5
CONFIG_PTHREAD_TASK_CORE_DEFAULT=-1
EOF
else
    cat > "$PROJECT_DIR/sdkconfig.defaults" << 'EOF'
# Bare-metal mode - minimal C++ support

# Disable C++ exceptions for smaller binary
CONFIG_COMPILER_CXX_EXCEPTIONS=n

# Increase stack sizes for QUIC
CONFIG_ESP_MAIN_TASK_STACK_SIZE=8192
CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=4096

# Enable PSRAM if available
CONFIG_SPIRAM=y
CONFIG_SPIRAM_MODE_OCT=y
CONFIG_SPIRAM_SPEED_80M=y

# mbedTLS configuration for QUIC
CONFIG_MBEDTLS_SSL_PROTO_TLS1_3=y
CONFIG_MBEDTLS_SSL_TLS1_3_COMPATIBILITY_MODE=y
EOF
fi

# Build the project
cd "$PROJECT_DIR"
echo "Configuring ESP-IDF project..."
idf.py set-target esp32s3

echo "Building..."
idf.py build 2>&1 | tee /tmp/build.log || {
    echo "Build failed. Last 100 lines of log:"
    tail -100 /tmp/build.log
    exit 1
}

# Create output directory structure
OUTPUT_DIR="/output/$OUTPUT_SUBDIR"
mkdir -p "$OUTPUT_DIR/lib"
mkdir -p "$OUTPUT_DIR/include"

# Find and copy all built static libraries
echo "Collecting static libraries..."

# Main quicr library
find "$PROJECT_DIR/build" -name "libquicr.a" -exec cp {} "$OUTPUT_DIR/lib/" \;

# picoquic libraries
find "$PROJECT_DIR/build" -name "libpicoquic*.a" -exec cp {} "$OUTPUT_DIR/lib/" \;
find "$PROJECT_DIR/build" -name "libpicohttp*.a" -exec cp {} "$OUTPUT_DIR/lib/" \;

# picotls libraries
find "$PROJECT_DIR/build" -name "libpicotls*.a" -exec cp {} "$OUTPUT_DIR/lib/" \;

# We don't need to copy mbedtls - ESP-IDF provides it at runtime

# Copy headers
echo "Collecting headers..."
cp -r /build/libquicr/include/quicr "$OUTPUT_DIR/include/"

# Copy picoquic headers needed for FFI
mkdir -p "$OUTPUT_DIR/include/picoquic"
cp /build/libquicr/dependencies/picoquic/picoquic/*.h "$OUTPUT_DIR/include/picoquic/" 2>/dev/null || true

# Copy picotls headers
mkdir -p "$OUTPUT_DIR/include/picotls"
cp /build/libquicr/dependencies/picotls/include/*.h "$OUTPUT_DIR/include/picotls/" 2>/dev/null || true
cp -r /build/libquicr/dependencies/picotls/include/picotls "$OUTPUT_DIR/include/" 2>/dev/null || true

# List what we built
echo ""
echo "=== Build complete ==="
echo "Libraries:"
ls -lh "$OUTPUT_DIR/lib/"*.a 2>/dev/null || echo "  (no .a files found)"

echo ""
echo "Headers:"
find "$OUTPUT_DIR/include" -name "*.h" | head -20
echo "  ..."

# Create a manifest file
cat > "$OUTPUT_DIR/BUILD_INFO.txt" << EOF
libquicr ESP32-S3 prebuilt libraries
====================================

Built: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
ESP-IDF: $(idf.py --version)
Target: esp32s3
Mode: $BUILD_MODE
C++ Exceptions: $([ "$STD_MODE" = "true" ] && echo "enabled" || echo "disabled")

Libraries:
$(ls -1 "$OUTPUT_DIR/lib/"*.a 2>/dev/null | xargs -I{} basename {})

Source: libquicr $(cd /build/libquicr && git rev-parse --short HEAD 2>/dev/null || echo "unknown")
EOF

echo ""
echo "Build info saved to $OUTPUT_DIR/BUILD_INFO.txt"
echo "Copy the contents of $OUTPUT_DIR to vendor/prebuilt/$OUTPUT_SUBDIR/"

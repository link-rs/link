#!/bin/bash
# Build libquicr for ESP32-S3 (Xtensa) bare-metal target
#
# This script cross-compiles libquicr and its dependencies for ESP32-S3
# using the xtensa-esp-elf toolchain.
#
# Prerequisites:
#   - Espressif toolchain installed (espup or manual install)
#   - CMake 3.13+
#   - Ninja or Make
#
# Usage:
#   ./scripts/build-esp32s3.sh [debug|release]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
LIBQUICR_DIR="$ROOT_DIR/libquicr"
BUILD_TYPE="${1:-Release}"

# Find toolchain
if [ -n "$XTENSA_TOOLCHAIN" ]; then
    TOOLCHAIN_DIR="$XTENSA_TOOLCHAIN"
elif [ -d "$HOME/.espressif/tools/xtensa-esp-elf" ]; then
    # Find the latest version
    TOOLCHAIN_DIR=$(ls -d "$HOME/.espressif/tools/xtensa-esp-elf/esp-"* 2>/dev/null | sort -V | tail -1)/xtensa-esp-elf
else
    echo "Error: Could not find Xtensa toolchain"
    echo "Please install espup (https://github.com/esp-rs/espup) and run:"
    echo "  espup install"
    echo "Or set XTENSA_TOOLCHAIN environment variable"
    exit 1
fi

if [ ! -f "$TOOLCHAIN_DIR/bin/xtensa-esp-elf-gcc" ]; then
    echo "Error: Toolchain not found at $TOOLCHAIN_DIR"
    exit 1
fi

echo "Using toolchain: $TOOLCHAIN_DIR"
echo "Build type: $BUILD_TYPE"

# Set up build directory
BUILD_DIR="$ROOT_DIR/target/esp32s3-build"
INSTALL_DIR="$ROOT_DIR/target/esp32s3"

mkdir -p "$BUILD_DIR"
mkdir -p "$INSTALL_DIR"

# Export toolchain for CMake
export XTENSA_TOOLCHAIN="$TOOLCHAIN_DIR"
export PATH="$TOOLCHAIN_DIR/bin:$PATH"

# Check if we have submodule dependencies initialized
if [ ! -f "$LIBQUICR_DIR/dependencies/picoquic/CMakeLists.txt" ]; then
    echo "Initializing libquicr submodules..."
    (cd "$LIBQUICR_DIR" && git submodule update --init --recursive)
fi

echo "Configuring CMake..."
cd "$BUILD_DIR"

cmake "$LIBQUICR_DIR" \
    -DCMAKE_TOOLCHAIN_FILE="$ROOT_DIR/cmake/toolchain-xtensa-esp32s3.cmake" \
    -DCMAKE_BUILD_TYPE="$BUILD_TYPE" \
    -DCMAKE_INSTALL_PREFIX="$INSTALL_DIR" \
    -DPLATFORM_ESP_HAL=ON \
    -DUSE_MBEDTLS=ON \
    -DQUICR_BUILD_TESTS=OFF \
    -Dquicr_BUILD_BENCHMARKS=OFF \
    -DDRAFT_PARSER_SETUP_VENV=OFF \
    -G "Unix Makefiles"

echo "Building libquicr..."
cmake --build . --parallel $(nproc 2>/dev/null || sysctl -n hw.ncpu)

echo "Installing to $INSTALL_DIR..."
cmake --install .

# Copy additional libraries that aren't auto-installed
echo "Copying additional libraries..."
cp -f "$BUILD_DIR/src/libquicr.a" "$INSTALL_DIR/lib/" 2>/dev/null || true
cp -f "$BUILD_DIR/dependencies/picotls/libpicotls-"*.a "$INSTALL_DIR/lib/" 2>/dev/null || true

# Copy quicr bridge header if c-bridge was built
if [ -f "$BUILD_DIR/c-bridge/CMakeFiles" ]; then
    mkdir -p "$INSTALL_DIR/include/quicr"
    cp -f "$LIBQUICR_DIR/c-bridge/include/quicr/quicr_bridge.h" "$INSTALL_DIR/include/quicr/" 2>/dev/null || true
fi

echo ""
echo "Build complete!"
echo "Libraries installed to: $INSTALL_DIR"
echo ""
echo "To use with quicr-rs, set these environment variables:"
echo "  export QUICR_ESP32S3_LIB=$INSTALL_DIR/lib"
echo "  export QUICR_ESP32S3_INCLUDE=$INSTALL_DIR/include"
echo ""
echo "Then build link/net without the ffi-stub feature:"
echo "  cargo build --target xtensa-esp32s3-none-elf"

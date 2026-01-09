#!/bin/bash
# SPDX-FileCopyrightText: Copyright (c) 2024 Cisco Systems
# SPDX-License-Identifier: BSD-2-Clause
#
# Build QuicR libraries for ESP32-S3 and optionally build link firmware
#
# Usage:
#   ./scripts/build-all.sh              # Build only quicr libraries
#   ./scripts/build-all.sh --with-link  # Build quicr + link firmware
#   ./scripts/build-all.sh --clean      # Clean build everything
#   ./scripts/build-all.sh --help       # Show help

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LINK_DIR="$(cd "$ROOT_DIR/../link" 2>/dev/null && pwd)" || LINK_DIR=""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

usage() {
    cat << EOF
Usage: $0 [OPTIONS]

Build QuicR libraries for ESP32-S3 and optionally build link firmware.

Options:
  --with-link    Also build link/net firmware after building quicr
  --link-only    Only build link/net (assumes quicr libs already built)
  --clean        Clean all build artifacts before building
  --help         Show this help message

Examples:
  $0                    # Build quicr C++ libs only
  $0 --with-link        # Build quicr + link firmware
  $0 --clean --with-link # Clean build everything
  $0 --link-only        # Just rebuild link firmware

Environment:
  XTENSA_TOOLCHAIN      Path to xtensa-esp-elf toolchain (auto-detected)
  LINK_DIR              Path to link firmware (default: ../link)
EOF
    exit 0
}

# Parse arguments
BUILD_LINK=false
LINK_ONLY=false
CLEAN_BUILD=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --with-link)
            BUILD_LINK=true
            shift
            ;;
        --link-only)
            LINK_ONLY=true
            shift
            ;;
        --clean)
            CLEAN_BUILD=true
            shift
            ;;
        --help|-h)
            usage
            ;;
        *)
            error "Unknown option: $1. Use --help for usage."
            ;;
    esac
done

# Check for link directory if building link
if [[ "$BUILD_LINK" == "true" || "$LINK_ONLY" == "true" ]]; then
    if [[ -z "$LINK_DIR" || ! -d "$LINK_DIR/net" ]]; then
        error "Link firmware not found. Expected at: $ROOT_DIR/../link/net"
    fi
fi

cd "$ROOT_DIR"

# Clean if requested
if [[ "$CLEAN_BUILD" == "true" ]]; then
    info "Cleaning build artifacts..."
    rm -rf target/esp32s3-build target/esp32s3
    if [[ "$BUILD_LINK" == "true" || "$LINK_ONLY" == "true" ]]; then
        (cd "$LINK_DIR/net" && cargo clean 2>/dev/null) || true
    fi
fi

# Build quicr C++ libraries (unless link-only)
if [[ "$LINK_ONLY" != "true" ]]; then
    info "Building QuicR C++ libraries for ESP32-S3..."

    # Check prerequisites
    if ! command -v cmake &> /dev/null; then
        error "CMake not found. Install with: brew install cmake"
    fi

    # Run the ESP32-S3 build script
    "$SCRIPT_DIR/build-esp32s3.sh"

    # Verify build output
    if [[ ! -f "$ROOT_DIR/target/esp32s3/lib/libquicr-bridge.a" ]]; then
        error "Build failed: libquicr-bridge.a not found"
    fi

    info "QuicR libraries built successfully!"
    echo ""
    echo "Libraries installed to: $ROOT_DIR/target/esp32s3/lib/"
    ls -lh "$ROOT_DIR/target/esp32s3/lib/"*.a | awk '{print "  " $9 " (" $5 ")"}'
    echo ""
fi

# Build link firmware if requested
if [[ "$BUILD_LINK" == "true" || "$LINK_ONLY" == "true" ]]; then
    info "Building Link firmware with QuicR..."

    cd "$LINK_DIR/net"

    # Check if using ffi-stub (development mode)
    if grep -q 'ffi-stub' Cargo.toml; then
        warn "Link is configured with 'ffi-stub' feature (development mode)"
        warn "To use real quicr libraries, remove 'ffi-stub' from Cargo.toml"
    fi

    # Set environment for quicr build
    export QUICR_ESP32S3_LIB="$ROOT_DIR/target/esp32s3/lib"
    export QUICR_ESP32S3_INCLUDE="$ROOT_DIR/target/esp32s3/include"

    # Build
    cargo build --release --target xtensa-esp32s3-none-elf

    info "Link firmware built successfully!"

    # Show binary size
    BINARY="$LINK_DIR/net/target/xtensa-esp32s3-none-elf/release/net"
    if [[ -f "$BINARY" ]]; then
        SIZE=$(ls -lh "$BINARY" | awk '{print $5}')
        echo ""
        echo "Firmware binary: $BINARY ($SIZE)"
    fi
fi

echo ""
info "Build complete!"

# Show next steps
if [[ "$BUILD_LINK" != "true" && "$LINK_ONLY" != "true" ]]; then
    echo ""
    echo "Next steps:"
    echo "  1. Build link firmware: $0 --link-only"
    echo "  2. Or run: cd ../link/net && cargo build --release --target xtensa-esp32s3-none-elf"
fi

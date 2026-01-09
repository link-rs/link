#!/bin/bash
# Build libquicr for ESP32-S3 using Docker + ESP-IDF
#
# This script builds libquicr and its dependencies in a Docker container
# using the official Espressif IDF v5.4 image, then copies the resulting
# static libraries to vendor/prebuilt/esp32s3/ (or esp32s3-std/ for --std mode)
#
# Usage:
#   ./scripts/docker-build-esp32s3.sh [--std] [--rebuild-image] [--no-cache]
#
# Options:
#   --std             Build for ESP-IDF std (with full C++ support, exceptions)
#                     Output goes to vendor/prebuilt/esp32s3-std/
#   --rebuild-image   Force rebuild the Docker image
#   --no-cache        Build without Docker cache (slower but cleaner)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
DOCKER_DIR="$ROOT_DIR/docker"
OUTPUT_DIR="$ROOT_DIR/vendor/prebuilt"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

# Parse arguments
REBUILD_IMAGE=false
NO_CACHE=""
STD_MODE=false
while [[ $# -gt 0 ]]; do
    case $1 in
        --std)
            STD_MODE=true
            shift
            ;;
        --rebuild-image)
            REBUILD_IMAGE=true
            shift
            ;;
        --no-cache)
            NO_CACHE="--no-cache"
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--std] [--rebuild-image] [--no-cache]"
            echo ""
            echo "Build libquicr for ESP32-S3 using Docker + ESP-IDF"
            echo ""
            echo "Options:"
            echo "  --std             Build for ESP-IDF std mode (C++20, exceptions)"
            echo "  --rebuild-image   Force rebuild the Docker image"
            echo "  --no-cache        Build without Docker cache"
            echo ""
            echo "Output directories:"
            echo "  Default:  vendor/prebuilt/esp32s3/      (bare-metal)"
            echo "  --std:    vendor/prebuilt/esp32s3-std/  (ESP-IDF std)"
            exit 0
            ;;
        *)
            error "Unknown option: $1"
            ;;
    esac
done

# Set output subdirectory based on mode
if [ "$STD_MODE" = true ]; then
    OUTPUT_SUBDIR="esp32s3-std"
    BUILD_MODE="ESP-IDF std"
else
    OUTPUT_SUBDIR="esp32s3"
    BUILD_MODE="bare-metal"
fi

# Check for Docker
if ! command -v docker &> /dev/null; then
    error "Docker not found. Please install Docker first."
fi

# Check if libquicr submodule is initialized
if [ ! -f "$ROOT_DIR/libquicr/CMakeLists.txt" ]; then
    info "Initializing libquicr submodule..."
    (cd "$ROOT_DIR" && git submodule update --init --recursive libquicr)
fi

# Also init libquicr's own submodules
if [ ! -f "$ROOT_DIR/libquicr/dependencies/picoquic/CMakeLists.txt" ]; then
    info "Initializing libquicr dependencies..."
    (cd "$ROOT_DIR/libquicr" && git submodule update --init --recursive)
fi

IMAGE_NAME="quicr-esp32s3-builder"
IMAGE_TAG="latest"

# Check if image exists or needs rebuild
IMAGE_EXISTS=$(docker images -q "$IMAGE_NAME:$IMAGE_TAG" 2>/dev/null)
if [ -z "$IMAGE_EXISTS" ] || [ "$REBUILD_IMAGE" = true ]; then
    info "Building Docker image: $IMAGE_NAME:$IMAGE_TAG"
    info "This may take several minutes on first run..."

    docker build \
        $NO_CACHE \
        -t "$IMAGE_NAME:$IMAGE_TAG" \
        -f "$DOCKER_DIR/Dockerfile.esp32s3" \
        "$ROOT_DIR"

    info "Docker image built successfully"
else
    info "Using existing Docker image: $IMAGE_NAME:$IMAGE_TAG"
    info "Use --rebuild-image to force rebuild"
fi

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Run the build in Docker
info "Building libquicr for ESP32-S3 ($BUILD_MODE mode)..."
info "Output will be written to: $OUTPUT_DIR/$OUTPUT_SUBDIR/"

# Pass the mode to the container via environment variable
docker run --rm \
    -v "$OUTPUT_DIR:/output" \
    -v "$ROOT_DIR/libquicr:/build/libquicr:ro" \
    -e "STD_MODE=$STD_MODE" \
    -e "OUTPUT_SUBDIR=$OUTPUT_SUBDIR" \
    "$IMAGE_NAME:$IMAGE_TAG"

# Verify output
if [ ! -d "$OUTPUT_DIR/$OUTPUT_SUBDIR/lib" ]; then
    error "Build failed: no libraries produced"
fi

LIB_COUNT=$(ls -1 "$OUTPUT_DIR/$OUTPUT_SUBDIR/lib/"*.a 2>/dev/null | wc -l)
if [ "$LIB_COUNT" -eq 0 ]; then
    error "Build failed: no .a files produced"
fi

info "Build complete!"
echo ""
echo "Libraries built ($BUILD_MODE mode):"
ls -lh "$OUTPUT_DIR/$OUTPUT_SUBDIR/lib/"*.a | awk '{print "  " $9 " (" $5 ")"}'
echo ""
if [ "$STD_MODE" = true ]; then
    echo "To use prebuilt libraries (ESP-IDF std), add to Cargo.toml:"
    echo "  quicr = { ..., features = [\"prebuilt-esp32s3-std\"] }"
    echo ""
    echo "To rebuild from source with ESP-IDF in Docker, use:"
    echo "  quicr = { ..., features = [\"espidf-std\"] }"
else
    echo "To use prebuilt libraries (bare-metal), add to Cargo.toml:"
    echo "  quicr = { ..., features = [\"prebuilt-esp32s3\"] }"
    echo ""
    echo "To rebuild from source with ESP-IDF in Docker, use:"
    echo "  quicr = { ..., features = [\"espidf-build\"] }"
fi
